// Canonical (Hermes attachment) recovery: bounded retries, settled routes,
// and never forking a busy canonical scope onto a second connection.

/// A canonical gateway stub: it negotiates the attachment, counts each
/// `session/load`, and either fails it or replays nothing.
fn canonical_stub_script(loads: &std::path::Path, load_fails: bool) -> String {
    format!(
        r#"
import sys,json
fail={fail}
for line in sys.stdin:
 r=json.loads(line);m=r['method']
 if m=='initialize': result={{'agentCapabilities':{{'_meta':{{'hermesAttachment':{{'version':1,'features':dict.fromkeys(['historyFreeReplay','deliveryReplay','activeTurnSnapshot','terminalReceipts','canonicalAsyncWake','retainedAdmission'],True)}}}}}}}}
 elif m=='session/load':
  open({loads:?},'a').write('load\n')
  if fail:
   print(json.dumps(dict(jsonrpc='2.0',id=r['id'],error=dict(code=-32000,message='session missing'))),flush=True);continue
  result={{'_meta':dict(lastDeliveryId=0,hasMoreDeliveries=False,replayComplete=True,historyIncluded=False)}}
 else:
  print(json.dumps(dict(jsonrpc='2.0',id=r['id'],error=dict(code=-32000,message='synthetic work forbidden'))),flush=True);continue
 print(json.dumps(dict(jsonrpc='2.0',id=r['id'],result=result)),flush=True)
"#,
        fail = if load_fails { "True" } else { "False" },
        loads = loads.display().to_string(),
    )
}

/// A freshly spawned canonical worker: it has negotiated the attachment but
/// has never run a user turn, exactly like a worker after restart/respawn.
async fn fresh_canonical_agent(index: usize, load_fails: bool) -> (OwnedAgent, tempfile::TempDir) {
    let log = tempfile::tempdir().unwrap();
    let script = canonical_stub_script(&log.path().join("loads"), load_fails);
    let mut agent = idle_agent_with_session(SessionScope::Conversation {
        channel_id: Uuid::new_v4(),
    })
    .await;
    agent.acp.shutdown().await;
    agent.index = index;
    agent.state = SessionState::default();
    agent.acp = AcpClient::spawn("python3", &["-u".into(), "-c".into(), script], &[], false)
        .await
        .unwrap();
    agent.acp.initialize().await.unwrap();
    assert!(agent.acp.canonical_attachment());
    (agent, log)
}

fn load_count(log: &tempfile::TempDir) -> usize {
    std::fs::read_to_string(log.path().join("loads"))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

fn canonical_route(
    dir: &std::path::Path,
    sid: &str,
    state: crate::background_routes::attachment::State,
) -> SessionScope {
    let scope = SessionScope::Conversation {
        channel_id: Uuid::new_v4(),
    };
    let trigger = nostr::EventBuilder::text_note("original")
        .sign_with_keys(&nostr::Keys::generate())
        .unwrap();
    crate::background_routes::save(dir, sid, &scope, &trigger).unwrap();
    crate::background_routes::attachment::save(dir, sid, &state).unwrap();
    scope
}

#[tokio::test]
async fn canonical_recovery_failure_backs_off_instead_of_reloading_every_tick() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("routes");
    let state = crate::background_routes::attachment::State {
        active_turn: Some("wake".into()),
        ..Default::default()
    };
    let scope = canonical_route(&dir, "origin", state);
    let mut ctx = make_prompt_context_no_owner();
    ctx.background_routes_dir = Some(dir);
    let ctx = Arc::new(ctx);
    let config = crate::error_outcome_emission_tests::test_config();
    let (agent, log) = fresh_canonical_agent(0, true).await;
    let mut pool = AgentPool::from_slots(vec![Some(agent)]);
    let subscribed = HashSet::from([scope.channel_id()]);

    crate::dispatch_delivery_turns(&mut pool, &ctx, &config, &subscribed);
    let result = tokio::time::timeout(Duration::from_secs(5), pool.result_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result.outcome, PromptOutcome::Error(_)));
    assert_eq!(load_count(&log), 1);
    pool.join_set.join_next().await.unwrap().unwrap();
    pool.task_map.clear();
    pool.return_agent(result.agent);

    // The next maintenance tick must honour the backoff, not reload again.
    crate::dispatch_delivery_turns(&mut pool, &ctx, &config, &subscribed);
    assert!(
        pool.join_set.is_empty(),
        "failed canonical recovery must back off before loading again"
    );
    assert_eq!(load_count(&log), 1);
}

/// A settled route (nothing active or unpublished) is still observed after
/// restart, so a gateway wake finishing then is published unattended, but at
/// most once per `SETTLED_POLL_INTERVAL` rather than on every tick.
#[tokio::test]
async fn settled_canonical_route_is_polled_slowly_not_every_tick() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("routes");
    let mut state = crate::background_routes::attachment::State::default();
    state.outbound.insert("turn:t".into(), "Answer.".into());
    state.published.insert("turn:t".into());
    let scope = canonical_route(&dir, "settled", state);
    let mut ctx = make_prompt_context_no_owner();
    ctx.background_routes_dir = Some(dir);
    let ctx = Arc::new(ctx);
    let config = crate::error_outcome_emission_tests::test_config();
    let subscribed = HashSet::from([scope.channel_id()]);
    let (agent, log) = fresh_canonical_agent(0, false).await;
    let mut pool = AgentPool::from_slots(vec![Some(agent)]);

    async fn tick_and_settle(
        pool: &mut AgentPool,
        ctx: &Arc<PromptContext>,
        config: &crate::config::Config,
        subscribed: &HashSet<Uuid>,
    ) {
        crate::dispatch_delivery_turns(pool, ctx, config, subscribed);
        if pool.join_set.is_empty() {
            return;
        }
        let result = tokio::time::timeout(Duration::from_secs(5), pool.result_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(result.outcome, PromptOutcome::Ok(_)));
        pool.join_set.join_next().await.unwrap().unwrap();
        pool.task_map.clear();
        pool.return_agent(result.agent);
    }

    tick_and_settle(&mut pool, &ctx, &config, &subscribed).await;
    assert_eq!(load_count(&log), 1, "settled route must still be observed after restart");
    tick_and_settle(&mut pool, &ctx, &config, &subscribed).await;
    assert_eq!(load_count(&log), 1, "settled route must not be reloaded on the next tick");
    pool.recovery_retries
        .lock()
        .unwrap()
        .age_settled_polls(crate::background_recovery::SETTLED_POLL_INTERVAL);
    tick_and_settle(&mut pool, &ctx, &config, &subscribed).await;
    assert_eq!(load_count(&log), 2, "settled route must be re-polled after the interval");
}

#[tokio::test]
async fn canonical_scope_with_busy_owner_is_held_never_forked() {
    for scope in [
        SessionScope::Conversation {
            channel_id: Uuid::new_v4(),
        },
        SessionScope::Thread {
            channel_id: Uuid::new_v4(),
            root_event_id: "root".into(),
        },
    ] {
        let (idle, _log) = fresh_canonical_agent(1, false).await;
        let mut pool = AgentPool::from_slots(vec![None, Some(idle)]);
        pool.record_scope_owner(scope.clone(), 0);
        pool.task_map.insert(
            tokio::spawn(async {}).id(),
            TaskMeta {
                agent_index: 0,
                channel_id: Some(scope.channel_id()),
                scope: Some(scope.clone()),
                turn_id: "busy".into(),
                recoverable_batch: None,
                control_tx: None,
                steer_tx: None,
                successful_steer_deliveries: HashSet::new(),
            },
        );
        let start = std::time::Instant::now();
        for elapsed in [Duration::ZERO, HOLD_BUSY_OWNER_TIMEOUT * 10] {
            let decision = pool.hold_decision(&scope, start + elapsed, HOLD_BUSY_OWNER_TIMEOUT);
            assert!(
                matches!(decision, HoldDecision::Hold { owner_index: 0, .. }),
                "canonical {} scope must queue behind its busy owner, got {decision:?}",
                scope.telemetry_label()
            );
        }
    }
}

/// Every production spawn path — initial pool, lazy wake, slot refill and
/// crash respawn — must hand a canonical worker its durable routes, or an
/// idle worker cannot load a session (or accept an unsolicited durable frame)
/// until an unrelated user turn happens to run on it.
#[tokio::test]
async fn spawned_and_respawned_canonical_workers_can_load_without_a_user_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("routes");
    canonical_route(
        &dir,
        "origin",
        crate::background_routes::attachment::State::default(),
    );
    let args = vec![
        "-u".to_string(),
        "-c".to_string(),
        canonical_stub_script(&tmp.path().join("loads"), false),
    ];
    let (mut respawned, _, _) =
        crate::spawn_and_init("python3", &args, &[], false, 0, None, Some(dir.clone()))
            .await
            .unwrap();
    respawned.session_load("origin", "/", vec![]).await.unwrap();
    respawned.shutdown().await;

    let startup = crate::PoolStartup {
        agents: 1,
        command: "python3".into(),
        args,
        extra_env: vec![],
        has_generated_codex_config: false,
        model: None,
        effort_level: None,
        observer: None,
        background_routes_dir: Some(dir),
    };
    let mut pool = crate::initialize_agent_pool(&startup, None).await.unwrap();
    let mut agent = pool.try_claim(None).unwrap();
    agent.acp.session_load("origin", "/", vec![]).await.unwrap();
    agent.acp.shutdown().await;
}
