//! Read-only scheduling snapshots and authoritative per-attempt admission.
//! Snapshots are hints; only `admit` grants permission to contact an upstream.
use crate::{ExecutionTarget, RequestContext, Result};
use async_trait::async_trait;
use std::fmt::Debug;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Default)]
pub struct AccountCapacitySnapshot {
    pub rpm: u64,
    pub tpm: u64,
    pub in_flight: u64,
    pub in_flight_limit: u32,
}

#[async_trait]
pub trait AccountCapacityPolicy: Send + Sync + Debug {
    async fn snapshot(&self, account_id: Uuid) -> Result<AccountCapacitySnapshot>;
    async fn admit(
        &self,
        ctx: &RequestContext,
        target: &ExecutionTarget,
    ) -> Result<Box<dyn AccountAttemptLease>>;
}

#[async_trait]
pub trait AccountAttemptLease: Send + Sync + Debug {
    /// Runs alongside upstream execution. Losing the reservation is fail-closed;
    /// this future must never recreate an expired quota reservation.
    async fn keep_alive(&self) -> Result<()>;
    /// Exact usage is supplied only for a successful, finalized attempt. Unknown
    /// usage retains the admitted prediction. Ambiguous failures may retain the
    /// shared in-flight lease until its bounded deadline rather than oversubscribe.
    async fn finish(&mut self, exact_tokens: Option<u32>, release_in_flight: bool) -> Result<()>;
}
