use super::*;
use crate::background_routes::attachment as store;
use serde_json::json;

#[tokio::test]
async fn unknown_admission_after_restart_never_creates_fresh_work() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("requests");
    let keys = nostr::Keys::generate();
    let trigger = nostr::EventBuilder::new(nostr::Kind::Custom(9), "run")
        .sign_with_keys(&keys)
        .unwrap();
    crate::background_routes::save(
        dir.path(),
        "s",
        &crate::scope::SessionScope::Conversation {
            channel_id: uuid::Uuid::new_v4(),
        },
        &trigger,
    )
    .unwrap();
    let script = r#"
import sys,json
for line in sys.stdin:
 r=json.loads(line);m=r['method'];p=r['params']
 with open(sys.argv[1],'a') as f: f.write(m+'\n')
 if m=='initialize': result={'agentCapabilities':{'_meta':{'hermesAttachment':{'version':1,'features':dict.fromkeys(['historyFreeReplay','deliveryReplay','activeTurnSnapshot','terminalReceipts','canonicalAsyncWake','retainedAdmission'],True)}}}}
 elif m in ['_hermes/turn/admit','_hermes/turn/status']: result=dict(sessionId=p['sessionId'],admissionId=p['admissionId'],status='unknown')
 else: raise Exception('unknown admission must not start work')
 print(json.dumps(dict(jsonrpc='2.0',id=r['id'],result=result)),flush=True)
"#;
    for _ in 0..2 {
        let mut client = AcpClient::spawn(
            "python3",
            &[
                "-u".into(),
                "-c".into(),
                script.into(),
                log.display().to_string(),
            ],
            &[],
            false,
        )
        .await
        .unwrap();
        client.set_background_routes_dir(Some(dir.path().into()));
        client.initialize().await.unwrap();
        let error = client
            .session_prompt_with_idle_timeout(
                "s",
                "run",
                std::time::Duration::from_secs(2),
                std::time::Duration::from_secs(3),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unresolved"));
        client.shutdown().await;
    }
    assert_eq!(
        std::fs::read_to_string(log).unwrap(),
        "initialize\n_hermes/turn/admit\ninitialize\n_hermes/turn/status\n"
    );
    assert_eq!(store::load(dir.path(), "s").unwrap().admissions.len(), 1);
}

#[test]
fn malformed_notice_never_advances_durable_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let unsupported = json!({"method":"session/update","params":{"sessionId":"s","_meta":{"deliveryId":1},"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"image","data":"invalid"}}}});
    assert!(store::accept(dir.path(), &unsupported).is_err());
    assert_eq!(store::load(dir.path(), "s").unwrap().cursor, 0);
}

#[test]
fn stale_outgoing_ack_cannot_erase_staged_multipart() {
    let dir = tempfile::tempdir().unwrap();
    let stale = store::load(dir.path(), "s").unwrap();
    let part = json!({"method":"session/update","params":{"sessionId":"s","_meta":{"deliveryId":1,"kind":"final","operation":"replace","messageId":"m","turnId":"t","part":0,"parts":2},"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"first"}}}});
    store::accept(dir.path(), &part).unwrap();
    assert!(store::save(dir.path(), "s", &stale).is_err());
    let saved = store::load(dir.path(), "s").unwrap();
    assert_eq!(saved.cursor, 1);
    assert!(saved.groups.contains_key("m"));
    assert!(saved.finals.is_empty());
}
