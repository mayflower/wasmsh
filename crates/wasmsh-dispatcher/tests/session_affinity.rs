use wasmsh_dispatcher::{DispatchRequest, Dispatcher, RunnerSnapshot};

fn runner(id: &str) -> RunnerSnapshot {
    RunnerSnapshot {
        runner_id: id.into(),
        restore_slots: 2,
        inflight_restores: 0,
        restore_queue_depth: 0,
        restore_p95_ms: 10.0,
        active_sessions: 0,
        draining: false,
        healthy: true,
    }
}

#[test]
fn existing_session_stays_on_its_runner() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.upsert_runner(runner("runner-a"));
    dispatcher.upsert_runner(runner("runner-b"));

    let first = dispatcher
        .dispatch(&DispatchRequest {
            session_id: "sticky-session".into(),
        })
        .unwrap();
    let second = dispatcher
        .dispatch(&DispatchRequest {
            session_id: "sticky-session".into(),
        })
        .unwrap();

    assert_eq!(first.runner_id, second.runner_id);
    assert!(second.affinity_reused);
}

#[test]
fn releasing_a_session_clears_affinity() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.upsert_runner(runner("runner-a"));

    dispatcher
        .dispatch(&DispatchRequest {
            session_id: "ephemeral-session".into(),
        })
        .unwrap();
    assert!(dispatcher.affinity_for("ephemeral-session").is_some());

    dispatcher.release_session("ephemeral-session");

    assert!(dispatcher.affinity_for("ephemeral-session").is_none());
}
