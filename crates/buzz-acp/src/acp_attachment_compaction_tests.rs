//! Acknowledged publications compact; consumer state stays bounded forever.
use super::*;
use crate::background_routes::attachment as store;
use serde_json::json;

fn route(dir: &std::path::Path) -> nostr::Keys {
    let keys = nostr::Keys::generate();
    let trigger = nostr::EventBuilder::text_note("work")
        .sign_with_keys(&keys)
        .unwrap();
    crate::background_routes::save(
        dir,
        "s",
        &crate::scope::SessionScope::Conversation {
            channel_id: uuid::Uuid::new_v4(),
        },
        &trigger,
    )
    .unwrap();
    keys
}

fn publish_all(dir: &std::path::Path, keys: &nostr::Keys) {
    let state = store::load(dir, "s").unwrap();
    let pending: Vec<String> = state
        .outbound
        .into_keys()
        .filter(|key| !state.published.contains(key))
        .collect();
    for key in pending {
        if store::prepare_publication(dir, "s", &key, keys)
            .unwrap()
            .is_some()
        {
            store::ack_publication(dir, "s", &key).unwrap();
        }
    }
}

#[test]
fn acked_publications_compact_to_bounded_tombstones() {
    let dir = tempfile::tempdir().unwrap();
    let keys = route(dir.path());
    for i in 0..300i64 {
        let turn = format!("t{i}");
        store::accept(dir.path(), &json!({"method":"session/update","params":{"sessionId":"s","_meta":{"deliveryId":3*i+1,"kind":"final","operation":"replace","messageId":format!("{turn}:m"),"turnId":turn,"part":0,"parts":1},"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"answer"}}}})).unwrap();
        store::accept(dir.path(), &json!({"method":"_hermes/turn_complete","params":{"sessionId":"s","turnId":turn,"stopReason":"end_turn","_meta":{"deliveryId":3*i+2}}})).unwrap();
        store::accept(dir.path(), &json!({"method":"session/update","params":{"sessionId":"s","_meta":{"deliveryId":3*i+3},"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"notice"}}}})).unwrap();
        publish_all(dir.path(), &keys);
    }
    let state = store::load(dir.path(), "s").unwrap();
    assert!(state.outbound.is_empty() && state.outbound_routes.is_empty());
    assert!(state.publications.is_empty() && state.finals.is_empty());
    assert!(state.published.len() <= 256, "{}", state.published.len());
    assert!(state.terminals.len() <= 256, "{}", state.terminals.len());
    assert!(
        state.terminals.contains_key("t299"),
        "recent outcomes stay observable"
    );
    // Neither a replayed old frame nor a duplicate recent terminal republishes.
    store::accept(dir.path(), &json!({"method":"_hermes/turn_complete","params":{"sessionId":"s","turnId":"t0","stopReason":"end_turn","_meta":{"deliveryId":2}}})).unwrap();
    store::accept(dir.path(), &json!({"method":"_hermes/turn_complete","params":{"sessionId":"s","turnId":"t299","stopReason":"end_turn","_meta":{"deliveryId":5000}}})).unwrap();
    assert!(store::load(dir.path(), "s").unwrap().outbound.is_empty());
}

#[tokio::test]
async fn admission_is_released_once_its_turn_is_published() {
    let dir = tempfile::tempdir().unwrap();
    let keys = route(dir.path());
    let script = r#"
import sys,json
for line in sys.stdin:
 r=json.loads(line); m=r['method']; p=r['params']
 if m=='initialize': result={'agentCapabilities':{'_meta':{'hermesAttachment':{'version':1,'features':dict.fromkeys(['historyFreeReplay','deliveryReplay','activeTurnSnapshot','terminalReceipts','canonicalAsyncWake','retainedAdmission'],True)}}}}
 elif m in ['_hermes/turn/admit','_hermes/turn/status']: result=dict(sessionId='s',admissionId=p['admissionId'],turnId='t',status='completed')
 elif m=='session/load':
  if p['_meta']['afterDeliveryId']<1: print(json.dumps(dict(jsonrpc='2.0',method='_hermes/turn_complete',params=dict(sessionId='s',turnId='t',stopReason='end_turn',_meta=dict(deliveryId=1)))),flush=True)
  result={'_meta':dict(lastDeliveryId=1,hasMoreDeliveries=False,replayComplete=True,historyIncluded=False)}
 print(json.dumps(dict(jsonrpc='2.0',id=r['id'],result=result)),flush=True)
"#;
    let mut client = AcpClient::spawn(
        "python3",
        &["-u".into(), "-c".into(), script.into()],
        &[],
        false,
    )
    .await
    .unwrap();
    client.set_background_routes_dir(Some(dir.path().into()));
    client.initialize().await.unwrap();
    let result = client
        .session_prompt_with_idle_timeout(
            "s",
            "work",
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(10),
        )
        .await;
    assert_eq!(result.unwrap(), StopReason::EndTurn);
    client.shutdown().await;
    assert_eq!(store::load(dir.path(), "s").unwrap().admissions.len(), 1);
    publish_all(dir.path(), &keys);
    let state = store::load(dir.path(), "s").unwrap();
    assert!(state.admissions.is_empty(), "{:?}", state.admissions);
    assert!(state.outbound.is_empty());
}
