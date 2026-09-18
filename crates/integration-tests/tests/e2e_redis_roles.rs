//! Separate disposable cache pressure from quota and runtime-state correctness.
use deadpool_redis::{Pool, redis};
use keycompute_config::RedisConfig;
use keycompute_db::DbRouter;
use keycompute_ratelimit::{RateLimitConfig, RateLimitKey, account::AccountQuotaService};
use keycompute_runtime::{redis_roles::RedisConnectionRole, redis_store::RedisRuntimeStore};
use keycompute_server::{AppState, AppStateConfig, state::RateLimitBackendConfig};
use keycompute_types::{AccountAttemptLease, KeyComputeError};
use serial_test::serial;
use std::time::Duration;
use uuid::Uuid;

fn critical_url() -> String {
    integration_tests::common::resolve_redis_url()
}
fn cache_url() -> Option<String> {
    match std::env::var("CACHE_REDIS_URL").or_else(|_| std::env::var("KC__REDIS__CACHE_URL")) {
        Ok(url) => Some(url),
        Err(_) if std::env::var_os("CI").is_some() => {
            panic!("CI must configure the separate cache Redis")
        }
        Err(_) => {
            eprintln!("Set CACHE_REDIS_URL to run separate Redis integration coverage");
            None
        }
    }
}
async fn state_with_cache(cache: Option<String>) -> keycompute_server::error::Result<AppState> {
    AppState::try_with_pool_and_config(
        DbRouter::single(sea_orm::DatabaseConnection::Disconnected),
        AppStateConfig {
            rate_limit: RateLimitBackendConfig::Redis(RedisConfig {
                url: critical_url(),
                cache_url: cache,
                cache_pool_size: 2,
                cache_timeout_ms: 500,
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
}
fn role_pool(url: &str, role: RedisConnectionRole) -> Pool {
    RedisRuntimeStore::create_pool_with_role(
        url,
        2,
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(1),
        role,
    )
    .unwrap()
}
async fn info(pool: &Pool, section: &str) -> String {
    let mut conn = pool.get().await.unwrap();
    redis::cmd("INFO")
        .arg(section)
        .query_async(&mut conn)
        .await
        .unwrap()
}
fn field<'a>(text: &'a str, name: &str) -> &'a str {
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(key, value)| (key == name).then_some(value.trim()))
        .unwrap()
}

#[tokio::test]
async fn missing_cache_endpoint_never_reuses_critical_pool() {
    let state = state_with_cache(None).await.unwrap();
    assert!(!state.cache.is_available());
    assert!(state.runtime_state.is_available());
    let key = format!("role-test:{}", Uuid::new_v4());
    state
        .cache
        .set(&key, &"disposable", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(state.runtime_state.get::<String>(&key).await.unwrap(), None);
    state
        .runtime_state
        .set(&key, &"critical", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(state.cache.get::<String>(&key).await.unwrap(), None);
    state.runtime_state.delete(&key).await.unwrap();
}

#[tokio::test]
async fn optional_cache_outage_does_not_disable_critical_quota_checks() {
    let state = state_with_cache(Some("redis://127.0.0.1:0".into()))
        .await
        .unwrap();
    assert!(
        state.cache.is_available(),
        "configured cache must remain recoverable"
    );
    assert!(!state.cache.probe().await);
    let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    state
        .rate_limiter
        .check_and_record_with_config(&key, &RateLimitConfig::new(1, 100))
        .await
        .unwrap();
    assert!(matches!(
        state
            .rate_limiter
            .check_and_record_with_config(&key, &RateLimitConfig::new(1, 100))
            .await,
        Err(KeyComputeError::RateLimitExceeded(_))
    ));
}

#[tokio::test]
async fn cache_database_number_does_not_count_as_physical_isolation() {
    let mut same = url::Url::parse(&critical_url()).unwrap();
    same.set_path("/1");
    let config = RedisConfig {
        url: critical_url(),
        cache_url: Some(same.to_string()),
        ..Default::default()
    };
    assert!(config.validate_cache_endpoint().is_err());
    // Even bypassing static URL validation cannot make this pool hand out a
    // connection: the runtime run_id check rejects the same physical server.
    let critical = role_pool(&critical_url(), RedisConnectionRole::CriticalState);
    let cache = role_pool(
        same.as_str(),
        RedisConnectionRole::EvictableCache {
            critical_pool: critical,
        },
    );
    let error = match cache.get().await {
        Ok(_) => panic!("same server accepted as cache"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("must not be the critical Redis server")
    );
}

#[tokio::test]
async fn evictable_redis_cannot_be_used_for_critical_state_or_node_work() {
    let Some(url) = cache_url() else { return };
    let pool = role_pool(&url, RedisConnectionRole::CriticalState);
    let error = match pool.get().await {
        Ok(_) => panic!("evictable primary accepted as critical state"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("noeviction"));
    let result = AppState::try_with_pool_and_config(
        DbRouter::single(sea_orm::DatabaseConnection::Disconnected),
        AppStateConfig {
            rate_limit: RateLimitBackendConfig::Redis(RedisConfig {
                url,
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await;
    assert!(
        result.is_err(),
        "server must not start with evictable quota/queue state"
    );
}

#[tokio::test]
#[serial(redis_role_cache)]
async fn cache_eviction_does_not_erase_quota_slots_or_runtime_affinity() {
    let Some(url) = cache_url() else { return };
    let state = state_with_cache(Some(url)).await.unwrap();
    assert!(
        state.cache.is_available(),
        "separate cache must be role-verified and enabled"
    );
    let critical = state.runtime_state.pool().unwrap();
    let cache = state.cache.pool().unwrap();
    let primary_info = info(critical, "server").await;
    let cache_info = info(cache, "server").await;
    assert_ne!(field(&primary_info, "run_id"), field(&cache_info, "run_id"));
    assert_eq!(
        field(&info(critical, "memory").await, "maxmemory_policy"),
        "noeviction"
    );
    let before: u64 = field(&info(cache, "stats").await, "evicted_keys")
        .parse()
        .unwrap();
    let prefix = format!("role-pressure:{}", Uuid::new_v4());
    let affinity = serde_json::json!({"account_id":Uuid::new_v4(),"tenant_id":Uuid::new_v4()});
    state
        .runtime_state
        .set(&prefix, &affinity, Duration::from_secs(120))
        .await
        .unwrap();
    let caller_key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    state
        .rate_limiter
        .check_and_record_with_config(&caller_key, &RateLimitConfig::new(1, 1000))
        .await
        .unwrap();
    let quotas = AccountQuotaService::redis(critical.clone(), 1, Duration::from_secs(180)).unwrap();
    let account = Uuid::new_v4();
    let mut lease = quotas
        .admit(account, 17, RateLimitConfig::new(1, 1000))
        .await
        .unwrap();
    let payload = "x".repeat(64 * 1024);
    // The dedicated test cache has an 8 MiB maxmemory budget. Each call is
    // bounded and only this test's disposable namespace is populated.
    for index in 0..512 {
        state
            .cache
            .set(
                &format!("{prefix}:{index}"),
                &payload,
                Duration::from_secs(60),
            )
            .await
            .unwrap();
    }
    let after: u64 = field(&info(cache, "stats").await, "evicted_keys")
        .parse()
        .unwrap();
    assert!(
        after > before,
        "test did not actually exercise Redis cache eviction"
    );
    assert_eq!(
        state
            .runtime_state
            .get::<serde_json::Value>(&prefix)
            .await
            .unwrap(),
        Some(affinity)
    );
    assert!(matches!(
        state
            .rate_limiter
            .check_and_record_with_config(&caller_key, &RateLimitConfig::new(1, 1000))
            .await,
        Err(KeyComputeError::RateLimitExceeded(_))
    ));
    let usage = quotas.snapshot(account).await.unwrap();
    assert_eq!((usage.rpm, usage.tpm, usage.in_flight), (1, 17, 1));
    lease.finish(Some(17), true).await.unwrap();
    state.runtime_state.delete(&prefix).await.unwrap();
    for index in 0..512 {
        state
            .cache
            .delete(&format!("{prefix}:{index}"))
            .await
            .unwrap();
    }
}
