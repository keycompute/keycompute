//! PricingService + CacheService 端到端集成测试
//!
//! 验证 L1（本地 LRU）和 L2（Redis 分布式缓存）两级缓存链路的正确性：
//! - L2 缓存写回和命中
//! - L2 + L1 两级缓存一致性
//! - get_or_insert_with_lock 防击穿路径
//! - 跨租户定价隔离（租户特定定价永不泄漏到 nil_tenant 共享 key）

use integration_tests::db::initialize_test_schema;
use keycompute_cache::CacheService;
use keycompute_db::{
    CreatePricingRequest,
    models::pricing_model::{BillingDimension, PricingScopeType},
};
use keycompute_pricing::PricingService;
use sea_orm::ConnectionTrait;
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

/// 获取测试用 Redis URL
fn get_redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
}

/// 获取测试用 Database URL
fn get_database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://keycompute:change-me-strong-password@localhost:5432/keycompute".to_string()
    })
}

/// 生成唯一的测试标识符
fn generate_test_id() -> String {
    Uuid::new_v4().simple().to_string()
}

/// 尝试创建 CacheService（Redis 不可用时跳过）
async fn try_create_cache() -> Option<Arc<CacheService>> {
    let url = get_redis_url();
    if url.is_empty() {
        return None;
    }

    let pool = match keycompute_runtime::redis_store::RedisRuntimeStore::create_pool(&url) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIP: Failed to create Redis pool: {}", e);
            return None;
        }
    };

    // 验证 Redis 可用
    if pool.get().await.is_err() {
        eprintln!("SKIP: Redis not reachable");
        return None;
    }

    let test_prefix = format!("kc:e2e:cache:{}:", generate_test_id());
    Some(Arc::new(
        CacheService::with_pool(pool).with_prefix(test_prefix),
    ))
}

/// 尝试创建数据库连接（DB 不可用时跳过）
async fn try_create_db_pool() -> Option<sea_orm::DatabaseConnection> {
    let url = get_database_url();
    match sea_orm::Database::connect(&url).await {
        Ok(db) => Some(db),
        Err(e) => {
            eprintln!("SKIP: Database not reachable: {}", e);
            None
        }
    }
}

/// 测试：PricingService 通过 L2 缓存回源，第二次调用命中 L2 缓存
///
/// 场景：无数据库连接，create_snapshot 应使用 L2 缓存 + 硬编码默认价格。
/// 第一次调用：L1 miss → L2 miss + 锁获取 → 回源（硬编码默认）→ 写入 L2 + L1
/// 第二次调用：L1 miss（模拟过期）→ L2 hit → 填充 L1
#[tokio::test]
async fn test_pricing_with_l2_cache_lifecycle() {
    let Some(cache) = try_create_cache().await else {
        return;
    };

    let pricing = PricingService::new().with_dist_cache(Arc::clone(&cache));
    let tenant_id = Uuid::new_v4();

    // 第一次调用：L1 miss → L2 miss → 回源（硬编码默认）→ 写入 L2 + L1
    let snapshot1 = pricing
        .create_snapshot("gpt-4o", &tenant_id, None)
        .await
        .expect("Should return hardcoded default pricing");
    assert_eq!(snapshot1.model_name, "gpt-4o");
    assert!(snapshot1.input_price_per_1k > rust_decimal::Decimal::ZERO);

    // 第二次调用：应命中 L2 缓存，返回相同值
    let snapshot2 = pricing
        .create_snapshot("gpt-4o", &tenant_id, None)
        .await
        .expect("Should return cached pricing");
    assert_eq!(snapshot2.model_name, "gpt-4o");
    assert_eq!(
        snapshot1.input_price_per_1k, snapshot2.input_price_per_1k,
        "Second call should return same cached pricing"
    );
    assert_eq!(
        snapshot1.output_price_per_1k, snapshot2.output_price_per_1k,
        "Second call should return same cached pricing"
    );
}

/// 测试：不同租户的定价缓存隔离
///
/// 场景：两个不同租户请求同一模型定价，应各自缓存且互不干扰。
#[tokio::test]
async fn test_pricing_cache_tenant_isolation() {
    let Some(cache) = try_create_cache().await else {
        return;
    };

    let pricing = PricingService::new().with_dist_cache(Arc::clone(&cache));
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();

    let snap_a = pricing
        .create_snapshot("gpt-4o", &tenant_a, None)
        .await
        .expect("Tenant A should get pricing");
    let snap_b = pricing
        .create_snapshot("gpt-4o", &tenant_b, None)
        .await
        .expect("Tenant B should get pricing");

    // 没有数据库，默认定价应相同
    assert_eq!(snap_a.input_price_per_1k, snap_b.input_price_per_1k);
    assert_eq!(snap_a.output_price_per_1k, snap_b.output_price_per_1k);
}

/// 测试：禁用 L2 缓存（no-op CacheService）时 PricingService 正常工作
///
/// 场景：CacheService::disabled 传递给 PricingService，验证回退到直接 DB 查询。
#[tokio::test]
async fn test_pricing_with_disabled_cache() {
    let cache = Arc::new(CacheService::disabled());
    assert!(
        !cache.is_available(),
        "Disabled cache should not be available"
    );

    let pricing = PricingService::new().with_dist_cache(cache);
    let tenant_id = Uuid::new_v4();

    // 没有 DB 连接 + 没有缓存，应使用硬编码默认价格
    let snapshot = pricing
        .create_snapshot("gpt-4o", &tenant_id, None)
        .await
        .expect("Should work with disabled cache");
    assert_eq!(snapshot.model_name, "gpt-4o");
    assert!(snapshot.input_price_per_1k > rust_decimal::Decimal::ZERO);
}

/// 测试：不同模型的定价缓存独立
#[tokio::test]
async fn test_pricing_cache_model_isolation() {
    let Some(cache) = try_create_cache().await else {
        return;
    };

    let pricing = PricingService::new().with_dist_cache(Arc::clone(&cache));
    let tenant_id = Uuid::new_v4();

    // 两个不同模型
    let snap_gpt = pricing
        .create_snapshot("gpt-4o", &tenant_id, None)
        .await
        .expect("gpt-4o pricing");
    let snap_claude = pricing
        .create_snapshot("claude-3", &tenant_id, None)
        .await
        .expect("claude-3 pricing");

    // 都是硬编码默认值，值应相同（都是统一的默认价格）
    assert_eq!(snap_gpt.input_price_per_1k, snap_claude.input_price_per_1k);
    assert_eq!(snap_gpt.model_name, "gpt-4o");
    assert_eq!(snap_claude.model_name, "claude-3");
}

// ── 跨租户定价隔离测试（验证安全修复） ────────────────────────────

/// 测试：跨租户定价隔离——验证修复后的 nil_tenant key 不泄漏租户自定义定价
///
/// 场景：
/// - DB 中有模型 "gpt-4o" 的默认定价（¥0.10 input / ¥0.30 output）
/// - Tenant A 有自定义定价（¥0.20 input / ¥0.50 output）
/// - Tenant B 使用默认定价
///
/// 验证：
/// 1. Tenant A 调用 create_snapshot → 返回 ¥0.20/¥0.50（租户特定）
/// 2. Tenant B 调用 create_snapshot → 返回 ¥0.10/¥0.30（默认，非 Tenant A 的定价）
/// 3. L2 nil_tenant key 存储的是 ¥0.10/¥0.30（默认），而非 ¥0.20/¥0.50（租户特定）
#[tokio::test]
async fn test_cross_tenant_pricing_isolation_with_db() {
    use bigdecimal::BigDecimal;
    use chrono::Utc;
    use integration_tests::db::{cleanup_test_data, create_test_tenant};
    let db = try_create_db_pool()
        .await
        .expect("isolated PostgreSQL is required");
    let cache = try_create_cache()
        .await
        .expect("isolated Redis is required");
    initialize_test_schema(&db).await.unwrap();
    let run = generate_test_id();
    let tenant_a = create_test_tenant(&db, "pricing-a", &run).await;
    let tenant_b = create_test_tenant(&db, "pricing-b", &run).await;
    let model = format!("pricing-scope-{run}");
    let provider = "provideraccount";
    let default = integration_tests::db::create_test_pricing(
        &db,
        &CreatePricingRequest {
            scope_type: PricingScopeType::Platform,
            tenant_id: None,
            model_name: model.clone(),
            billing_dimension: BillingDimension::ProviderAccount,
            currency: Some("CNY".into()),
            input_price_per_1k: BigDecimal::from_str("0.10").unwrap(),
            output_price_per_1k: BigDecimal::from_str("0.30").unwrap(),
            is_default: Some(true),
            effective_from: Some(Utc::now()),
            effective_until: None,
        },
    )
    .await
    .unwrap();
    integration_tests::db::create_test_pricing(
        &db,
        &CreatePricingRequest {
            scope_type: PricingScopeType::Tenant,
            tenant_id: Some(tenant_a.id),
            model_name: model.clone(),
            billing_dimension: BillingDimension::ProviderAccount,
            currency: Some("CNY".into()),
            input_price_per_1k: BigDecimal::from_str("0.20").unwrap(),
            output_price_per_1k: BigDecimal::from_str("0.50").unwrap(),
            is_default: Some(false),
            effective_from: Some(Utc::now()),
            effective_until: None,
        },
    )
    .await
    .unwrap();
    let pool = keycompute_db::DbRouter::single(db.clone());
    let pricing = PricingService::with_pool(pool.clone()).with_dist_cache(cache.clone());
    // Warm the platform/default result FIRST. A must still resolve its custom
    // price rather than treating an L1 or L2 cache miss as no tenant override.
    let b = pricing
        .create_snapshot(&model, &tenant_b.id, Some(provider))
        .await
        .unwrap();
    let a = pricing
        .create_snapshot(&model, &tenant_a.id, Some(provider))
        .await
        .unwrap();
    assert_eq!(
        b.input_price_per_1k,
        rust_decimal::Decimal::from_str("0.1").unwrap()
    );
    assert_eq!(
        a.input_price_per_1k,
        rust_decimal::Decimal::from_str("0.2").unwrap()
    );
    assert_eq!(
        a.output_price_per_1k,
        rust_decimal::Decimal::from_str("0.5").unwrap()
    );
    for tenant in [tenant_a.id, tenant_b.id] {
        let revision =
            keycompute_db::PricingModel::runtime_cache_revision(&db, tenant, &model, provider)
                .await
                .unwrap();
        let key = format!("pricing:v3:tenant:{tenant}:{model}:{provider}:{revision}");
        assert!(
            cache
                .get::<serde_json::Value>(&key)
                .await
                .unwrap()
                .is_some(),
            "real Redis entry must exist"
        );
    }
    // Independently constructed instances must use correctly tenant-keyed L2.
    let second = PricingService::with_pool(pool.clone()).with_dist_cache(cache.clone());
    assert_eq!(
        second
            .create_snapshot(&model, &tenant_a.id, Some(provider))
            .await
            .unwrap()
            .input_price_per_1k,
        a.input_price_per_1k
    );
    assert_eq!(
        second
            .create_snapshot(&model, &tenant_b.id, Some(provider))
            .await
            .unwrap()
            .input_price_per_1k,
        b.input_price_per_1k
    );
    // Also check local-only caching in both orderings, with no Redis fallback.
    for order in [[tenant_b.id, tenant_a.id], [tenant_a.id, tenant_b.id]] {
        let local = PricingService::with_pool(pool.clone());
        for tenant in order {
            let got = local
                .create_snapshot(&model, &tenant, Some(provider))
                .await
                .unwrap();
            assert_eq!(
                got.input_price_per_1k,
                if tenant == tenant_a.id {
                    a.input_price_per_1k
                } else {
                    b.input_price_per_1k
                }
            );
        }
    }
    // Platform defaults are protected from business deletion; remove only this
    // exact synthetic fixture directly during isolated-test teardown.
    let removed = pool.write_conn().execute(sea_orm::Statement::from_sql_and_values(
        sea_orm::DbBackend::Postgres,
        "DELETE FROM pricing_models WHERE id=$1 AND scope_type='platform' AND tenant_id IS NULL AND model_name=$2",
        [default.id.into(), model.into()],
    )).await.unwrap();
    assert_eq!(removed.rows_affected(), 1);
    cleanup_test_data(&db, &run).await.unwrap();
}

#[tokio::test]
async fn committed_price_revisions_fence_other_process_caches_and_future_boundaries() {
    use chrono::{Duration as ChronoDuration, Utc};
    use integration_tests::db::{
        TestDataGuard, create_test_pool, create_test_pricing, create_test_tenant,
    };
    use keycompute_db::{
        AuditContext, TenantMembership, User,
        models::pricing_model::{TenantPricingScope, UpdatePricingRequest},
    };
    use keycompute_types::{CredentialKind, PlatformRole, TenantRole};
    use sea_orm::{ConnectionTrait, TransactionTrait};
    let db = create_test_pool().await;
    let run = generate_test_id();
    let mut guard = TestDataGuard::new(db.clone(), run.clone());
    let tenant = create_test_tenant(&db, "price-revision", &run).await;
    let model = format!("pricing-revision-{run}");
    let row = create_test_pricing(
        &db,
        &CreatePricingRequest {
            scope_type: PricingScopeType::Tenant,
            tenant_id: Some(tenant.id),
            model_name: model.clone(),
            billing_dimension: BillingDimension::ProviderAccount,
            currency: Some("CNY".into()),
            input_price_per_1k: "0.2".parse().unwrap(),
            output_price_per_1k: "0.3".parse().unwrap(),
            is_default: Some(false),
            effective_from: None,
            effective_until: None,
        },
    )
    .await
    .unwrap();
    let dist = try_create_cache().await.expect("isolated Redis required");
    let router = keycompute_db::DbRouter::single(db.clone());
    let first = PricingService::with_pool(router.clone()).with_dist_cache(dist.clone());
    let second = PricingService::with_pool(router.clone()).with_dist_cache(dist.clone());
    for service in [&first, &second] {
        assert_eq!(
            service
                .create_snapshot(&model, &tenant.id, Some("provideraccount"))
                .await
                .unwrap()
                .input_price_per_1k,
            rust_decimal::Decimal::from_str("0.2").unwrap()
        );
    }
    let user = User::find_by_id(&db, tenant.owner_user_id)
        .await
        .unwrap()
        .unwrap();
    let member = TenantMembership::find(&db, tenant.id, user.id)
        .await
        .unwrap()
        .unwrap();
    let scope = TenantPricingScope::checked(
        tenant.id,
        user.id,
        CredentialKind::Jwt,
        user.token_version,
        tenant.authz_version,
        member.authz_version,
    )
    .unwrap();
    let actor = AuditContext {
        actor_user_id: user.id,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::None,
        actor_tenant_role: Some(TenantRole::Admin),
        request_id: Some(Uuid::new_v4()),
    };
    let tx = db.begin().await.unwrap();
    let changed = keycompute_db::PricingModel::update_in_tenant(
        &tx,
        scope,
        row.id,
        &UpdatePricingRequest {
            input_price_per_1k: Some("0.8".parse().unwrap()),
            output_price_per_1k: None,
            effective_until: None,
            expected_version: row.version,
        },
        &actor,
    )
    .await
    .unwrap();
    // Uncommitted prices and revisions remain invisible even to another process.
    assert_eq!(
        second
            .create_snapshot(&model, &tenant.id, Some("provideraccount"))
            .await
            .unwrap()
            .input_price_per_1k,
        rust_decimal::Decimal::from_str("0.2").unwrap()
    );
    tx.commit().await.unwrap();
    for service in [&first, &second] {
        assert_eq!(
            service
                .create_snapshot(&model, &tenant.id, Some("provideraccount"))
                .await
                .unwrap()
                .input_price_per_1k,
            rust_decimal::Decimal::from_str("0.8").unwrap(),
            "other process L1 must not outlive a committed version"
        );
    }
    let tx = db.begin().await.unwrap();
    keycompute_db::PricingModel::update_in_tenant(
        &tx,
        scope,
        row.id,
        &UpdatePricingRequest {
            input_price_per_1k: Some("0.9".parse().unwrap()),
            output_price_per_1k: None,
            effective_until: None,
            expected_version: changed.version,
        },
        &actor,
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        second
            .create_snapshot(&model, &tenant.id, Some("provideraccount"))
            .await
            .unwrap()
            .input_price_per_1k,
        rust_decimal::Decimal::from_str("0.8").unwrap()
    );
    // A future validity boundary also fences caches without another mutation.
    let tx = db.begin().await.unwrap();
    let until = Utc::now() + ChronoDuration::seconds(1);
    keycompute_db::PricingModel::update_in_tenant(
        &tx,
        scope,
        row.id,
        &UpdatePricingRequest {
            input_price_per_1k: None,
            output_price_per_1k: None,
            effective_until: Some(until),
            expected_version: changed.version,
        },
        &actor,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        second
            .create_snapshot(&model, &tenant.id, Some("provideraccount"))
            .await
            .unwrap()
            .input_price_per_1k,
        rust_decimal::Decimal::from_str("0.8").unwrap()
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let after = second
        .create_snapshot(&model, &tenant.id, Some("provideraccount"))
        .await
        .unwrap();
    assert_eq!(
        after.input_price_per_1k,
        rust_decimal::Decimal::from_str("0.1").unwrap()
    );
    // The existing runtime policy permits a platform fallback from the other
    // billing dimension. Its validity window must also fence this cache key.
    let cross_model = format!("pricing-cross-dimension-{run}");
    let cross = create_test_pricing(
        &db,
        &CreatePricingRequest {
            scope_type: PricingScopeType::Platform,
            tenant_id: None,
            model_name: cross_model.clone(),
            billing_dimension: BillingDimension::ProviderAccount,
            currency: Some("CNY".into()),
            input_price_per_1k: "0.65".parse().unwrap(),
            output_price_per_1k: "0.7".parse().unwrap(),
            is_default: Some(true),
            effective_from: None,
            effective_until: Some(Utc::now() + ChronoDuration::seconds(1)),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        second
            .create_snapshot(&cross_model, &tenant.id, Some("node"))
            .await
            .unwrap()
            .input_price_per_1k,
        rust_decimal::Decimal::from_str("0.65").unwrap()
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert_eq!(
        second
            .create_snapshot(&cross_model, &tenant.id, Some("node"))
            .await
            .unwrap()
            .input_price_per_1k,
        rust_decimal::Decimal::from_str("0.1").unwrap()
    );
    db.execute(sea_orm::Statement::from_sql_and_values(sea_orm::DbBackend::Postgres,
        "DELETE FROM pricing_models WHERE id=$1 AND scope_type='platform' AND tenant_id IS NULL AND model_name=$2",
        [cross.id.into(),cross_model.into()])).await.unwrap();
    // Revision reads are SELECT-only and never persist activity timestamps.
    let count=db.query_one(sea_orm::Statement::from_sql_and_values(sea_orm::DbBackend::Postgres,
        "SELECT version FROM pricing_cache_revisions WHERE scope_type='tenant' AND tenant_id=$1",[tenant.id.into()])).await.unwrap().unwrap();
    assert_eq!(count.try_get::<i64>("", "version").unwrap(), 3);
    guard.cleanup().await.unwrap();
}
