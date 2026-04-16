//! Routing primitives for the scalable `wasmsh + Pyodide` dispatcher.
//!
//! The dispatcher intentionally has no runtime or capability routing
//! dimension. It only reasons about runner health, restore capacity, and
//! session affinity.

#![warn(missing_docs)]

use std::cmp::Ordering;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Identifies a runner instance.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RunnerId(String);

impl RunnerId {
    /// Creates a new runner identifier.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the string form of the identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for RunnerId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for RunnerId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// Routing request for a session.
///
/// There is deliberately no runtime field here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchRequest {
    /// Stable session identifier used for affinity.
    pub session_id: String,
}

/// Runner health and capacity snapshot used for routing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunnerSnapshot {
    /// Runner identity.
    pub runner_id: RunnerId,
    /// Maximum concurrent restores this runner accepts.
    pub restore_slots: u32,
    /// Restores currently in flight.
    pub inflight_restores: u32,
    /// Number of queued restore requests.
    pub restore_queue_depth: u32,
    /// Observed restore p95 in milliseconds.
    pub restore_p95_ms: f64,
    /// Active user sessions on the runner.
    pub active_sessions: u32,
    /// Drain mode rejects new sessions but preserves existing affinity.
    pub draining: bool,
    /// Unhealthy runners are never selected for new sessions.
    pub healthy: bool,
}

impl RunnerSnapshot {
    /// Returns the currently available restore slots.
    #[must_use]
    pub fn available_restore_slots(&self) -> u32 {
        self.restore_slots.saturating_sub(self.inflight_restores)
    }

    /// Returns whether the runner may accept a new session.
    #[must_use]
    pub fn accepts_new_sessions(&self) -> bool {
        self.healthy && !self.draining && self.available_restore_slots() > 0
    }
}

/// Successful routing decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchDecision {
    /// Runner chosen for the request.
    pub runner_id: RunnerId,
    /// Indicates whether an existing affinity mapping was reused.
    pub affinity_reused: bool,
}

/// Dispatcher-level routing errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// No runner can currently accept a new session.
    #[error("no healthy runner has free restore capacity")]
    NoRunnerAvailable,
}

/// In-memory dispatcher model.
#[derive(Debug, Default)]
pub struct Dispatcher {
    runners: HashMap<RunnerId, RunnerSnapshot>,
    session_affinity: HashMap<String, RunnerId>,
}

impl Dispatcher {
    /// Creates an empty dispatcher state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces a runner snapshot.
    pub fn upsert_runner(&mut self, snapshot: RunnerSnapshot) {
        self.runners.insert(snapshot.runner_id.clone(), snapshot);
    }

    /// Removes a runner from the active set.
    pub fn remove_runner(&mut self, runner_id: &RunnerId) -> Option<RunnerSnapshot> {
        self.runners.remove(runner_id)
    }

    /// Clears affinity for a completed session.
    pub fn release_session(&mut self, session_id: &str) {
        self.session_affinity.remove(session_id);
    }

    /// Returns the runner currently pinned to the session, if any.
    #[must_use]
    pub fn affinity_for(&self, session_id: &str) -> Option<&RunnerId> {
        self.session_affinity.get(session_id)
    }

    /// Routes a request using affinity first, then capacity ordering.
    pub fn dispatch(
        &mut self,
        request: &DispatchRequest,
    ) -> Result<DispatchDecision, DispatchError> {
        if let Some(runner_id) = self.session_affinity.get(&request.session_id) {
            if self.runners.contains_key(runner_id) {
                return Ok(DispatchDecision {
                    runner_id: runner_id.clone(),
                    affinity_reused: true,
                });
            }
            self.session_affinity.remove(&request.session_id);
        }

        let selected = self
            .runners
            .values()
            .filter(|snapshot| snapshot.accepts_new_sessions())
            .min_by(|left, right| compare_runner_priority(left, right))
            .map(|snapshot| snapshot.runner_id.clone())
            .ok_or(DispatchError::NoRunnerAvailable)?;

        self.session_affinity
            .insert(request.session_id.clone(), selected.clone());

        Ok(DispatchDecision {
            runner_id: selected,
            affinity_reused: false,
        })
    }
}

fn compare_runner_priority(left: &RunnerSnapshot, right: &RunnerSnapshot) -> Ordering {
    right
        .available_restore_slots()
        .cmp(&left.available_restore_slots())
        .then_with(|| left.restore_queue_depth.cmp(&right.restore_queue_depth))
        .then_with(|| {
            left.restore_p95_ms
                .partial_cmp(&right.restore_p95_ms)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| left.active_sessions.cmp(&right.active_sessions))
        .then_with(|| left.runner_id.cmp(&right.runner_id))
}
