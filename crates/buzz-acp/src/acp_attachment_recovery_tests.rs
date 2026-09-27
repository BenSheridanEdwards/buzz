//! Durable frames that are not errors must advance the cursor; faults stay
//! scoped to their own session; replay fences tolerate live overtakes.
use super::*;
use crate::background_routes::attachment as store;
use serde_json::json;

const INIT: &str = "{'agentCapabilities':{'_meta':{'hermesAttachment':{'version':1,'features':dict.fromkeys(['historyFreeReplay','deliveryReplay','activeTurnSnapshot','terminalReceipts','canonicalAsyncWake','retainedAdmission'],True)}}}}";

/// Spawn a Python fake gateway. `body` handles every non-initialize request
/// with `r`, `m`, `p` bound and must set `result` (or `continue`).
async fn gateway(dir: &std::path::Path, body: &str) -> AcpClient {
    let script = format!(
        "import sys,json\nstate={{}}\ndef emit(f): print(json.dumps(dict(jsonrpc='2.0',**f)),flush=True)\nfor line in sys.stdin:\n r=json.loads(line);m=r['method'];p=r['params']\n if m=='initialize': result={INIT}\n else:\n{body}\n print(json.dumps(dict(jsonrpc='2.0',id=r['id'],result=result)),flush=True)\n"
    );
    let mut client = AcpClient::spawn("python3", &["-u".into(), "-c".into(), script], &[], false)
        .await
        .unwrap();
    client.set_background_routes_dir(Some(dir.into()));
    client.initialize().await.unwrap();
    client
}

fn terminal(sid: &str, turn: &str, reason: &str, id: i64) -> serde_json::Value {
    json!({"method":"_hermes/turn_complete","params":{"sessionId":sid,"turnId":turn,"stopReason":reason,"_meta":{"deliveryId":id}}})
}

#[test]
fn non_error_durable_frames_always_advance_with_a_visible_outcome() {
    // (frame, outbound key, expected text fragment)
    let cases = [
        (
            terminal("s", "t", "max_tokens", 1),
            "turn:t",
            "output limit",
        ),
        (
            terminal("s", "t", "max_turn_requests", 1),
            "turn:t",
            "request limit",
        ),
        (terminal("s", "t", "refusal", 1), "turn:t", "declined"),
        (
            terminal("s", "t", "MAX_TOKENS", 1),
            "turn:t",
            "output limit",
        ),
        (
            terminal("s", "t", "future_reason", 1),
            "turn:t",
            "future_reason",
        ),
        (
            terminal("s", "t", "end_turn", 1),
            "turn:t",
            "without a reply",
        ),
        (
            json!({"method":"_hermes/turn_complete","params":{"sessionId":"s","turnId":"t","_meta":{"deliveryId":1}}}),
            "turn:t",
            "no outcome",
        ),
        (
            json!({"method":"session/update","params":{"sessionId":"s","_meta":{"deliveryId":1},"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"image","data":"x"}}}}),
            "notice:1",
            "cannot display",
        ),
    ];
    for (frame, key, fragment) in cases {
        let dir = tempfile::tempdir().unwrap();
        store::accept(dir.path(), &frame).unwrap_or_else(|e| panic!("{frame}: {e}"));
        let state = store::load(dir.path(), "s").unwrap();
        assert_eq!(state.cursor, 1, "{frame} must advance the cursor");
        let text = state
            .outbound
            .get(key)
            .unwrap_or_else(|| panic!("{frame}: no {key}"));
        assert!(text.contains(fragment), "{frame}: {text}");
        store::accept(dir.path(), &frame).unwrap();
    }
    // A partial final is kept ahead of the stop notice.
    let dir = tempfile::tempdir().unwrap();
    store::accept(dir.path(), &json!({"method":"session/update","params":{"sessionId":"s","_meta":{"deliveryId":1,"kind":"final","operation":"replace","messageId":"m","turnId":"t","part":0,"parts":1},"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"partial"}}}})).unwrap();
    store::accept(dir.path(), &terminal("s", "t", "max_tokens", 2)).unwrap();
    let text = &store::load(dir.path(), "s").unwrap().outbound["turn:t"];
    assert!(
        text.starts_with("partial") && text.contains("output limit"),
        "{text}"
    );
}

#[test]
fn durable_non_message_update_is_skipped_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let tool = json!({"method":"session/update","params":{"sessionId":"s","_meta":{"deliveryId":4},"update":{"sessionUpdate":"tool_call","toolCallId":"c","title":"read"}}});
    store::accept(dir.path(), &tool).unwrap();
    let state = store::load(dir.path(), "s").unwrap();
    assert_eq!(state.cursor, 4);
    assert!(state.outbound.is_empty(), "tool telemetry is not a reply");
}

#[tokio::test]
async fn admitted_turn_with_max_tokens_resolves_to_that_stop_reason() {
    let dir = tempfile::tempdir().unwrap();
    let trigger = nostr::EventBuilder::text_note("work")
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
    let body = "  if m in ['_hermes/turn/admit','_hermes/turn/status']: result=dict(sessionId='s',admissionId=p['admissionId'],turnId='t',status='completed')\n  elif m=='session/load':\n   if p['_meta']['afterDeliveryId']<1: emit(dict(method='_hermes/turn_complete',params=dict(sessionId='s',turnId='t',stopReason='max_tokens',_meta=dict(deliveryId=1))))\n   result={'_meta':dict(lastDeliveryId=1,hasMoreDeliveries=False,replayComplete=True,historyIncluded=False)}\n  else: raise Exception(m)";
    let mut client = gateway(dir.path(), body).await;
    let result = client
        .session_prompt_with_idle_timeout(
            "s",
            "work",
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(10),
        )
        .await;
    assert_eq!(result.unwrap(), StopReason::MaxTokens);
    client.shutdown().await;
}

#[tokio::test]
async fn poison_frame_for_one_session_cannot_abort_another_sessions_request() {
    let dir = tempfile::tempdir().unwrap();
    // Session b's journal holds a structurally invalid final (parts=0).
    let body = "  if m=='session/load':\n   sid=p['sessionId']\n   n=state.get(sid,0);state[sid]=n+1\n   if sid=='a' and n==1: emit(dict(method='session/update',params=dict(sessionId='b',_meta=dict(deliveryId=7,kind='final',operation='replace',messageId='m',turnId='t',part=0,parts=0),update=dict(sessionUpdate='agent_message_chunk',content=dict(type='text',text='x')))))\n   if sid=='b' and n>0: emit(dict(method='session/update',params=dict(sessionId='b',_meta=dict(deliveryId=7,kind='final',operation='replace',messageId='m',turnId='t',part=0,parts=0),update=dict(sessionUpdate='agent_message_chunk',content=dict(type='text',text='x')))))\n   last=7 if (sid=='b' and n>0) else 0\n   result={'_meta':dict(lastDeliveryId=last,hasMoreDeliveries=False,replayComplete=True,historyIncluded=False)}\n  else: raise Exception(m)";
    let mut client = gateway(dir.path(), body).await;
    client.session_load("a", "/tmp", vec![]).await.unwrap();
    client.session_load("b", "/tmp", vec![]).await.unwrap();
    // The live poison frame for b arrives while a's load is in flight.
    client
        .session_load("a", "/tmp", vec![])
        .await
        .expect("another session's poison frame must not abort this request");
    assert_eq!(store::load(dir.path(), "b").unwrap().cursor, 0);
    // b itself still reports its structural violation on replay.
    let error = client.session_load("b", "/tmp", vec![]).await.unwrap_err();
    assert!(error.to_string().contains("part bounds"), "{error}");
    assert_eq!(store::load(dir.path(), "b").unwrap().cursor, 0);
    client.shutdown().await;
}

#[tokio::test]
async fn live_frame_beyond_final_replay_fence_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let body = "  if m=='session/load':\n   emit(dict(method='session/update',params=dict(sessionId='s',_meta=dict(deliveryId=3),update=dict(sessionUpdate='agent_message_chunk',content=dict(type='text',text='replayed')))))\n   emit(dict(method='session/update',params=dict(sessionId='s',_meta=dict(deliveryId=5),update=dict(sessionUpdate='agent_message_chunk',content=dict(type='text',text='live')))))\n   result={'_meta':dict(lastDeliveryId=3,hasMoreDeliveries=False,replayComplete=True,historyIncluded=False)}\n  else: raise Exception(m)";
    let mut client = gateway(dir.path(), body).await;
    client
        .session_load("s", "/tmp", vec![])
        .await
        .expect("a live frame may legitimately precede the load response");
    let state = store::load(dir.path(), "s").unwrap();
    assert_eq!(state.cursor, 5);
    assert_eq!(state.outbound["notice:5"], "live");
    assert_eq!(state.outbound["notice:3"], "replayed");
    client.shutdown().await;
}

#[tokio::test]
async fn live_frames_never_skip_unreplayed_deliveries() {
    let dir = tempfile::tempdir().unwrap();
    // A live frame (9) for s arrives before s was ever loaded on this
    // connection, and again in the middle of a paged replay.
    let body = "  if m=='session/load':\n   sid=p['sessionId'];after=p['_meta']['afterDeliveryId']\n   def notice(i): emit(dict(method='session/update',params=dict(sessionId='s',_meta=dict(deliveryId=i),update=dict(sessionUpdate='agent_message_chunk',content=dict(type='text',text='n%d'%i)))))\n   if sid=='other': notice(9);result={'_meta':dict(lastDeliveryId=0,hasMoreDeliveries=False,replayComplete=True,historyIncluded=False)}\n   elif after<1: notice(1);notice(9);result={'_meta':dict(lastDeliveryId=1,hasMoreDeliveries=True,replayComplete=False,historyIncluded=False)}\n   else:\n    for i in [2,9]:\n     if i>after: notice(i)\n    result={'_meta':dict(lastDeliveryId=9,hasMoreDeliveries=False,replayComplete=True,historyIncluded=False)}\n  else: raise Exception(m)";
    let mut client = gateway(dir.path(), body).await;
    client.session_load("other", "/tmp", vec![]).await.unwrap();
    client.session_load("s", "/tmp", vec![]).await.unwrap();
    let state = store::load(dir.path(), "s").unwrap();
    assert_eq!(state.cursor, 9);
    for id in [1, 2, 9] {
        assert!(
            state.outbound.contains_key(&format!("notice:{id}")),
            "delivery {id} skipped: {:?}",
            state.outbound
        );
    }
    client.shutdown().await;
}
