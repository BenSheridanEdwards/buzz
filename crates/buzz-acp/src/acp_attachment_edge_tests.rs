use super::*;
use crate::background_routes::attachment as store;
use serde_json::json;

#[tokio::test]
async fn canonical_cancel_is_correlated_request_and_waits_for_own_receipt() {
    let script = r#"
import sys,json
cancelled=False
for line in sys.stdin:
 r=json.loads(line);m=r['method'];p=r['params']
 if m=='initialize': result={'agentCapabilities':{'_meta':{'hermesAttachment':{'version':1,'features':dict.fromkeys(['historyFreeReplay','deliveryReplay','activeTurnSnapshot','terminalReceipts','canonicalAsyncWake','retainedAdmission'],True)}}}}
 elif m=='session/cancel':
  assert p['turnId']=='owned' and r.get('id') is not None
  cancelled=True;result={}
 elif m=='session/load':
  if cancelled:
   for i,t in enumerate(['other','owned']): print(json.dumps(dict(jsonrpc='2.0',method='_hermes/turn_complete',params=dict(sessionId='s',turnId=t,stopReason='cancelled',_meta=dict(deliveryId=i+1)))),flush=True)
  result={'_meta':dict(lastDeliveryId=2 if cancelled else 0,hasMoreDeliveries=False,replayComplete=True,historyIncluded=False)}
  if not cancelled: result['_meta']['activeTurn']=dict(turnId='owned',status='in_progress',snapshotTruncated=False)
 else: raise Exception('unexpected request')
 print(json.dumps(dict(jsonrpc='2.0',id=r['id'],result=result)),flush=True)
"#;
    let dir = tempfile::tempdir().unwrap();
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
    client.session_load("s", "/tmp", vec![]).await.unwrap();
    assert_eq!(
        client
            .cancel_with_cleanup_grace("s", std::time::Duration::from_secs(3))
            .await
            .unwrap(),
        StopReason::Cancelled
    );
    assert!(store::load(dir.path(), "s")
        .unwrap()
        .terminals
        .contains_key("owned"));
    client.shutdown().await;
}

#[test]
fn stale_ack_snapshot_cannot_erase_new_delivery_or_multipart() {
    let dir = tempfile::tempdir().unwrap();
    store::save(dir.path(), "s", &Default::default()).unwrap();
    let mut stale = store::load(dir.path(), "s").unwrap();
    let notice = json!({"method":"session/update","params":{"sessionId":"s","_meta":{"deliveryId":1},"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"durable notice"}}}});
    store::accept(dir.path(), &notice).unwrap();
    stale.published.insert("old-event".into());
    assert!(
        store::save(dir.path(), "s", &stale).is_err(),
        "stale outgoing ack must not overwrite journal consumption"
    );
    let state = store::load(dir.path(), "s").unwrap();
    assert_eq!(state.cursor, 1);
    assert_eq!(state.outbound["notice:1"], "durable notice");
}

#[tokio::test]
async fn hermes_generated_multipart_fixture_crosses_production_ndjson() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../test-fixtures/hermes-attachment/canonical-v1.json"
    ))
    .unwrap();
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../test-fixtures/hermes-attachment/canonical-v1.json");
    let script = r#"
import sys,json
f=json.load(open(sys.argv[1]))
for line in sys.stdin:
 r=json.loads(line)
 if r['method']=='initialize': result=f['initialize']
 elif r['method']=='session/load':
  assert r['params']['_meta']['history'] is False
  for frame in f['frames']:
   if frame['params']['_meta']['deliveryId']>r['params']['_meta']['afterDeliveryId']: print(json.dumps(frame),flush=True)
  result=f['load']
 else: raise Exception('unexpected method')
 print(json.dumps(dict(jsonrpc='2.0',id=r['id'],result=result)),flush=True)
"#;
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let mut client = AcpClient::spawn(
            "python3",
            &[
                "-u".into(),
                "-c".into(),
                script.into(),
                fixture_path.display().to_string(),
            ],
            &[],
            false,
        )
        .await
        .unwrap();
        client.set_background_routes_dir(Some(dir.path().into()));
        client.initialize().await.unwrap();
        client
            .session_load("fixture-session", "/tmp", vec![])
            .await
            .unwrap();
        client.shutdown().await;
    }
    let state = store::load(dir.path(), "fixture-session").unwrap();
    assert_eq!(
        state.outbound["turn:fixture-turn"],
        format!("Final {}", "界".repeat(30000))
    );
    assert!(state.groups.is_empty());
    assert_eq!(
        state.cursor,
        fixture["load"]["_meta"]["lastDeliveryId"].as_i64().unwrap()
    );
}

#[tokio::test]
async fn explicit_approval_controls_keep_native_response_ownership() {
    let script = r#"
import sys,json
for line in sys.stdin:
 r=json.loads(line);m=r['method']
 if m=='initialize': result={'agentCapabilities':{'_meta':{'hermesAttachment':{'version':1,'features':dict.fromkeys(['historyFreeReplay','deliveryReplay','activeTurnSnapshot','terminalReceipts','canonicalAsyncWake','retainedAdmission'],True)}}}}
 elif m=='session/prompt':
  assert len(r['params']['prompt'])==1 and r['params']['prompt'][0]['text'] in ['/approve','/deny']
  result={'stopReason':'end_turn'}
 else: raise Exception('control must not be admitted')
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
    client.initialize().await.unwrap();
    for control in ["/approve", "/deny"] {
        assert_eq!(
            client
                .session_prompt_blocks_with_idle_timeout(
                    "s",
                    &[control, "[Context] prior conversation"],
                    std::time::Duration::from_secs(2),
                    std::time::Duration::from_secs(3)
                )
                .await
                .unwrap(),
            StopReason::EndTurn
        );
    }
    client.shutdown().await;
}
