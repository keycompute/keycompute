//! Demand-driven, single-flight recovery for an optional Redis cache.
//! No timer or task retains application state after shutdown. Failed probes
//! back off from one to thirty seconds; cancellation also frees the probe slot.
use std::{sync::Mutex, time::Duration};
use tokio::time::Instant;

#[derive(Debug)]
struct State {
    ready: bool,
    probing: bool,
    epoch: u64,
    retry_at: Instant,
    delay: Duration,
}
#[derive(Debug)]
pub(super) struct RecoveryGate {
    state: Mutex<State>,
    minimum: Duration,
    maximum: Duration,
}
impl RecoveryGate {
    pub(super) fn new() -> Self {
        Self::with_delays(Duration::from_secs(1), Duration::from_secs(30))
    }
    fn with_delays(minimum: Duration, maximum: Duration) -> Self {
        Self {
            state: Mutex::new(State {
                ready: false,
                probing: false,
                epoch: 0,
                retry_at: Instant::now(),
                delay: minimum,
            }),
            minimum,
            maximum,
        }
    }
    pub(super) fn begin(&self) -> Option<Probe<'_>> {
        let mut state = self.state.lock().expect("cache recovery state poisoned");
        if !state.ready && (state.probing || Instant::now() < state.retry_at) {
            return None;
        }
        let exclusive = !state.ready;
        state.probing |= exclusive;
        Some(Probe {
            gate: self,
            epoch: state.epoch,
            exclusive,
            completed: false,
        })
    }
    pub(super) fn is_blocked(&self) -> bool {
        let state = self.state.lock().expect("cache recovery state poisoned");
        !state.ready && (state.probing || Instant::now() < state.retry_at)
    }
}

pub(super) struct Probe<'a> {
    gate: &'a RecoveryGate,
    epoch: u64,
    exclusive: bool,
    completed: bool,
}
impl Probe<'_> {
    pub(super) fn succeeded(mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .expect("cache recovery state poisoned");
        if self.exclusive {
            state.probing = false;
        }
        // A late success must not erase a newer concurrent failure.
        if state.epoch == self.epoch {
            state.ready = true;
            state.delay = self.gate.minimum;
        }
        self.completed = true;
    }
    fn failed(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .expect("cache recovery state poisoned");
        if self.exclusive {
            state.probing = false;
        }
        if state.epoch == self.epoch && (state.ready || self.exclusive) {
            state.ready = false;
            state.epoch = state.epoch.wrapping_add(1);
            state.retry_at = Instant::now() + state.delay;
            state.delay = state.delay.saturating_mul(2).min(self.gate.maximum);
        }
        self.completed = true;
    }
}
impl Drop for Probe<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.failed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn failed_and_cancelled_probes_are_single_flight_and_recoverable() {
        let gate = RecoveryGate::with_delays(Duration::from_millis(5), Duration::from_millis(20));
        let probe = gate.begin().unwrap();
        assert!(gate.begin().is_none());
        drop(probe);
        assert!(gate.is_blocked());
        assert!(gate.begin().is_none());
        tokio::time::sleep(Duration::from_millis(10)).await;
        gate.begin().unwrap().succeeded();
        assert!(!gate.is_blocked());
        let first = gate.begin().unwrap();
        let second = gate.begin().unwrap();
        first.succeeded();
        second.succeeded();
    }
    #[tokio::test]
    async fn old_success_cannot_override_new_failure_and_backoff_is_capped() {
        let gate = RecoveryGate::with_delays(Duration::from_millis(1), Duration::from_millis(4));
        gate.begin().unwrap().succeeded();
        let old_success = gate.begin().unwrap();
        drop(gate.begin().unwrap());
        old_success.succeeded();
        assert!(gate.is_blocked());
        for _ in 0..5 {
            tokio::time::sleep(Duration::from_millis(6)).await;
            drop(gate.begin().unwrap());
        }
        assert_eq!(gate.state.lock().unwrap().delay, Duration::from_millis(4));
    }
}
