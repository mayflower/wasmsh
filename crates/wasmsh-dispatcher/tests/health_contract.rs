use wasmsh_dispatcher::{DispatchError, DispatchRequest, Dispatcher, RunnerSnapshot};

fn runner(id: &str, restore_slots: u32, healthy: bool) -> RunnerSnapshot {
    RunnerSnapshot {
        runner_id: id.into(),
        restore_slots,
        inflight_restores: 0,
        restore_queue_depth: 0,
        restore_p95_ms: 10.0,
        active_sessions: 0,
        draining: false,
        healthy,
    }
}

#[test]
fn unhealthy_runners_are_not_selected() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.upsert_runner(runner("runner-a", 2, false));
    dispatcher.upsert_runner(runner("runner-b", 2, true));

    let decision = dispatcher
        .dispatch(&DispatchRequest {
            session_id: "healthy-only".into(),
        })
        .unwrap();

    assert_eq!(decision.runner_id.as_str(), "runner-b");
}

#[test]
fn runners_without_restore_capacity_are_not_selected() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.upsert_runner(runner("runner-a", 0, true));

    let error = dispatcher
        .dispatch(&DispatchRequest {
            session_id: "blocked".into(),
        })
        .unwrap_err();

    assert_eq!(error, DispatchError::NoRunnerAvailable);
}
