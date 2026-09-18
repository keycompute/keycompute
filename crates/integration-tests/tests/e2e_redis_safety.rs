//! Fault injection requires an explicitly marked disposable Redis instance.
//! Never point FAULT_REDIS_URL at production or the ordinary shared test Redis.
use deadpool_redis::{
    Pool,
    redis::{self, aio::MultiplexedConnection},
};
use keycompute_config::RedisConfig;
use keycompute_db::DbRouter;
use keycompute_ratelimit::{RateLimitConfig, RateLimitKey, RateLimitService};
use keycompute_runtime::{redis_roles::RedisConnectionRole, redis_store::RedisRuntimeStore};
use keycompute_server::{AppState, AppStateConfig, state::RateLimitBackendConfig};
use node_gateway::NodeGatewayRedis;
use serial_test::serial;
use std::{sync::Arc, time::Duration};
use tokio::time::timeout;
use uuid::Uuid;

struct FaultRedis {
    url: String,
    admin: MultiplexedConnection,
}
impl FaultRedis {
    async fn connect() -> Option<Self> {
        let url = match std::env::var("FAULT_REDIS_URL") {
            Ok(url) => url,
            Err(_) if std::env::var_os("CI").is_some() => {
                panic!("CI must provide disposable FAULT_REDIS_URL")
            }
            Err(_) => {
                eprintln!("set FAULT_REDIS_URL to run destructive test-only Redis faults");
                return None;
            }
        };
        let mut admin = timeout(
            Duration::from_secs(3),
            redis::Client::open(url.clone())
                .unwrap()
                .get_multiplexed_async_connection(),
        )
        .await
        .unwrap()
        .unwrap();
        admin.set_response_timeout(Duration::from_secs(3));
        let marker: Option<String> = redis::cmd("GET")
            .arg("keycompute:test:fault-instance")
            .query_async(&mut admin)
            .await
            .unwrap();
        assert_eq!(
            marker.as_deref(),
            Some("disposable-redis-v1"),
            "refusing CONFIG mutations: endpoint is not explicitly marked disposable"
        );
        let mut env = Self { url, admin };
        env.reset().await;
        Some(env)
    }
    async fn config(&mut self, name: &str, value: &str) {
        redis::cmd("CONFIG")
            .arg("SET")
            .arg(name)
            .arg(value)
            .query_async::<()>(&mut self.admin)
            .await
            .unwrap();
    }
    async fn reset(&mut self) {
        self.config("maxmemory", "64mb").await;
        self.config("maxmemory-policy", "noeviction").await;
    }
    async fn client_id(&mut self, pool: &Pool) -> i64 {
        let mut conn = pool.get().await.unwrap();
        redis::cmd("CLIENT")
            .arg("ID")
            .query_async(&mut conn)
            .await
            .unwrap()
    }
}
fn pool(url: &str) -> Pool {
    RedisRuntimeStore::create_pool_with_role(
        url,
        1,
        Duration::from_millis(500),
        Duration::from_millis(300),
        Duration::from_millis(500),
        RedisConnectionRole::CriticalState,
    )
    .unwrap()
}
fn assert_oom<T: std::fmt::Debug, E: std::fmt::Display>(result: Result<T, E>) {
    match result {
        Ok(value) => panic!("state growth succeeded under OOM: {value:?}"),
        Err(error) => assert!(
            error.to_string().to_ascii_lowercase().contains("oom"),
            "unexpected failure: {error}"
        ),
    }
}

#[tokio::test]
#[serial(redis_faults)]
async fn critical_oom_rejects_growth_before_mutation_but_cleanup_and_recovery_work() {
    let Some(mut env) = FaultRedis::connect().await else {
        return;
    };
    let command_pool = pool(&env.url);
    let limiter = RateLimitService::with_redis_pool(command_pool.clone());
    let node = NodeGatewayRedis::new(
        Arc::new(RedisRuntimeStore::with_pool(command_pool.clone())),
        &RedisConfig {
            url: env.url.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let physical = Uuid::new_v4();
    let terminal = Uuid::new_v4();
    let tombstone = Uuid::new_v4();
    let limit = RateLimitConfig::new(100, 1000);
    limiter
        .reserve_token_usage(&key, physical, terminal, 31, &limit)
        .await
        .unwrap();
    limiter
        .record_token_usage_once(&key, tombstone, 7)
        .await
        .unwrap();
    let model = format!("safety-{}", Uuid::new_v4());
    let task = Uuid::new_v4();
    node.push_to_model_queue(&model, task).await.unwrap();
    node.push_result_notification(task, "original")
        .await
        .unwrap();
    // Prime growth scripts too: the test exercises cached EVALSHA, not only NOSCRIPT.
    node.repush_queued_task(&model, task).await.unwrap();
    limiter
        .check_and_record_with_config(&key, &limit)
        .await
        .unwrap();
    let lock_key = format!("safety:lock:{}", Uuid::new_v4());
    let (locked, owner) = keycompute_cache::lock::acquire_lock(
        &command_pool,
        &lock_key,
        120,
        1,
        Duration::from_millis(1),
    )
    .await
    .unwrap();
    assert!(locked);
    let fill_key = format!("safety:fill:{}", Uuid::new_v4());
    redis::cmd("SET")
        .arg(&fill_key)
        .arg(vec![b'x'; 4 * 1024 * 1024])
        .arg("EX")
        .arg(120)
        .query_async::<()>(&mut env.admin)
        .await
        .unwrap();
    env.config("maxmemory", "2mb").await;
    let direct: redis::RedisResult<()> = redis::cmd("SET")
        .arg("safety:oom-new-key")
        .arg("x")
        .query_async(&mut env.admin)
        .await;
    assert_oom(direct);
    for _ in 0..20 {
        let fresh = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        assert_oom(limiter.check_and_record_with_config(&fresh, &limit).await);
        assert_eq!(limiter.get_rpm_count(&fresh).await.unwrap(), 0);
    }
    assert_oom(
        limiter
            .reserve_token_usage(&key, Uuid::new_v4(), Uuid::new_v4(), 3, &limit)
            .await,
    );
    assert_oom(
        limiter
            .restore_token_reservation(&key, Uuid::new_v4(), Uuid::new_v4(), 3)
            .await,
    );
    assert_oom(
        limiter
            .renew_token_reservation(&key, physical, terminal, 31)
            .await,
    );
    assert_oom(
        limiter
            .reconcile_token_usage_now(&key, physical, terminal, 17)
            .await,
    );
    assert_oom(node.push_to_model_queue(&model, Uuid::new_v4()).await);
    assert_oom(node.repush_queued_task(&model, task).await);
    assert_oom(node.push_result_notification(task, "replacement").await);
    assert_eq!(
        limiter.get_tpm_count(&key).await.unwrap(),
        38,
        "rejected settlement erased prediction"
    );
    let queue: Vec<String> = redis::cmd("LRANGE")
        .arg(format!("queue:node:model:{model}"))
        .arg(0)
        .arg(-1)
        .query_async(&mut env.admin)
        .await
        .unwrap();
    let notification: Vec<String> = redis::cmd("LRANGE")
        .arg(format!("task:result:{task}"))
        .arg(0)
        .arg(-1)
        .query_async(&mut env.admin)
        .await
        .unwrap();
    assert_eq!(queue, vec![task.to_string()]);
    assert_eq!(notification, vec!["original".to_string()]);
    // Under OOM release still shrinks existing records and preserves terminal fencing.
    limiter
        .release_token_reservation(&key, physical)
        .await
        .unwrap();
    assert_eq!(limiter.get_tpm_count(&key).await.unwrap(), 7);
    limiter
        .release_token_reservation(&key, tombstone)
        .await
        .unwrap();
    assert_eq!(limiter.get_tpm_count(&key).await.unwrap(), 7);
    assert_eq!(node.remove_from_model_queue(&model, task).await.unwrap(), 1);
    keycompute_cache::lock::release_lock(&command_pool, &lock_key, "wrong-owner")
        .await
        .unwrap();
    let retained: Option<String> = redis::cmd("GET")
        .arg(&lock_key)
        .query_async(&mut env.admin)
        .await
        .unwrap();
    assert_eq!(retained.as_deref(), Some(owner.as_str()));
    keycompute_cache::lock::release_lock(&command_pool, &lock_key, &owner)
        .await
        .unwrap();
    let released: Option<String> = redis::cmd("GET")
        .arg(&lock_key)
        .query_async(&mut env.admin)
        .await
        .unwrap();
    assert!(released.is_none());
    let stats: String = redis::cmd("INFO")
        .arg("stats")
        .query_async(&mut env.admin)
        .await
        .unwrap();
    assert!(stats.lines().any(|line| line.trim() == "evicted_keys:0"));
    env.reset().await;
    redis::cmd("DEL")
        .arg(fill_key)
        .query_async::<u64>(&mut env.admin)
        .await
        .unwrap();
    limiter
        .reserve_token_usage(&key, physical, terminal, 31, &limit)
        .await
        .unwrap();
    limiter
        .reconcile_token_usage_now(&key, physical, terminal, 17)
        .await
        .unwrap();
    limiter
        .reconcile_token_usage_now(&key, physical, terminal, 17)
        .await
        .unwrap();
    assert_eq!(limiter.get_tpm_count(&key).await.unwrap(), 24);
    node.push_result_notification(task, "replacement")
        .await
        .unwrap();
    assert_eq!(
        node.wait_for_result(task, 1).await.unwrap().as_deref(),
        Some("replacement")
    );
}

#[tokio::test]
#[serial(redis_faults)]
async fn pooled_critical_connection_rejects_online_policy_drift_and_recovers() {
    let Some(mut env) = FaultRedis::connect().await else {
        return;
    };
    let command_pool = pool(&env.url);
    let old_id = env.client_id(&command_pool).await;
    env.config("maxmemory-policy", "allkeys-lru").await;
    assert!(
        timeout(Duration::from_secs(3), command_pool.get())
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(command_pool.status().size, 0);
    env.reset().await;
    assert_ne!(
        env.client_id(&command_pool).await,
        old_id,
        "unsafe idle socket was recycled"
    );
}

#[tokio::test]
#[serial(redis_faults)]
async fn optional_cache_recovers_from_startup_failure_and_rechecks_existing_sockets() {
    let Some(mut env) = FaultRedis::connect().await else {
        return;
    };
    // Initially wrong role: this disposable instance cannot yet be used as cache.
    let state = AppState::try_with_pool_and_config(
        DbRouter::single(sea_orm::DatabaseConnection::Disconnected),
        AppStateConfig {
            rate_limit: RateLimitBackendConfig::Redis(RedisConfig {
                url: integration_tests::common::resolve_redis_url(),
                pool_size: 1,
                cache_url: Some(env.url.clone()),
                cache_pool_size: 1,
                cache_timeout_ms: 300,
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        state.cache.is_available(),
        "startup failure discarded recovery configuration"
    );
    assert!(!state.cache.probe().await);
    let mut probes = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let cache = state.cache.clone();
        probes.spawn(async move { cache.probe().await });
    }
    while let Some(result) = probes.join_next().await {
        assert!(!result.unwrap());
    }
    env.config("maxmemory-policy", "allkeys-lru").await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    // Verification must use its dedicated metadata pool, not this occupied slot.
    let held_command_slot = state.runtime_state.pool().unwrap().get().await.unwrap();
    assert!(
        timeout(Duration::from_secs(3), state.cache.probe())
            .await
            .unwrap()
    );
    drop(held_command_slot);
    let key = format!("safety:cache:{}", Uuid::new_v4());
    state
        .cache
        .set(&key, &"recovered", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(
        state.cache.get::<String>(&key).await.unwrap().as_deref(),
        Some("recovered")
    );
    // A previously ready cache must also reject role drift on the next checkout.
    env.config("maxmemory-policy", "noeviction").await;
    assert!(!state.cache.probe().await);
    env.config("maxmemory-policy", "allkeys-lru").await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(state.cache.probe().await);
    env.config("maxmemory", "0").await;
    assert!(
        !state.cache.probe().await,
        "unbounded cache budget survived checkout validation"
    );
    env.config("maxmemory", "64mb").await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(state.cache.probe().await);
    state.cache.delete(&key).await.unwrap();
    drop(state);
    env.reset().await;
}

struct Relay(tokio::task::JoinHandle<()>);
impl Drop for Relay {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl Relay {
    async fn stop(mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}

#[tokio::test]
async fn optional_cache_recovers_after_real_startup_transport_timeout() {
    let target_url = match std::env::var("CACHE_REDIS_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("CI").is_some() => panic!("CI must provide CACHE_REDIS_URL"),
        Err(_) => return,
    };
    let target = url::Url::parse(&target_url).unwrap();
    assert_eq!(
        target.scheme(),
        "redis",
        "test relay requires plaintext test Redis"
    );
    let target_address = (
        target.host_str().unwrap().to_string(),
        target.port().unwrap_or(6379),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut endpoint = target.clone();
    endpoint.set_host(Some("127.0.0.1")).unwrap();
    endpoint
        .set_port(Some(listener.local_addr().unwrap().port()))
        .unwrap();
    // Bound but not accepting: TCP setup succeeds and Redis setup stalls.
    let state = AppState::try_with_pool_and_config(
        DbRouter::single(sea_orm::DatabaseConnection::Disconnected),
        AppStateConfig {
            rate_limit: RateLimitBackendConfig::Redis(RedisConfig {
                url: integration_tests::common::resolve_redis_url(),
                cache_url: Some(endpoint.to_string()),
                cache_pool_size: 1,
                cache_timeout_ms: 300,
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(!state.cache.probe().await);
    let relay = Relay(tokio::spawn(async move {
        let mut peers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((mut client, _)) = accepted else { break };
                    if peers.len() >= 8 { continue; }
                    let target_address = target_address.clone();
                    peers.spawn(async move {
                        if let Ok(mut backend) = tokio::net::TcpStream::connect(target_address).await {
                            let _ = tokio::io::copy_bidirectional(&mut client, &mut backend).await;
                        }
                    });
                }
                _ = peers.join_next(), if !peers.is_empty() => {}
            }
        }
    }));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(
        timeout(Duration::from_secs(3), state.cache.probe())
            .await
            .unwrap(),
        "cache did not recover after transport became available"
    );
    let key = format!("safety:transport-recovery:{}", Uuid::new_v4());
    state
        .cache
        .set(&key, &"recovered", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(
        state.cache.get::<String>(&key).await.unwrap().as_deref(),
        Some("recovered")
    );
    state.cache.delete(&key).await.unwrap();
    relay.stop().await;
}
