//! Real-PostgreSQL regressions for shared-lock API-key authentication.
//!
//! These tests use held row locks and pg_blocking_pids, not elapsed-time
//! throughput thresholds, to distinguish overlapping readers from the old
//! tenant/user/key serialization. DATABASE_URL must point to a test database.

use integration_tests::common::generate_test_id;
use integration_tests::db::{
    TenantActor, TestDataGuard, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_auth::{AuthContext, ProduceAiKeyValidator};
use keycompute_db::{CreateProduceAiKeyRequest, DbRouter, ProduceAiKey};
use keycompute_types::{KeyComputeError, Result};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait,
};
use std::time::Duration;
use tokio::task::JoinHandle;
use uuid::Uuid;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

struct Fixture {
    pool: DatabaseConnection,
    guard: TestDataGuard,
    test_id: String,
    user: TenantActor,
    key: ProduceAiKey,
    token: String,
    validator: ProduceAiKeyValidator,
}

impl Fixture {
    async fn new() -> Self {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        let guard = TestDataGuard::new(pool.clone(), test_id.clone());
        let tenant = create_test_tenant(&pool, "auth-read", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "auth-read", &test_id).await;
        let (key, token) = create_key(&pool, &user).await;
        let validator = ProduceAiKeyValidator::with_pool(DbRouter::single(pool.clone()));
        Self {
            pool,
            guard,
            test_id,
            user,
            key,
            token,
            validator,
        }
    }
}

async fn create_key(pool: &DatabaseConnection, user: &TenantActor) -> (ProduceAiKey, String) {
    let token = ProduceAiKeyValidator::generate_key();
    let key = ProduceAiKey::create(
        pool,
        &CreateProduceAiKeyRequest {
            tenant_id: user.tenant_id,
            user_id: user.id,
            name: "auth-read-test".to_string(),
            produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&token),
            produce_ai_key_preview: "sk-test-****".to_string(),
            expires_at: None,
        },
    )
    .await
    .expect("test key should be created");
    (key, token)
}

// Never leave a detached validator holding DB locks after a failing assertion.
struct PendingValidation(JoinHandle<Result<AuthContext>>);

impl PendingValidation {
    fn start(validator: ProduceAiKeyValidator, token: String) -> Self {
        Self(tokio::spawn(
            async move { validator.validate(&token).await },
        ))
    }

    async fn finish(&mut self) -> Result<AuthContext> {
        tokio::time::timeout(TEST_TIMEOUT, &mut self.0)
            .await
            .expect("validation must finish within the test budget")
            .expect("validation task must not panic")
    }
}

impl Drop for PendingValidation {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn backend_pid(tx: &DatabaseTransaction) -> i32 {
    tx.query_one(Statement::from_string(
        DbBackend::Postgres,
        "SELECT pg_backend_pid()".to_string(),
    ))
    .await
    .expect("PID query should succeed")
    .expect("PID row should exist")
    .try_get_by_index(0)
    .expect("PID should decode")
}

async fn wait_for_blocked_validator(pool: &DatabaseConnection, gate_pid: i32) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            let row = pool
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE wait_event_type = 'Lock' AND $1 = ANY(pg_blocking_pids(pid)) AND query LIKE '%FOR SHARE%') AS waiting",
                    [gate_pid.into()],
                ))
                .await
                .expect("wait probe should succeed")
                .expect("wait probe should return a row");
            if row
                .try_get_by_index::<bool>(0)
                .expect("waiting flag should decode")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the validator must reach the specific gate, not an unrelated test's lock");
}

async fn hold_row(tx: &DatabaseTransaction, table: &str, id: Uuid, mode: &str) {
    // Only test-owned, fixed table/mode literals are passed to this helper.
    tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!("SELECT id FROM {table} WHERE id = $1 FOR {mode}"),
        [id.into()],
    ))
    .await
    .expect("gate row lock should succeed")
    .expect("gate row should exist");
}

#[tokio::test]
async fn validators_can_overlap_even_for_the_same_key() {
    let mut f = Fixture::new().await;
    let gate = f.pool.begin().await.expect("gate should begin");
    hold_row(&gate, "tenants", f.user.tenant_id, "SHARE").await;
    hold_row(&gate, "users", f.user.id, "SHARE").await;
    hold_row(&gate, "produce_ai_keys", f.key.id, "SHARE").await;

    // Every reader must finish while all three shared locks are still held.
    // An exclusive tenant/user/key lock OR an in-transaction last-used UPDATE
    // would block here, even if the other two locks had already been fixed.
    let validations = (0..8).map(|_| f.validator.validate(&f.token));
    let result =
        tokio::time::timeout(TEST_TIMEOUT, futures::future::try_join_all(validations)).await;
    let key = ProduceAiKey::find_by_id(&f.pool, f.key.id)
        .await
        .expect("key query should succeed")
        .expect("key should exist");
    gate.rollback().await.expect("gate should release");
    let contexts = result
        .expect("shared readers must not wait for the gate to release")
        .expect("all readers should authenticate");
    assert_eq!(contexts.len(), 8);
    assert!(contexts.iter().all(|ctx| ctx.produce_ai_key_id == f.key.id));
    assert!(
        key.last_used_at.is_none(),
        "telemetry must skip a shared-locked key"
    );
    f.guard.cleanup().await.expect("cleanup should succeed");
}

async fn assert_blocked_reader_does_not_serialize_other_requests(same_user: bool) {
    let mut f = Fixture::new().await;
    let other_user = if same_user {
        f.user.clone()
    } else {
        create_test_user(&f.pool, f.user.tenant_id, "auth-other", &f.test_id).await
    };
    let (other_key, other_token) = create_key(&f.pool, &other_user).await;
    let gate = f.pool.begin().await.expect("gate should begin");
    if same_user {
        hold_row(&gate, "produce_ai_keys", f.key.id, "UPDATE").await;
    } else {
        hold_row(&gate, "users", f.user.id, "NO KEY UPDATE").await;
    }
    let gate_pid = backend_pid(&gate).await;
    let mut blocked = PendingValidation::start(f.validator.clone(), f.token.clone());
    wait_for_blocked_validator(&f.pool, gate_pid).await;

    let other_result = tokio::time::timeout(TEST_TIMEOUT, f.validator.validate(&other_token)).await;
    gate.rollback().await.expect("gate should release");
    let other_ctx = other_result
        .expect("a blocked reader must not serialize its tenant or user")
        .expect("the other key should authenticate");
    assert_eq!(other_ctx.produce_ai_key_id, other_key.id);
    blocked
        .finish()
        .await
        .expect("released reader should authenticate");
    f.guard.cleanup().await.expect("cleanup should succeed");
}

#[tokio::test]
async fn blocked_user_does_not_stall_other_users_in_the_tenant() {
    assert_blocked_reader_does_not_serialize_other_requests(false).await;
}

#[tokio::test]
async fn blocked_key_does_not_stall_another_key_of_the_same_user() {
    assert_blocked_reader_does_not_serialize_other_requests(true).await;
}

#[tokio::test]
async fn key_mutations_committed_while_validation_waits_are_rechecked() {
    for mutation in [
        "UPDATE produce_ai_keys SET revoked = TRUE WHERE id = $1",
        "UPDATE produce_ai_keys SET expires_at = NOW() - INTERVAL '1 minute' WHERE id = $1",
        "DELETE FROM produce_ai_keys WHERE id = $1",
    ] {
        let mut f = Fixture::new().await;
        let gate = f.pool.begin().await.expect("gate should begin");
        hold_row(&gate, "produce_ai_keys", f.key.id, "UPDATE").await;
        let gate_pid = backend_pid(&gate).await;
        let mut pending = PendingValidation::start(f.validator.clone(), f.token.clone());
        wait_for_blocked_validator(&f.pool, gate_pid).await;
        gate.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            mutation,
            [f.key.id.into()],
        ))
        .await
        .expect("key mutation should succeed");
        gate.commit().await.expect("key mutation should commit");
        assert!(
            matches!(pending.finish().await, Err(KeyComputeError::AuthError(_))),
            "validation must reject the current key, not authenticate a stale candidate"
        );
        f.guard.cleanup().await.expect("cleanup should succeed");
    }
}

#[tokio::test]
async fn an_inflight_reader_still_excludes_tenant_and_user_updates() {
    let mut f = Fixture::new().await;
    let gate = f.pool.begin().await.expect("gate should begin");
    hold_row(&gate, "produce_ai_keys", f.key.id, "UPDATE").await;
    let gate_pid = backend_pid(&gate).await;
    let mut pending = PendingValidation::start(f.validator.clone(), f.token.clone());
    wait_for_blocked_validator(&f.pool, gate_pid).await;

    // The validator is now holding the tenant and user SHARE locks. Ordinary
    // non-key UPDATEs must conflict, which rules out the unsafe KEY SHARE fix.
    for (sql, id) in [
        (
            "UPDATE tenants SET status = 'inactive' WHERE id = $1",
            f.user.tenant_id,
        ),
        ("UPDATE users SET name = 'Changed' WHERE id = $1", f.user.id),
    ] {
        let writer = f.pool.begin().await.expect("writer should begin");
        writer
            .execute_unprepared("SET LOCAL lock_timeout = '100ms'")
            .await
            .expect("lock timeout should be set");
        let result = tokio::time::timeout(
            TEST_TIMEOUT,
            writer.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                [id.into()],
            )),
        )
        .await
        .expect("writer should hit the database lock timeout");
        writer.rollback().await.expect("writer should roll back");
        let error = result.expect_err("ordinary UPDATE must wait for authentication to commit");
        assert!(
            error.to_string().contains("lock timeout"),
            "unexpected DB error: {error}"
        );
    }
    gate.rollback().await.expect("gate should release");
    pending
        .finish()
        .await
        .expect("rolled-back writers must not invalidate the key");
    f.guard.cleanup().await.expect("cleanup should succeed");
}

#[tokio::test]
async fn cancelling_a_waiting_validator_releases_parent_locks() {
    let mut f = Fixture::new().await;
    let gate = f.pool.begin().await.expect("gate should begin");
    hold_row(&gate, "produce_ai_keys", f.key.id, "UPDATE").await;
    let gate_pid = backend_pid(&gate).await;
    let mut pending = PendingValidation::start(f.validator.clone(), f.token.clone());
    wait_for_blocked_validator(&f.pool, gate_pid).await;
    pending.0.abort();
    let cancelled = tokio::time::timeout(TEST_TIMEOUT, &mut pending.0)
        .await
        .expect("cancelled validator must stop")
        .expect_err("aborted task should not return a result");
    assert!(cancelled.is_cancelled());

    // Dropping a Rust future need not cancel an already-sent PostgreSQL
    // statement. The auth transaction's server-side timeout must bound its
    // remaining lock wait, allowing rollback without releasing the key gate.
    tokio::time::timeout(
        TEST_TIMEOUT,
        f.pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenants SET status = 'inactive' WHERE id = $1",
            [f.user.tenant_id.into()],
        )),
    )
    .await
    .expect("cancelled authentication must release its tenant lock")
    .expect("tenant update should succeed");
    gate.rollback().await.expect("gate should release");
    assert!(matches!(
        f.validator.validate(&f.token).await,
        Err(KeyComputeError::AuthError(_))
    ));
    f.guard.cleanup().await.expect("cleanup should succeed");
}

#[tokio::test]
async fn last_used_is_best_effort_sampled_and_never_rewinds() {
    let mut f = Fixture::new().await;
    let first_seen = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            f.validator
                .validate(&f.token)
                .await
                .expect("key should authenticate");
            let row = ProduceAiKey::find_by_id(&f.pool, f.key.id)
                .await
                .expect("key query should succeed")
                .expect("key should exist");
            if let Some(time) = row.last_used_at {
                break time;
            }
            // Other concurrent tests can temporarily occupy the one telemetry
            // slot. Future authentications retry sampling without queuing.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a healthy idle database should eventually record a sample");

    for _ in 0..8 {
        f.validator
            .validate(&f.token)
            .await
            .expect("key should authenticate");
    }
    let row = ProduceAiKey::find_by_id(&f.pool, f.key.id)
        .await
        .expect("key query should succeed")
        .expect("key should exist");
    assert_eq!(
        row.last_used_at,
        Some(first_seen),
        "hot keys must not write per request"
    );
    assert_eq!(row.user_id, f.user.id);
    assert_eq!(row.tenant_id, f.user.tenant_id);
    assert!(!row.revoked);

    let future_seen = first_seen + chrono::Duration::hours(1);
    f.pool
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE produce_ai_keys SET last_used_at = $2 WHERE id = $1",
            [f.key.id.into(), future_seen.into()],
        ))
        .await
        .expect("future timestamp setup should succeed");
    f.validator
        .validate(&f.token)
        .await
        .expect("telemetry must not affect authorization");
    let row = ProduceAiKey::find_by_id(&f.pool, f.key.id)
        .await
        .expect("key query should succeed")
        .expect("key should exist");
    assert_eq!(
        row.last_used_at,
        Some(future_seen),
        "older samples must never rewind time"
    );
    f.guard.cleanup().await.expect("cleanup should succeed");
}

#[tokio::test]
async fn authentication_lock_timeout_fails_closed_and_allows_retry() {
    let mut f = Fixture::new().await;
    let gate = f.pool.begin().await.expect("gate should begin");
    hold_row(&gate, "produce_ai_keys", f.key.id, "UPDATE").await;
    let gate_pid = backend_pid(&gate).await;
    let mut pending = PendingValidation::start(f.validator.clone(), f.token.clone());
    wait_for_blocked_validator(&f.pool, gate_pid).await;

    // Do not release the gate until PostgreSQL's auth-only wait budget fires.
    // A backend timeout must not be reinterpreted as an authenticated identity.
    let error = pending
        .finish()
        .await
        .expect_err("a blocked authentication must fail closed");
    assert!(
        matches!(&error, KeyComputeError::DatabaseError(message) if message.contains("lock timeout")),
        "unexpected authentication failure: {error}"
    );
    let row = ProduceAiKey::find_by_id(&f.pool, f.key.id)
        .await
        .expect("key query should succeed")
        .expect("key should exist");
    assert!(
        row.last_used_at.is_none(),
        "failed auth must not record usage"
    );

    gate.rollback().await.expect("gate should release");
    f.validator
        .validate(&f.token)
        .await
        .expect("a fresh authentication should succeed once contention clears");
    f.guard.cleanup().await.expect("cleanup should succeed");
}
