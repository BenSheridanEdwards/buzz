use super::*;
#[path = "acp_attachment_edge_tests.rs"]
mod edges;
#[path = "acp_attachment_safety_tests.rs"]
mod safety;
use serde_json::json;

#[test]
fn durable_canonical_scope_selects_original_session_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let keys = nostr::Keys::generate();
    let trigger = nostr::EventBuilder::text_note("work")
        .sign_with_keys(&keys)
        .unwrap();
    let scope = crate::scope::SessionScope::Conversation {
        channel_id: uuid::Uuid::new_v4(),
    };
    crate::background_routes::save(dir.path(), "s", &scope, &trigger).unwrap();
    crate::background_routes::attachment::save(dir.path(), "s", &Default::default()).unwrap();
    assert_eq!(
        crate::background_routes::canonical_scope(dir.path(), &scope)
            .unwrap()
            .unwrap()
            .session_id,
        "s"
    );
}

#[tokio::test]
#[ignore = "requires isolated GatewayRunner from test_rust_consumer.py"]
async fn rust_gateway_consumer() {
    use crate::background_routes::{self, attachment as store};
    let dir = std::path::PathBuf::from(std::env::var("BUZZ_TEST_STATE").unwrap());
    let python = std::env::var("BUZZ_TEST_PYTHON").unwrap();
    let script = "import os,sys; from pathlib import Path; sys.dont_write_bytecode=True; sys.path.insert(0,os.environ['BUZZ_TEST_HERMES_ROOT']); os.environ['HERMES_HOME']=str(Path(os.environ['BUZZ_TEST_ACP_SOCKET']).parent.parent); from acp_adapter.attach import main; main()";
    let keys = nostr::Keys::generate();
    let scope = crate::scope::SessionScope::Conversation {
        channel_id: uuid::Uuid::new_v4(),
    };
    let mut sid = String::new();
    for index in 0..2 {
        let mut client = AcpClient::spawn(
            &python,
            &["-u".into(), "-c".into(), script.into()],
            &[],
            false,
        )
        .await
        .unwrap();
        client.set_background_routes_dir(Some(dir.clone()));
        client.initialize().await.unwrap();
        if index == 0 {
            sid = client
                .session_new("/tmp", vec![], None, None)
                .await
                .unwrap();
            assert!(
                store::path(&dir, &sid).exists(),
                "initial cursor must be durable before admission"
            );
        }
        let trigger = nostr::EventBuilder::text_note(format!("work {index}"))
            .sign_with_keys(&keys)
            .unwrap();
        background_routes::save(&dir, &sid, &scope, &trigger).unwrap();
        for _ in 0..2 {
            let result = client
                .session_prompt_with_idle_timeout(
                    &sid,
                    "hi",
                    std::time::Duration::from_secs(5),
                    std::time::Duration::from_secs(20),
                )
                .await
                .unwrap();
            assert_eq!(result, StopReason::EndTurn);
        }
        let state = store::load(&dir, &sid).unwrap();
        assert_eq!(state.finals.len(), index + 1);
        assert!(state.finals.values().all(|text| text == "hello world"));
        let rest = crate::relay::RestClient {
            http: reqwest::Client::new(),
            base_url: "http://127.0.0.1:1".into(),
            keys: keys.clone(),
            auth_tag_json: None,
        };
        let key = state.outbound.keys().next().unwrap();
        let event = store::prepare_publication(&dir, &sid, key, &rest.keys)
            .unwrap()
            .unwrap();
        assert!(store::Publication {
            dir: &dir,
            sid: &sid,
            key
        }
        .submit(&rest, &event)
        .await
        .is_err());
        let after = store::load(&dir, &sid).unwrap();
        assert!(
            !after.publications.is_empty(),
            "failed network publication must retain signed event"
        );
        assert!(after.published.is_empty());
        client.shutdown().await;
    }
}

#[tokio::test]
async fn retained_admission_retries_by_trigger_without_native_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let keys = nostr::Keys::generate();
    let trigger = nostr::EventBuilder::text_note("work")
        .sign_with_keys(&keys)
        .unwrap();
    let scope = crate::scope::SessionScope::Conversation {
        channel_id: uuid::Uuid::new_v4(),
    };
    crate::background_routes::save(dir.path(), "s", &scope, &trigger).unwrap();
    let script = r#"
import sys,json
for line in sys.stdin:
 r=json.loads(line); m=r['method']; p=r['params']; result={}
 if m=='initialize': result={'agentCapabilities':{'_meta':{'hermesAttachment':{'version':1,'features':dict.fromkeys(['historyFreeReplay','deliveryReplay','activeTurnSnapshot','terminalReceipts','canonicalAsyncWake','retainedAdmission'],True)}}}}
 elif m=='_hermes/turn/admit':
  result=dict(sessionId='s',admissionId=p['admissionId'],turnId='t',status='completed')
 elif m=='_hermes/turn/status': result=dict(sessionId='s',admissionId=p['admissionId'],turnId='t',status='completed')
 elif m=='session/load':
  for frame in [dict(method='session/update',params=dict(sessionId='s',_meta=dict(kind='final',operation='replace',messageId='t:assistant',turnId='t',part=0,parts=1,deliveryId=1),update=dict(sessionUpdate='agent_message_chunk',content=dict(type='text',text='answer')))),dict(method='_hermes/turn_complete',params=dict(sessionId='s',turnId='t',stopReason='end_turn',_meta=dict(deliveryId=2)))]: print(json.dumps(dict(jsonrpc='2.0',**frame)),flush=True)
  result={'_meta':dict(lastDeliveryId=2,hasMoreDeliveries=False,replayComplete=True,historyIncluded=False)}
 else:
  print(json.dumps(dict(jsonrpc='2.0',id=r['id'],error=dict(code=-32000,message='native prompt forbidden'))),flush=True);continue
 print(json.dumps(dict(jsonrpc='2.0',id=r['id'],result=result)),flush=True)
"#;
    for _ in 0..2 {
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
        assert_eq!(
            client.peek_turn_text(),
            "",
            "only durable publisher owns canonical reply"
        );
        client.shutdown().await;
    }
}

#[tokio::test]
async fn negotiated_replay_survives_restart_and_stages_multipart() {
    let dir = tempfile::tempdir().unwrap();
    let keys = nostr::Keys::generate();
    let trigger = nostr::EventBuilder::text_note("work")
        .sign_with_keys(&keys)
        .unwrap();
    let scope = crate::scope::SessionScope::Conversation {
        channel_id: uuid::Uuid::new_v4(),
    };
    crate::background_routes::save(dir.path(), "s", &scope, &trigger).unwrap();
    let init = json!({"agentCapabilities":{"_meta":{"hermesAttachment":{"version":1,"features":{
        "historyFreeReplay":true,"deliveryReplay":true,"activeTurnSnapshot":true,
        "terminalReceipts":true,"canonicalAsyncWake":true,"retainedAdmission":true}}}}});
    let part = |index, text, delivery| {
        json!({"jsonrpc":"2.0","method":"session/update","params":{
        "sessionId":"s","_meta":{"kind":"final","operation":"replace","messageId":"t:assistant","turnId":"t","part":index,"parts":2,"deliveryId":delivery,"replay":true},
        "update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}}})
    };
    let script = format!(
        r#"
        read -t 5 init
        echo '{{"jsonrpc":"2.0","id":0,"result":{init}}}'
        read -t 5 load
        echo '{part0}'
        echo '{{"jsonrpc":"2.0","id":1,"result":{{"_meta":{{"lastDeliveryId":17,"hasMoreDeliveries":true,"replayComplete":false,"historyIncluded":false}}}}}}'
        read -t 5 load
        case "$load" in *'"afterDeliveryId":17'*'"history":false'*) ;; *) exit 7 ;; esac
        echo '{part1}'
        echo '{{"jsonrpc":"2.0","method":"_hermes/turn_complete","params":{{"sessionId":"s","turnId":"t","stopReason":"end_turn","_meta":{{"deliveryId":20}}}}}}'
        echo '{{"jsonrpc":"2.0","id":2,"result":{{"_meta":{{"lastDeliveryId":20,"hasMoreDeliveries":false,"replayComplete":true,"historyIncluded":false}}}}}}'
        sleep 2
    "#,
        part0 = part(0, "Final ", 17),
        part1 = part(1, "界", 19)
    );
    let mut client = AcpClient::spawn("bash", &["-c".into(), script], &[], false)
        .await
        .unwrap();
    client.set_background_routes_dir(Some(dir.path().into()));
    client.initialize().await.unwrap();
    let result = client.session_load("s", "/tmp", vec![]).await;
    assert!(result.is_ok(), "negotiated journal must drain: {result:?}");
    assert_eq!(
        client.peek_turn_text(),
        "",
        "replay is not native prompt output"
    );
    client.shutdown().await;
    let state: serde_json::Value = serde_json::from_slice(
        &std::fs::read(crate::background_routes::attachment::path(dir.path(), "s")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["cursor"], 20);
    assert_eq!(state["finals"]["t"], "Final 界");
    assert_eq!(state["terminals"]["t"]["stopReason"], "end_turn");
    assert_eq!(
        state["outbound"]["turn:t"], "Final 界",
        "terminal must durably enqueue final publication before checkpoint"
    );
    let first =
        crate::background_routes::attachment::prepare_publication(dir.path(), "s", "turn:t", &keys)
            .unwrap()
            .unwrap();
    let retry =
        crate::background_routes::attachment::prepare_publication(dir.path(), "s", "turn:t", &keys)
            .unwrap()
            .unwrap();
    assert_eq!(
        first, retry,
        "ambiguous publication retries the exact signed event"
    );
    assert_eq!(first.content, "Final 界");
    crate::background_routes::attachment::ack_publication(dir.path(), "s", "turn:t").unwrap();
    assert!(crate::background_routes::attachment::prepare_publication(
        dir.path(),
        "s",
        "turn:t",
        &keys
    )
    .unwrap()
    .is_none());
}
