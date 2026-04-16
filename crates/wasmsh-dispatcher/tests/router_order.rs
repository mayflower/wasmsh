use wasmsh_dispatcher::{DispatchRequest, Dispatcher, RunnerSnapshot};

fn runner(
    id: &str,
    restore_slots: u32,
    inflight_restores: u32,
    restore_queue_depth: u32,
    restore_p95_ms: f64,
    active_sessions: u32,
) -> RunnerSnapshot {
    RunnerSnapshot {
        runner_id: id.into(),
        restore_slots,
        inflight_restores,
        restore_queue_depth,
        restore_p95_ms,
        active_sessions,
        draining: false,
        healthy: true,
    }
}

#[test]
fn router_prefers_more_free_restore_slots_before_anything_else() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.upsert_runner(runner("runner-a", 4, 3, 0, 15.0, 0));
    dispatcher.upsert_runner(runner("runner-b", 4, 1, 9, 90.0, 10));

    let decision = dispatcher
        .dispatch(&DispatchRequest {
            session_id: "session-a".into(),
        })
        .unwrap();

    assert_eq!(decision.runner_id.as_str(), "runner-b");
    assert!(!decision.affinity_reused);
}

#[test]
fn router_breaks_ties_by_queue_then_p95_then_active_sessions() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.upsert_runner(runner("runner-a", 4, 2, 4, 90.0, 8));
    dispatcher.upsert_runner(runner("runner-b", 4, 2, 1, 95.0, 9));
    dispatcher.upsert_runner(runner("runner-c", 4, 2, 1, 40.0, 7));

    let decision = dispatcher
        .dispatch(&DispatchRequest {
            session_id: "session-b".into(),
        })
        .unwrap();

    assert_eq!(decision.runner_id.as_str(), "runner-c");
}
