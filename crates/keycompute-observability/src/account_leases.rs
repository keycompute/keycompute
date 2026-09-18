//! Local lease-owner telemetry, never a substitute for shared Redis quota.
use once_cell::sync::Lazy;
use prometheus::{IntCounterVec, IntGauge, opts};
use serde_json::{Value, json};
#[derive(Debug, Clone, Copy)]
pub enum LeaseEvent {
    Admitted,
    Rejected,
    AdmissionError,
    RenewalLost,
    Released,
    Retained,
    SettlementError,
    Abandoned,
}
impl LeaseEvent {
    const ALL: [Self; 8] = [
        Self::Admitted,
        Self::Rejected,
        Self::AdmissionError,
        Self::RenewalLost,
        Self::Released,
        Self::Retained,
        Self::SettlementError,
        Self::Abandoned,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Rejected => "rejected",
            Self::AdmissionError => "admission_error",
            Self::RenewalLost => "renewal_lost",
            Self::Released => "released",
            Self::Retained => "retained",
            Self::SettlementError => "settlement_error",
            Self::Abandoned => "abandoned",
        }
    }
}
static EVENTS: Lazy<IntCounterVec> = Lazy::new(|| {
    let value = IntCounterVec::new(
        opts!(
            "keycompute_account_lease_events_total",
            "Local shared-account lease outcomes"
        ),
        &["event"],
    )
    .unwrap();
    crate::metrics::REGISTRY
        .register(Box::new(value.clone()))
        .unwrap();
    value
});
static OWNERS: Lazy<IntGauge> = Lazy::new(|| {
    let value = IntGauge::new(
        "keycompute_account_lease_owners",
        "Live local owners, not shared Redis slot occupancy",
    )
    .unwrap();
    crate::metrics::REGISTRY
        .register(Box::new(value.clone()))
        .unwrap();
    value
});
pub fn record(event: LeaseEvent) {
    EVENTS.with_label_values(&[event.label()]).inc();
}
#[derive(Debug)]
pub struct LeaseOwner {
    finished: bool,
}
impl LeaseOwner {
    pub fn admitted() -> Self {
        OWNERS.inc();
        record(LeaseEvent::Admitted);
        Self { finished: false }
    }
    pub fn finish(&mut self, released: bool) {
        if !self.finished {
            record(if released {
                LeaseEvent::Released
            } else {
                LeaseEvent::Retained
            });
            self.finished = true;
        }
    }
}
impl Drop for LeaseOwner {
    fn drop(&mut self) {
        OWNERS.dec();
        if !self.finished {
            record(LeaseEvent::Abandoned);
        }
    }
}
pub fn snapshot() -> Value {
    json!({"scope":"local_owners_not_cluster_quota","live_owners":OWNERS.get(),"events":LeaseEvent::ALL.into_iter().map(|event|json!({"event":event.label(),"count":EVENTS.with_label_values(&[event.label()]).get()})).collect::<Vec<_>>()})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_owner_outcomes_are_idempotent_and_cancel_safe() {
        let start = OWNERS.get();
        let released = EVENTS.with_label_values(&["released"]).get();
        let abandoned = EVENTS.with_label_values(&["abandoned"]).get();
        let mut normal = LeaseOwner::admitted();
        let cancelled = LeaseOwner::admitted();
        assert_eq!(OWNERS.get(), start + 2);
        normal.finish(true);
        normal.finish(true);
        drop(normal);
        drop(cancelled);
        assert_eq!(OWNERS.get(), start);
        assert_eq!(EVENTS.with_label_values(&["released"]).get(), released + 1);
        assert_eq!(
            EVENTS.with_label_values(&["abandoned"]).get(),
            abandoned + 1
        );
        assert_eq!(snapshot()["events"].as_array().unwrap().len(), 8);
    }
}
