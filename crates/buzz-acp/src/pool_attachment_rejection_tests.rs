/// Relay stand-in: rejects (HTTP 400) any event whose content is `poison`,
/// accepts everything else, and records accepted contents in order.
async fn rejecting_relay() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let accepted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = accepted.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut raw = Vec::new();
            let mut buf = vec![0; 16384];
            loop {
                let n = socket.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw);
                if let Some(split) = text.find("\r\n\r\n") {
                    let len = text[..split]
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                        .unwrap_or(0);
                    if raw.len() >= split + 4 + len {
                        break;
                    }
                }
            }
            let text = String::from_utf8_lossy(&raw).to_string();
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
            let content = serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|v| v["content"].as_str().map(str::to_owned))
                .unwrap_or_default();
            let (status, reply) = if content == "poison" {
                ("400 Bad Request", r#"{"error":"invalid: reply parent not found"}"#.to_string())
            } else {
                seen.lock().unwrap().push(content);
                ("200 OK", r#"{"accepted":true}"#.to_string())
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (base, accepted)
}

// A record the relay permanently rejects (e.g. a reply to an ephemeral
// trigger that was never stored) must be dropped, not retried forever, and
// must not keep the session's recovery failing.
#[tokio::test]
async fn canonical_record_rejected_by_relay_is_dropped_not_retried_forever() {
    let tmp = tempfile::tempdir().unwrap();
    let keys = nostr::Keys::generate();
    let (base, accepted) = rejecting_relay().await;
    let mut ctx = make_prompt_context_no_owner();
    ctx.rest_client.keys = keys.clone();
    ctx.rest_client.base_url = base;
    ctx.background_routes_dir = Some(tmp.path().join("routes"));
    let dir = ctx.background_routes_dir.as_ref().unwrap();
    let trigger = nostr::EventBuilder::new(nostr::Kind::Custom(9), "question")
        .sign_with_keys(&keys)
        .unwrap();
    crate::background_routes::save(dir, "s", &SessionScope::Conversation { channel_id: Uuid::new_v4() }, &trigger).unwrap();
    for (id, text) in [(1, "before"), (2, "poison"), (3, "after")] {
        crate::background_routes::attachment::accept(dir, &serde_json::json!({"method":"session/update","params":{"sessionId":"s","_meta":{"deliveryId":id},"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}}})).unwrap();
    }
    let error = publish_canonical_outbox(&ctx, "s").await.unwrap_err();
    assert!(error.to_string().contains("rejected"), "{error}");
    assert_eq!(*accepted.lock().unwrap(), ["before", "after"]);
    // The rejected record is gone: the next pass succeeds without resubmitting it.
    publish_canonical_outbox(&ctx, "s").await.unwrap();
    assert!(crate::background_routes::attachment::load(dir, "s").unwrap().outbound.is_empty());
    assert_eq!(*accepted.lock().unwrap(), ["before", "after"]);
}

// A publication failure during canonical recovery is an outbox problem, not a
// broken agent: it must not be reported as a transport error (which respawns
// the worker and, once the circuit opens, exits the whole harness).
#[tokio::test]
async fn canonical_recovery_publication_failure_keeps_the_agent_alive() {
    let tmp = tempfile::tempdir().unwrap();
    let scope = SessionScope::Conversation { channel_id: Uuid::new_v4() };
    let keys = nostr::Keys::generate();
    let trigger = nostr::EventBuilder::text_note("original").sign_with_keys(&keys).unwrap();
    let (base, _accepted) = rejecting_relay().await;
    let mut ctx = make_prompt_context_no_owner();
    ctx.rest_client.keys = keys;
    ctx.rest_client.base_url = base;
    ctx.background_routes_dir = Some(tmp.path().join("routes"));
    crate::background_routes::save(ctx.background_routes_dir.as_ref().unwrap(), "origin", &scope, &trigger).unwrap();
    let mut config = crate::error_outcome_emission_tests::test_config();
    config.persona_env_vars.push((crate::config::HERMES_HOME_ENV.into(), tmp.path().to_string_lossy().into_owned()));
    std::fs::write(tmp.path().join(crate::BACKGROUND_WORK_MARKER), r#"{"pending":[]}"#).unwrap();
    let script = r#"
import sys,json
for line in sys.stdin:
 r=json.loads(line);m=r['method']
 if m=='initialize': result={'agentCapabilities':{'_meta':{'hermesAttachment':{'version':1,'features':dict.fromkeys(['historyFreeReplay','deliveryReplay','activeTurnSnapshot','terminalReceipts','canonicalAsyncWake','retainedAdmission'],True)}}}}
 elif m=='session/load':
  print(json.dumps(dict(jsonrpc='2.0',method='session/update',params=dict(sessionId='origin',_meta=dict(deliveryId=1),update=dict(sessionUpdate='agent_message_chunk',content=dict(type='text',text='poison'))))),flush=True)
  result={'_meta':dict(lastDeliveryId=1,hasMoreDeliveries=False,replayComplete=True,historyIncluded=False,activeTurn=None)}
 else:
  print(json.dumps(dict(jsonrpc='2.0',id=r['id'],error=dict(code=-32000,message='unexpected'))),flush=True);continue
 print(json.dumps(dict(jsonrpc='2.0',id=r['id'],result=result)),flush=True)
"#;
    let mut agent = idle_agent_with_session(scope.clone()).await;
    agent.acp.shutdown().await;
    agent.acp = AcpClient::spawn("python3", &["-u".into(), "-c".into(), script.into()], &[], false)
        .await
        .unwrap();
    agent.acp.initialize().await.unwrap();
    crate::background_routes::attachment::save(
        ctx.background_routes_dir.as_ref().unwrap(),
        "origin",
        &crate::background_routes::attachment::State { active_turn: Some("wake".into()), ..Default::default() },
    )
    .unwrap();
    let mut pool = AgentPool::from_slots(vec![Some(agent)]);
    let ctx = Arc::new(ctx);
    crate::dispatch_delivery_turns(&mut pool, &ctx, &config, &HashSet::from([scope.channel_id()]));
    let result = tokio::time::timeout(Duration::from_secs(5), pool.result_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(result.outcome, PromptOutcome::Ok(_)),
        "publication failure must not be a transport error: {:?}",
        match &result.outcome { PromptOutcome::Error(e) => e.to_string(), _ => String::new() }
    );
}
