//! Sampled, best-effort API-key usage metadata, never an authorization cache.
//!
//! At most one writer per process may be running or waiting for the DB pool.
//! Busy admission is dropped, not queued or retried; a later successful
//! authentication may sample again. Shutdown may lose a sample. Consumers must
//! not use last_used_at as an exact audit, billing, or key-expiration signal.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use keycompute_db::{DbRouter, ProduceAiKey};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const WRITE_TIMEOUT: Duration = Duration::from_millis(250);
const SAMPLE_INTERVAL: ChronoDuration = ChronoDuration::minutes(1);

// SKIP LOCKED is only for optional telemetry, never for the authorization
// reads. It avoids waiting behind an active validator or lifecycle writer.
// The guarded UPDATE coalesces samples across processes as well as locally;
// a delayed sample cannot overwrite a newer timestamp or any security field.
const RECORD_LAST_USED_SQL: &str = r#"
WITH writable AS (
    SELECT id FROM produce_ai_keys
    WHERE id = $1
      AND (last_used_at IS NULL OR last_used_at <= $2::timestamptz - INTERVAL '1 minute')
    FOR NO KEY UPDATE SKIP LOCKED
)
UPDATE produce_ai_keys AS k
SET last_used_at = GREATEST(k.last_used_at, $2::timestamptz),
    updated_at = GREATEST(k.updated_at, NOW())
FROM writable
WHERE k.id = writable.id
"#;

fn sample_due(last_used_at: Option<DateTime<Utc>>, observed_at: DateTime<Utc>) -> bool {
    last_used_at.is_none_or(|last_used| observed_at - last_used >= SAMPLE_INTERVAL)
}

fn try_write_slot() -> Option<OwnedSemaphorePermit> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(SLOTS.get_or_init(|| Arc::new(Semaphore::new(1))))
        .try_acquire_owned()
        .ok()
}

/// Must only be called after the successful authorization transaction commits.
pub(super) fn record(pool: Arc<DbRouter>, key: &ProduceAiKey) {
    let observed_at = Utc::now();
    if !sample_due(key.last_used_at, observed_at) {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    // Acquire before spawning: semaphore waiters would themselves form an
    // unbounded task queue. The permit is released even on cancellation.
    let Some(permit) = try_write_slot() else {
        return;
    };
    let key_id = key.id;
    runtime.spawn(async move {
        let _permit = permit;
        let statement = Statement::from_sql_and_values(
            DbBackend::Postgres,
            RECORD_LAST_USED_SQL,
            [key_id.into(), observed_at.into()],
        );
        // Include pool acquisition, SQL execution and commit in the budget.
        // SKIP LOCKED does not skip table-level locks, so also enforce short
        // server-side limits; cancelling the future alone is insufficient.
        let update = async {
            let tx = pool.begin().await?;
            tx.execute_unprepared(
                "SET LOCAL lock_timeout = '50ms'; SET LOCAL statement_timeout = '100ms'",
            )
            .await?;
            tx.execute(statement).await?;
            tx.commit().await
        };
        match tokio::time::timeout(WRITE_TIMEOUT, update).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::debug!(%key_id, %error, "API-key last-used sample was not recorded");
            }
            Err(_) => {
                tracing::debug!(%key_id, "API-key last-used sample timed out");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_is_due_only_for_missing_or_old_timestamps() {
        let now = Utc::now();
        assert!(sample_due(None, now));
        assert!(sample_due(Some(now - SAMPLE_INTERVAL), now));
        assert!(!sample_due(Some(now - ChronoDuration::seconds(59)), now));
        assert!(!sample_due(Some(now), now));
        assert!(!sample_due(Some(now + SAMPLE_INTERVAL), now));
    }

    #[test]
    fn writer_admission_is_bounded_and_released_on_drop() {
        let permit = try_write_slot().expect("first writer should be admitted");
        assert!(try_write_slot().is_none(), "busy writers must not queue");
        drop(permit);
        assert!(
            try_write_slot().is_some(),
            "a dropped writer must release capacity"
        );
    }
}
