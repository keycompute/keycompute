//! Writer-backed eligibility and shared Redis account budgets across callers.
use integration_tests::db::{TestDataGuard, create_test_pool, create_test_tenant};
use keycompute_db::{Account, CreateAccountRequest, DbRouter};
use keycompute_ratelimit::account::AccountQuotaService;
use keycompute_runtime::redis_store::RedisRuntimeStore;
use keycompute_server::account_capacity::ServerAccountCapacity;
use keycompute_types::{
    AccountCapacityPolicy, ExecutionTarget, KeyComputeError, Message, PricingSnapshot,
    RequestContext,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use std::time::Duration;
use uuid::Uuid;

const MODEL: &str = "gpt-account-capacity";
const KEY: &str = "sk-account-capacity-fixture";

struct Fixture {
    db: DatabaseConnection,
    cleanup: TestDataGuard,
    owner: Uuid,
    consumer: Uuid,
    account: Account,
}
impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = Uuid::new_v4().to_string();
        let cleanup = TestDataGuard::new(db.clone(), run.clone());
        let owner = create_test_tenant(&db, "capacity-owner", &run).await.id;
        let consumer = create_test_tenant(&db, "capacity-consumer", &run).await.id;
        keycompute_runtime::set_global_crypto(&keycompute_runtime::ApiKeyCrypto::generate_key())
            .unwrap();
        let account = Account::create(
            &db,
            &CreateAccountRequest {
                tenant_id: owner,
                provider: "openai".into(),
                name: format!("capacity-{run}"),
                endpoint: "https://provider.example/v1".into(),
                upstream_api_key_encrypted: keycompute_runtime::encrypt_api_key(KEY)
                    .unwrap()
                    .into_inner(),
                upstream_api_key_preview: "sk-****".into(),
                rpm_limit: Some(1),
                tpm_limit: Some(100_000),
                priority: Some(10),
                models_supported: vec![MODEL.into()],
                api_capabilities: vec!["chat_completions".into(), "responses".into()],
                visibility: Some("global".into()),
            },
        )
        .await
        .unwrap();
        Self {
            db,
            cleanup,
            owner,
            consumer,
            account,
        }
    }
    fn target(&self) -> ExecutionTarget {
        ExecutionTarget::new_provider(
            "openai",
            self.account.id,
            self.account.endpoint.clone(),
            KEY,
        )
    }
    fn context(&self, tenant: Uuid) -> RequestContext {
        let mut ctx = RequestContext::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            tenant,
            Uuid::new_v4(),
            MODEL,
            vec![Message::user("hello")],
            false,
            PricingSnapshot::default(),
        );
        ctx.max_tokens = Some(10);
        ctx
    }
    fn policy(&self) -> ServerAccountCapacity {
        let url = integration_tests::common::resolve_redis_url();
        let pool = RedisRuntimeStore::create_pool(&url).unwrap();
        ServerAccountCapacity {
            db: DbRouter::single(self.db.clone()),
            quotas: AccountQuotaService::redis(pool, 2, Duration::from_secs(180)).unwrap(),
        }
    }
    async fn update(&self, clause: &str) {
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!("UPDATE accounts SET {clause} WHERE id=$1"),
                [self.account.id.into()],
            ))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn account_quota_two_replicas_aggregate_distinct_tenants_users_and_api_keys() {
    let mut f = Fixture::new().await;
    let a = f.policy();
    let b = f.policy();
    let mut lease = a.admit(&f.context(f.owner), &f.target()).await.unwrap();
    assert_eq!(b.snapshot(f.account.id).await.unwrap().in_flight, 1);
    lease.finish(Some(7), true).await.unwrap();
    let denied = b.admit(&f.context(f.consumer), &f.target()).await;
    assert!(matches!(denied, Err(KeyComputeError::RateLimitExceeded(_))));
    let counts = b.snapshot(f.account.id).await.unwrap();
    assert_eq!(counts.rpm, 1);
    assert_eq!(counts.tpm, 7);
    assert_eq!(counts.in_flight, 0);
    f.cleanup.cleanup().await.unwrap();
}

#[tokio::test]
async fn account_policy_rechecks_visibility_enabled_model_and_capability() {
    let mut f = Fixture::new().await;
    let policy = f.policy();
    let ctx = f.context(f.consumer);
    let target = f.target();
    for (change, restore) in [
        ("visibility='tenant'", "visibility='global'"),
        ("enabled=FALSE", "enabled=TRUE"),
        (
            "models_supported=ARRAY[]::TEXT[]",
            "models_supported=ARRAY['gpt-account-capacity']::TEXT[]",
        ),
        (
            "api_capabilities=ARRAY['responses']::TEXT[]",
            "api_capabilities=ARRAY['chat_completions','responses']::TEXT[]",
        ),
        ("health_status='unhealthy'", "health_status='unknown'"),
    ] {
        f.update(change).await;
        assert!(
            matches!(
                policy.admit(&ctx, &target).await,
                Err(KeyComputeError::PermissionDenied(_))
            ),
            "stale target accepted after {change}"
        );
        assert_eq!(policy.snapshot(f.account.id).await.unwrap().rpm, 0);
        f.update(restore).await;
    }
    f.cleanup.cleanup().await.unwrap();
}

#[tokio::test]
async fn account_policy_rejects_changed_connection_and_inactive_account_owner() {
    let mut f = Fixture::new().await;
    let policy = f.policy();
    let ctx = f.context(f.consumer);
    let target = f.target();
    f.update("endpoint='https://changed.example/v1'").await;
    assert!(matches!(
        policy.admit(&ctx, &target).await,
        Err(KeyComputeError::PermissionDenied(_))
    ));
    f.update("endpoint='https://provider.example/v1'").await;
    let encrypted = keycompute_runtime::encrypt_api_key("replacement-key")
        .unwrap()
        .into_inner();
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE accounts SET upstream_api_key_encrypted=$2 WHERE id=$1",
        [f.account.id.into(), encrypted.into()],
    ))
    .await
    .unwrap();
    assert!(matches!(
        policy.admit(&ctx, &target).await,
        Err(KeyComputeError::PermissionDenied(_))
    ));
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenants SET status='inactive' WHERE id=$1",
        [f.owner.into()],
    ))
    .await
    .unwrap();
    assert!(matches!(
        policy.admit(&ctx, &target).await,
        Err(KeyComputeError::PermissionDenied(_))
    ));
    assert_eq!(policy.snapshot(f.account.id).await.unwrap().rpm, 0);
    f.cleanup.cleanup().await.unwrap();
}

#[tokio::test]
async fn account_policy_snapshot_outage_and_configuration_failure_are_fail_closed() {
    let db = DbRouter::single(sea_orm::DatabaseConnection::Disconnected);
    let quotas = AccountQuotaService::memory(1, Duration::from_secs(180)).unwrap();
    let policy = ServerAccountCapacity { db, quotas };
    let ctx = RequestContext::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        MODEL,
        vec![],
        false,
        PricingSnapshot::default(),
    );
    let target = ExecutionTarget::new_provider("openai", Uuid::new_v4(), "http://mock", KEY);
    assert!(matches!(
        policy.admit(&ctx, &target).await,
        Err(KeyComputeError::ServiceUnavailable(_))
    ));
    assert_eq!(
        policy
            .snapshot(match target {
                ExecutionTarget::UpstreamAccount { account_id, .. } => account_id,
                _ => unreachable!(),
            })
            .await
            .unwrap()
            .rpm,
        0
    );
}

#[tokio::test]
async fn account_snapshot_uses_one_checkout_without_changing_quota_semantics() {
    use keycompute_ratelimit::RateLimitConfig;
    use keycompute_types::AccountAttemptLease;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let checkouts = Arc::new(AtomicUsize::new(0));
    let created = checkouts.clone();
    let recycled = checkouts.clone();
    let mut config =
        deadpool_redis::Config::from_url(integration_tests::common::resolve_redis_url());
    config.pool = Some(deadpool_redis::PoolConfig::new(1));
    let pool = config
        .builder()
        .unwrap()
        .runtime(deadpool_redis::Runtime::Tokio1)
        .post_create(deadpool_redis::Hook::sync_fn(move |_, _| {
            created.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }))
        .post_recycle(deadpool_redis::Hook::sync_fn(move |_, _| {
            recycled.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }))
        .build()
        .unwrap();
    let service = AccountQuotaService::redis(pool, 2, Duration::from_secs(180)).unwrap();
    let id = Uuid::new_v4();
    let mut lease = service
        .admit(id, 70, RateLimitConfig::new(10, 100))
        .await
        .unwrap();
    checkouts.store(0, Ordering::SeqCst);
    let loaded = service.snapshot(id).await.unwrap();
    assert_eq!((loaded.rpm, loaded.tpm, loaded.in_flight), (1, 70, 1));
    assert_eq!(checkouts.load(Ordering::SeqCst), 1);
    lease.finish(Some(30), true).await.unwrap();
    checkouts.store(0, Ordering::SeqCst);
    let loaded = service.snapshot(id).await.unwrap();
    assert_eq!((loaded.rpm, loaded.tpm, loaded.in_flight), (1, 30, 0));
    assert_eq!(checkouts.load(Ordering::SeqCst), 1);
}
