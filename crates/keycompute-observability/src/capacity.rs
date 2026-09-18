//! Fixed-cardinality stage measurements. No tenant, account, model or request
//! labels; canceled futures remain visible rather than reporting success.
use once_cell::sync::Lazy;
use prometheus::{HistogramVec, core::Collector, histogram_opts};
use serde::Serialize;
use std::{future::Future, time::Instant};

#[derive(Debug, Clone, Copy)]
pub enum Stage {
    Ingress,
    GenerationQueue,
    Authentication,
    Routing,
    BalanceQueue,
    BalanceReserve,
    AccountAdmission,
    Upstream,
    Settlement,
    WriterBegin,
    BalanceRowLock,
    SettlementQueue,
    LedgerWrite,
    BalanceSettlement,
    Distribution,
    NodeTips,
    OutboxPersist,
}
impl Stage {
    const ALL: [Self; 17] = [
        Self::Ingress,
        Self::GenerationQueue,
        Self::Authentication,
        Self::Routing,
        Self::BalanceQueue,
        Self::BalanceReserve,
        Self::AccountAdmission,
        Self::Upstream,
        Self::Settlement,
        Self::WriterBegin,
        Self::BalanceRowLock,
        Self::SettlementQueue,
        Self::LedgerWrite,
        Self::BalanceSettlement,
        Self::Distribution,
        Self::NodeTips,
        Self::OutboxPersist,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Ingress => "ingress_queue",
            Self::GenerationQueue => "generation_queue",
            Self::Authentication => "authentication",
            Self::Routing => "routing",
            Self::BalanceQueue => "balance_queue",
            Self::BalanceReserve => "balance_reserve",
            Self::AccountAdmission => "account_admission",
            Self::Upstream => "upstream_attempt",
            Self::Settlement => "immediate_settlement",
            Self::WriterBegin => "writer_transaction_begin",
            Self::BalanceRowLock => "balance_row_lock_query",
            Self::SettlementQueue => "settlement_balance_queue",
            Self::LedgerWrite => "usage_ledger_write",
            Self::BalanceSettlement => "balance_settlement",
            Self::Distribution => "distribution_effect",
            Self::NodeTips => "node_tip_effect",
            Self::OutboxPersist => "settlement_outbox_write",
        }
    }
}
static DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    let h = HistogramVec::new(
        histogram_opts!(
            "keycompute_stage_duration_seconds",
            "Stage elapsed time, including its dependency and pool waits",
            vec![
                0.0001, 0.0005, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
                10.0, 30.0, 60.0, 120.0, 600.0
            ]
        ),
        &["stage", "outcome"],
    )
    .expect("stage histogram");
    crate::metrics::REGISTRY
        .register(Box::new(h.clone()))
        .expect("register stage histogram");
    h
});
struct Timer {
    stage: Stage,
    start: Instant,
    outcome: &'static str,
}
impl Timer {
    fn new(stage: Stage) -> Self {
        Self {
            stage,
            start: Instant::now(),
            outcome: "cancelled",
        }
    }
}
impl Drop for Timer {
    fn drop(&mut self) {
        DURATION
            .with_label_values(&[self.stage.label(), self.outcome])
            .observe(self.start.elapsed().as_secs_f64());
    }
}
pub async fn measure<T, E>(
    stage: Stage,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let mut timer = Timer::new(stage);
    let result = future.await;
    timer.outcome = if result.is_ok() { "ok" } else { "error" };
    result
}
pub async fn measure_bool(stage: Stage, future: impl Future<Output = bool>) -> bool {
    let mut timer = Timer::new(stage);
    let result = future.await;
    timer.outcome = if result { "ok" } else { "error" };
    result
}
#[derive(Debug, Serialize)]
pub struct StageObservation {
    pub stage: &'static str,
    pub outcome: &'static str,
    pub count: u64,
    pub total_seconds: f64,
    pub buckets: Vec<(f64, u64)>,
}
pub fn snapshot() -> Vec<StageObservation> {
    Stage::ALL
        .into_iter()
        .flat_map(|stage| {
            ["ok", "error", "cancelled"]
                .into_iter()
                .map(move |outcome| {
                    let histogram = DURATION.with_label_values(&[stage.label(), outcome]);
                    let families = histogram.collect();
                    let proto = families[0].get_metric()[0].get_histogram();
                    StageObservation {
                        stage: stage.label(),
                        outcome,
                        count: proto.get_sample_count(),
                        total_seconds: proto.get_sample_sum(),
                        buckets: proto
                            .get_bucket()
                            .iter()
                            .map(|b| (b.upper_bound(), b.cumulative_count()))
                            .collect(),
                    }
                })
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn count(outcome: &str) -> u64 {
        snapshot()
            .iter()
            .find(|v| v.stage == "ingress_queue" && v.outcome == outcome)
            .unwrap()
            .count
    }
    #[tokio::test]
    async fn stages_record_success_failure_and_cancellation_without_identity_labels() {
        let before = [count("ok"), count("error"), count("cancelled")];
        assert!(
            measure(Stage::Ingress, async { Ok::<_, ()>(()) })
                .await
                .is_ok()
        );
        assert!(
            measure(Stage::Ingress, async { Err::<(), _>(()) })
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(1),
                measure::<(), ()>(Stage::Ingress, std::future::pending())
            )
            .await
            .is_err()
        );
        assert_eq!(
            [count("ok"), count("error"), count("cancelled")],
            before.map(|n| n + 1)
        );
        assert_eq!(snapshot().len(), Stage::ALL.len() * 3);
    }
}
