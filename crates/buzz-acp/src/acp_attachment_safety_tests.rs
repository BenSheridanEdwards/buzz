use super::*;
use crate::background_routes::attachment as store;
use serde_json::json;

#[test]
fn attachment_capacity_failure_preserves_prior_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    store::save(dir.path(), "s", &Default::default()).unwrap();
    let before = std::fs::read(store::path(dir.path(), "s")).unwrap();
    let mut state = store::load(dir.path(), "s").unwrap();
    state.cursor = 99;
    state
        .outbound
        .insert("notice:99".into(), "x".repeat(17 * 1024 * 1024));
    assert!(store::save(dir.path(), "s", &state)
        .unwrap_err()
        .to_string()
        .contains("capacity"));
    assert_eq!(std::fs::read(store::path(dir.path(), "s")).unwrap(), before);
    let bad = json!({"method":"session/update","params":{"sessionId":"s","_meta":{"deliveryId":1,"kind":"final","operation":"replace","messageId":"m","turnId":"t","part":0,"parts":129}}});
    assert!(store::accept(dir.path(), &bad).is_err());
    assert_eq!(store::load(dir.path(), "s").unwrap().cursor, 0);
}

#[tokio::test]
async fn mismatched_not_found_receipt_never_readmits() {
    for mode in [
        "foreign-session",
        "foreign-admission",
        "missing-session",
        "missing-admission",
        "matching",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("requests");
        let trigger = nostr::EventBuilder::text_note("run")
            .sign_with_keys(&nostr::Keys::generate())
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
 else:
  result=dict(sessionId=p['sessionId'],admissionId=p['admissionId'],status='not_found')
  mode=sys.argv[2]
  if mode=='foreign-session': result['sessionId']='wrong-session'
  if mode=='foreign-admission': result['admissionId']='wrong-admission'
  if mode=='missing-session': del result['sessionId']
  if mode=='missing-admission': del result['admissionId']
  if mode=='matching':
   if 'original' in globals():
    assert p==original
    result['status']='unknown'
   else: original=p
 print(json.dumps(dict(jsonrpc='2.0',id=r['id'],result=result)),flush=True)
"#;
        let mut client = AcpClient::spawn(
            "python3",
            &[
                "-u".into(),
                "-c".into(),
                script.into(),
                log.display().to_string(),
                mode.into(),
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
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(1),
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains(if mode == "matching" {
                "unresolved"
            } else {
                "identity mismatch"
            }),
            "{mode}: {error}"
        );
        client.shutdown().await;
        let expected = if mode == "matching" {
            "initialize\n_hermes/turn/admit\n_hermes/turn/admit\n"
        } else {
            "initialize\n_hermes/turn/admit\n"
        };
        assert_eq!(
            std::fs::read_to_string(log).unwrap(),
            expected,
            "receipt case {mode}"
        );
    }
}

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

#[test]
fn pending_notice_keeps_original_thread_after_next_trigger() {
    let dir = tempfile::tempdir().unwrap();
    let keys = nostr::Keys::generate();
    let scope = crate::scope::SessionScope::Conversation {
        channel_id: uuid::Uuid::new_v4(),
    };
    let first = nostr::EventBuilder::text_note("first question")
        .sign_with_keys(&keys)
        .unwrap();
    let next = nostr::EventBuilder::text_note("next question")
        .sign_with_keys(&keys)
        .unwrap();
    crate::background_routes::save(dir.path(), "s", &scope, &first).unwrap();
    store::accept(dir.path(), &json!({"method":"session/update","params":{"sessionId":"s","_meta":{"deliveryId":1},"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"old result"}}}})).unwrap();
    // Publication was unavailable, then another foreground trigger replaced
    // the session's current return address before durable retry.
    crate::background_routes::save(dir.path(), "s", &scope, &next).unwrap();
    let event = store::prepare_publication(dir.path(), "s", "notice:1", &keys)
        .unwrap()
        .unwrap();
    assert_eq!(
        crate::queue::parse_thread_tags(&event).root_event_id,
        Some(first.id.to_hex()),
        "pending result retargeted to the next question"
    );
}
