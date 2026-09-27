#[tokio::test]
async fn canonical_signed_media_and_notice_outbox_survive_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let wav = tmp.path().join("reply.txt");
    std::fs::write(&wav, b"canonical artifact").unwrap();
    let keys = nostr::Keys::generate();
    let (base, messages) = media_relay_server(vec![], "unused".into(), None).await;
    let mut ctx = make_prompt_context_no_owner();
    ctx.agent_keys = keys.clone();
    ctx.rest_client.keys = keys.clone();
    ctx.rest_client.base_url = base;
    ctx.cwd = tmp.path().display().to_string();
    ctx.background_routes_dir = Some(tmp.path().join("routes"));
    let dir = ctx.background_routes_dir.as_ref().unwrap();
    let trigger = nostr::EventBuilder::new(nostr::Kind::Custom(9), "question")
        .sign_with_keys(&keys)
        .unwrap();
    crate::background_routes::save(
        dir,
        "s",
        &SessionScope::Conversation {
            channel_id: uuid::Uuid::new_v4(),
        },
        &trigger,
    )
    .unwrap();
    let mut state = crate::background_routes::attachment::State::default();
    state
        .outbound
        .insert("turn:t".into(), format!("Answer.\nMEDIA:{}", wav.display()));
    state
        .outbound
        .insert("notice:2".into(), "Background notice".into());
    crate::background_routes::attachment::save(dir, "s", &state).unwrap();
    publish_canonical_outbox(&ctx, "s").await.unwrap();
    let state = crate::background_routes::attachment::load(dir, "s").unwrap();
    assert!(state.published.contains("turn:t"));
    assert!(state.published.contains("notice:2"));
    assert!(state.outbound.is_empty() && state.publications.is_empty(), "acked records compact");
    let before = kind9_posts(&messages.lock().unwrap()).len();
    assert_eq!(before, 3);
    for event in kind9_posts(&messages.lock().unwrap()).iter() {
        let event: nostr::Event = serde_json::from_value(event.clone()).unwrap();
        event.verify().unwrap();
        assert_eq!(event.pubkey, keys.public_key());
        assert!(!event.content.contains("MEDIA:"), "publication must not expose harness directives or local paths: {}", event.content);
    }
    // Simulate a crash after remote acceptance but before durable local ACK:
    // the record and both signed events retained, neither acknowledged.
    let mut unacked = state;
    unacked.published.remove("turn:t");
    unacked
        .outbound
        .insert("turn:t".into(), format!("Answer.\nMEDIA:{}", wav.display()));
    for event in kind9_posts(&messages.lock().unwrap()) {
        let event: nostr::Event = serde_json::from_value(event).unwrap();
        let key = match event.content.as_str() {
            "Background notice" => continue,
            "Answer." => "turn:t",
            _ => "media:turn:t",
        };
        unacked.publications.insert(key.into(), event);
    }
    crate::background_routes::attachment::save(dir, "s", &unacked).unwrap();
    publish_canonical_outbox(&ctx, "s").await.unwrap();
    let sent = kind9_posts(&messages.lock().unwrap());
    let ids: std::collections::HashSet<_> =
        sent.iter().map(|v| v["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids.len(),
        3,
        "retry must reuse persisted signed event identities"
    );
    assert_eq!(sent.len(), 5);
}

#[tokio::test]
async fn canonical_media_refusal_retries_then_posts_text_without_blocking_others() {
    let tmp = tempfile::tempdir().unwrap();
    let good = tmp.path().join("good.txt");
    std::fs::write(&good, b"valid artifact").unwrap();
    let (base, messages) = media_relay_server(vec![], "unused".into(), None).await;
    let mut ctx = make_prompt_context_no_owner();
    ctx.rest_client.base_url = base;
    ctx.cwd = tmp.path().display().to_string();
    ctx.background_routes_dir = Some(tmp.path().join("routes"));
    let keys = nostr::Keys::generate();
    let trigger = nostr::EventBuilder::text_note("work")
        .sign_with_keys(&keys)
        .unwrap();
    let scope = SessionScope::Conversation {
        channel_id: Uuid::new_v4(),
    };
    let dir = ctx.background_routes_dir.as_ref().unwrap();
    crate::background_routes::save(dir, "s", &scope, &trigger).unwrap();
    let mut state = crate::background_routes::attachment::State::default();
    state
        .outbound
        .insert("turn:t".into(), format!("Here it is.\nMEDIA:{}\nMEDIA:/outside/absent.png", good.display()));
    crate::background_routes::attachment::save(dir, "s", &state).unwrap();
    crate::background_routes::attachment::accept(dir, &serde_json::json!({"method":"session/update","params":{"sessionId":"s","_meta":{"deliveryId":3},"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"later reply"}}}})).unwrap();
    for _ in 1..attachment_publish::MAX_MEDIA_ATTEMPTS {
        assert!(publish_canonical_outbox(&ctx, "s").await.is_err(), "partial upload must remain pending on retry");
    }
    let contents = |m: &[SeenRequest]| -> Vec<String> {
        kind9_posts(m).iter().map(|e| e["content"].as_str().unwrap().to_owned()).collect()
    };
    assert_eq!(contents(&messages.lock().unwrap()), ["later reply"], "a retrying record must not block later replies or commit a partial media set");
    // The final attempt gives up on the media and still delivers the reply.
    publish_canonical_outbox(&ctx, "s").await.unwrap();
    let posted = contents(&messages.lock().unwrap());
    assert_eq!(posted.len(), 2, "{posted:?}");
    assert!(posted[1].starts_with("Here it is.") && posted[1].contains("Could not attach"), "{}", posted[1]);
    assert!(!posted[1].contains("MEDIA:"));
    assert!(crate::background_routes::attachment::load(dir, "s").unwrap().outbound.is_empty());
}

#[tokio::test]
async fn canonical_unbound_record_is_dropped_and_replies_publish_in_delivery_order() {
    let tmp = tempfile::tempdir().unwrap();
    let keys = nostr::Keys::generate();
    let (base, messages) = media_relay_server(vec![], "unused".into(), None).await;
    let mut ctx = make_prompt_context_no_owner();
    ctx.rest_client.keys = keys.clone();
    ctx.rest_client.base_url = base;
    ctx.background_routes_dir = Some(tmp.path().join("routes"));
    let dir = ctx.background_routes_dir.as_ref().unwrap();
    // Committed before any return address existed: it can never bind.
    let mut orphan = crate::background_routes::attachment::State::default();
    orphan.outbound.insert("notice:1".into(), "orphan".into());
    crate::background_routes::attachment::save(dir, "s", &orphan).unwrap();
    let trigger = nostr::EventBuilder::new(nostr::Kind::Custom(9), "question")
        .sign_with_keys(&keys)
        .unwrap();
    crate::background_routes::save(dir, "s", &SessionScope::Conversation { channel_id: Uuid::new_v4() }, &trigger).unwrap();
    for id in [9, 10] {
        crate::background_routes::attachment::accept(dir, &serde_json::json!({"method":"session/update","params":{"sessionId":"s","_meta":{"deliveryId":id},"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":format!("n{id}")}}}})).unwrap();
    }
    let error = publish_canonical_outbox(&ctx, "s").await.unwrap_err();
    assert!(error.to_string().contains("unbound"), "{error}");
    let posted: Vec<String> = kind9_posts(&messages.lock().unwrap())
        .iter()
        .map(|e| e["content"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(posted, ["n9", "n10"]);
    publish_canonical_outbox(&ctx, "s").await.unwrap();
    assert!(crate::background_routes::attachment::load(dir, "s").unwrap().outbound.is_empty());
}

#[tokio::test]
async fn canonical_background_recovery_observes_without_synthetic_work() {
    let tmp = tempfile::tempdir().unwrap();
    let scope = SessionScope::Conversation {
        channel_id: Uuid::new_v4(),
    };
    let trigger = nostr::EventBuilder::text_note("original")
        .sign_with_keys(&nostr::Keys::generate())
        .unwrap();
    let mut ctx = make_prompt_context_no_owner();
    let (resolver, _, server) = counting_resolver(serde_json::json!({"accepted":true})).await;
    ctx.rest_client = resolver.rest_client.clone();
    ctx.background_routes_dir = Some(tmp.path().join("routes"));
    crate::background_routes::save(
        ctx.background_routes_dir.as_ref().unwrap(),
        "origin",
        &scope,
        &trigger,
    )
    .unwrap();
    let mut config = crate::error_outcome_emission_tests::test_config();
    config.persona_env_vars.push((
        crate::config::HERMES_HOME_ENV.into(),
        tmp.path().to_string_lossy().into_owned(),
    ));
    std::fs::write(
        tmp.path().join(crate::BACKGROUND_WORK_MARKER),
        r#"{"pending":[]}"#,
    )
    .unwrap();
    let script = r#"
import sys,json
for line in sys.stdin:
 r=json.loads(line);m=r['method']
 if m=='initialize': result={'agentCapabilities':{'_meta':{'hermesAttachment':{'version':1,'features':dict.fromkeys(['historyFreeReplay','deliveryReplay','activeTurnSnapshot','terminalReceipts','canonicalAsyncWake','retainedAdmission'],True)}}}}
 elif m=='session/load':
  print(json.dumps(dict(jsonrpc='2.0',method='session/update',params=dict(sessionId='origin',_meta=dict(deliveryId=1),update=dict(sessionUpdate='agent_message_chunk',content=dict(type='text',text='durable wake notice'))))),flush=True)
  result={'_meta':dict(lastDeliveryId=1,hasMoreDeliveries=False,replayComplete=True,historyIncluded=False,activeTurn=dict(turnId='wake',status='in_progress',snapshotTruncated=False))}
 else:
  print(json.dumps(dict(jsonrpc='2.0',id=r['id'],error=dict(code=-32000,message='synthetic work forbidden'))),flush=True);continue
 print(json.dumps(dict(jsonrpc='2.0',id=r['id'],result=result)),flush=True)
"#;
    let mut agent = idle_agent_with_session(scope.clone()).await;
    agent.acp.shutdown().await;
    agent.acp = AcpClient::spawn(
        "python3",
        &["-u".into(), "-c".into(), script.into()],
        &[],
        false,
    )
    .await
    .unwrap();
    // A freshly (re)spawned worker has never run a user turn, so nothing but
    // the production recovery path may give it the durable routes directory.
    agent.acp.initialize().await.unwrap();
    crate::background_routes::attachment::save(
        ctx.background_routes_dir.as_ref().unwrap(),
        "origin",
        &crate::background_routes::attachment::State {
            active_turn: Some("wake".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let observer = crate::observer::ObserverHandle::in_process();
    agent.acp.set_observer(Some(observer.clone()), 0);
    agent.acp.set_observer_context(crate::observer::context_for(Some(uuid::Uuid::new_v4()), Some("foreign".into()), None));
    let mut pool = AgentPool::from_slots(vec![Some(agent)]);
    let ctx = Arc::new(ctx);
    crate::dispatch_delivery_turns(
        &mut pool,
        &ctx,
        &config,
        &HashSet::from([scope.channel_id()]),
    );
    let result = tokio::time::timeout(Duration::from_secs(3), pool.result_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(result.outcome, PromptOutcome::Ok(_)),
        "observation must not dispatch DELIVERY_PROMPT"
    );
    let mut agent = result.agent;
    let restored = create_session_and_apply_model(
        &mut agent,
        &ctx,
        None,
        NewSessionChannelContext {
            huddle_instructions: None,
            canvas: None,
            name: None,
            scope: Some(&scope),
            channel_type: None,
        },
    )
    .await;
    assert_eq!(
        restored.unwrap(),
        "origin",
        "restart must reuse durable scope, not create duplicate session"
    );
    let state =
        crate::background_routes::attachment::load(&tmp.path().join("routes"), "origin").unwrap();
    let replay = observer.snapshot().into_iter().find(|event| event.payload["method"] == "session/load").unwrap();
    assert_eq!(replay.channel_id, Some(scope.channel_id().to_string()), "canonical recovery inherited foreign channel");
    assert_eq!(replay.session_id.as_deref(), Some("origin"));
    assert!(
        state.published.contains("notice:1"),
        "production background recovery must publish accepted durable rows"
    );
    server.abort();
}
