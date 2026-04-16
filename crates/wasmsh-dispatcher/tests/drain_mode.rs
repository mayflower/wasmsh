use wasmsh_dispatcher::{DispatchRequest, Dispatcher, RunnerSnapshot};

fn runner(id: &str, draining: bool) -> RunnerSnapshot {
    RunnerSnapshot {
        runner_id: id.into(),
        restore_slots: 2,
        inflight_restores: 0,
        restore_queue_depth: 0,
        restore_p95_ms: 10.0,
        active_sessions: 0,
        draining,
        healthy: true,
    }
}

#[test]
fn draining_runners_do_not_receive_new_sessions() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.upsert_runner(runner("runner-a", true));
    dispatcher.upsert_runner(runner("runner-b", false));

    let decision = dispatcher
        .dispatch(&DispatchRequest {
            session_id: "new-session".into(),
        })
        .unwrap();

    assert_eq!(decision.runner_id.as_str(), "runner-b");
}

#[test]
fn draining_runner_keeps_existing_affinity() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.upsert_runner(runner("runner-a", false));
    dispatcher.upsert_runner(runner("runner-b", false));

    let initial = dispatcher
        .dispatch(&DispatchRequest {
            session_id: "sticky-drain".into(),
        })
        .unwrap();

    let mut draining_snapshot = runner(initial.runner_id.as_str(), true);
    draining_snapshot.restore_slots = 1;
    dispatcher.upsert_runner(draining_snapshot);

    let follow_up = dispatcher
        .dispatch(&DispatchRequest {
            session_id: "sticky-drain".into(),
        })
        .unwrap();

    assert_eq!(initial.runner_id, follow_up.runner_id);
    assert!(follow_up.affinity_reused);
}
