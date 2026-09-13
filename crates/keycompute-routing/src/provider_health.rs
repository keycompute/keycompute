//! Provider/账号健康状态管理。
//!
//! 协议级指标仅保留为兼容性诊断；实际路由健康、动态惩罚和持久化状态
//! 均以具体渠道账号 ID 为键。

use chrono::{DateTime, SubsecRound, Utc};
use dashmap::{DashMap, DashSet};
use keycompute_db::{Account, DbRouter};
use keycompute_types::KeyComputeError;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

pub const ACCOUNT_HEALTH_UNKNOWN: &str = "unknown";
pub const ACCOUNT_HEALTHY: &str = "healthy";
pub const ACCOUNT_DEGRADED: &str = "degraded";
pub const ACCOUNT_UNHEALTHY: &str = "unhealthy";

/// Bound per-account runtime health backlog while the database writer is
/// unavailable. Probe snapshots are retained preferentially because they
/// fence the generation used by subsequent runtime events.
const MAX_PENDING_ACCOUNT_HEALTH_EVENTS: usize = 1024;
const MAX_EPHEMERAL_ACCOUNT_HEALTH_ENTRIES: usize = 1024;
const POSTGRES_TIMESTAMP_DIGITS: u16 = 6;

/// PostgreSQL `TIMESTAMPTZ` stores microseconds, while `chrono::Utc::now()`
/// may contain nanoseconds. Health timestamps are optimistic-concurrency
/// tokens, so both the local mirror and SQL parameters must use the same
/// precision or a following CAS can never match the row just written.
fn postgres_timestamp(value: DateTime<Utc>) -> DateTime<Utc> {
    value.trunc_subsecs(POSTGRES_TIMESTAMP_DIGITS)
}

/// Persistent, account-scoped health snapshot used by routing and the admin
/// console. `priority` is intentionally not part of this structure: it is an
/// administrator-owned configuration value on `accounts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountHealth {
    pub status: String,
    pub reason: Option<String>,
    pub penalty: i32,
    pub consecutive_failures: i32,
    pub success_count: i64,
    pub failure_count: i64,
    pub avg_latency_ms: Option<i64>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_failure_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub generation: i64,
    /// Account configuration version observed when this local snapshot was
    /// hydrated. Runtime events use it to reject results from an old endpoint
    /// or credential even when the request completes after the update.
    pub configuration_updated_at: Option<DateTime<Utc>>,
}

impl Default for AccountHealth {
    fn default() -> Self {
        Self {
            status: ACCOUNT_HEALTH_UNKNOWN.to_string(),
            reason: None,
            penalty: 0,
            consecutive_failures: 0,
            success_count: 0,
            failure_count: 0,
            avg_latency_ms: None,
            last_success_at: None,
            last_failure_at: None,
            updated_at: postgres_timestamp(Utc::now()),
            generation: 0,
            configuration_updated_at: None,
        }
    }
}

impl AccountHealth {
    pub fn from_account(account: &Account) -> Self {
        Self {
            status: if account.health_status.is_empty() {
                ACCOUNT_HEALTH_UNKNOWN.to_string()
            } else {
                account.health_status.clone()
            },
            reason: account.health_reason.clone(),
            penalty: account.health_penalty,
            consecutive_failures: account.health_consecutive_failures,
            success_count: account.health_success_count,
            failure_count: account.health_failure_count,
            avg_latency_ms: account.health_avg_latency_ms,
            last_success_at: account.health_last_success_at.map(postgres_timestamp),
            last_failure_at: account.health_last_failure_at.map(postgres_timestamp),
            updated_at: postgres_timestamp(account.health_updated_at),
            generation: account.health_generation,
            configuration_updated_at: Some(postgres_timestamp(account.updated_at)),
        }
    }

    pub fn is_routable(&self) -> bool {
        self.status != ACCOUNT_UNHEALTHY
    }

    fn update_average_latency(&mut self, latency_ms: i64) {
        let latency_ms = latency_ms.max(0);
        self.avg_latency_ms = Some(match self.avg_latency_ms {
            None => latency_ms,
            Some(current) => ((current as f64 * 0.7) + (latency_ms as f64 * 0.3)) as i64,
        });
    }

    fn mark_success(&mut self, latency_ms: i64) {
        self.mark_success_at(latency_ms, postgres_timestamp(Utc::now()));
    }

    fn mark_success_at(&mut self, latency_ms: i64, occurred_at: DateTime<Utc>) {
        let occurred_at = postgres_timestamp(occurred_at);
        self.success_count = self.success_count.saturating_add(1);
        self.consecutive_failures = 0;
        self.penalty = 0;
        self.status = ACCOUNT_HEALTHY.to_string();
        self.reason = None;
        self.update_average_latency(latency_ms);
        self.last_success_at = Some(
            self.last_success_at
                .map_or(occurred_at, |previous| previous.max(occurred_at)),
        );
        self.updated_at = occurred_at;
    }

    fn mark_failure(&mut self, reason: &str, hard_failure: bool) {
        self.mark_failure_at(reason, hard_failure, postgres_timestamp(Utc::now()));
    }

    fn mark_failure_at(&mut self, reason: &str, hard_failure: bool, occurred_at: DateTime<Utc>) {
        let occurred_at = postgres_timestamp(occurred_at);
        self.failure_count = self.failure_count.saturating_add(1);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_failure_at = Some(
            self.last_failure_at
                .map_or(occurred_at, |previous| previous.max(occurred_at)),
        );
        self.reason = Some(reason.to_string());
        if hard_failure || self.consecutive_failures >= 3 {
            self.status = ACCOUNT_UNHEALTHY.to_string();
            self.penalty = 100;
        } else {
            self.status = ACCOUNT_DEGRADED.to_string();
            self.penalty = (self.penalty + 35).min(99);
        }
        self.updated_at = occurred_at;
    }

    /// Keep the local runtime version aligned with the database merge query.
    /// Runtime writes advance `health_updated_at` by one microsecond even when
    /// the event timestamp is unchanged, so a following probe can use the
    /// locally observed version after the runtime event is persisted.
    fn advance_runtime_timestamp(
        &mut self,
        previous_updated_at: DateTime<Utc>,
        occurred_at: DateTime<Utc>,
    ) {
        self.updated_at = previous_updated_at.max(postgres_timestamp(occurred_at))
            + chrono::Duration::microseconds(1);
    }
}

/// Provider 健康状态
#[derive(Debug, Clone)]
pub struct ProviderHealth {
    /// Provider 名称
    pub name: String,
    /// 是否健康
    pub healthy: bool,
    /// 平均延迟（毫秒）
    pub avg_latency_ms: u64,
    /// 成功率（百分比，0-100）
    pub success_rate: f64,
    /// 总请求数
    pub total_requests: u64,
    /// 成功请求数
    pub success_requests: u64,
    /// 失败请求数
    pub failed_requests: u64,
    /// 最后更新时间
    pub last_updated: Instant,
    /// 最后成功时间
    pub last_success_at: Option<Instant>,
    /// 最后失败时间
    pub last_failure_at: Option<Instant>,
}

impl ProviderHealth {
    /// 创建新的健康状态
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            healthy: true,
            avg_latency_ms: 0,
            success_rate: 100.0,
            total_requests: 0,
            success_requests: 0,
            failed_requests: 0,
            last_updated: Instant::now(),
            last_success_at: None,
            last_failure_at: None,
        }
    }

    /// 记录协议级诊断成功请求。
    ///
    /// This compatibility metric is not consulted by account routing; use
    /// `record_account_success` for operational routing health.
    pub fn record_success(&mut self, latency_ms: u64) {
        self.total_requests += 1;
        self.success_requests += 1;
        self.last_success_at = Some(Instant::now());

        // 更新平均延迟（指数加权移动平均 EWMA）
        // alpha = 0.3 表示新样本占 30%，历史占 70%
        // 这样对突发延迟更敏感，同时保持一定平滑性
        const ALPHA: f64 = 0.3;
        if self.avg_latency_ms == 0 {
            self.avg_latency_ms = latency_ms;
        } else {
            let current = self.avg_latency_ms as f64;
            let new = latency_ms as f64;
            self.avg_latency_ms = (ALPHA * new + (1.0 - ALPHA) * current) as u64;
        }

        self.update_success_rate();
        self.last_updated = Instant::now();
    }

    /// 记录协议级诊断失败请求。
    ///
    /// This compatibility metric is not consulted by account routing; use
    /// `record_account_failure` for operational routing health.
    pub fn record_failure(&mut self) {
        self.total_requests += 1;
        self.failed_requests += 1;
        self.last_failure_at = Some(Instant::now());

        self.update_success_rate();
        self.last_updated = Instant::now();
    }

    /// 更新成功率
    fn update_success_rate(&mut self) {
        if self.total_requests > 0 {
            self.success_rate = (self.success_requests as f64 / self.total_requests as f64) * 100.0;
        }

        // 如果成功率低于阈值，标记为不健康
        if self.success_rate < 50.0 && self.total_requests >= 10 {
            self.healthy = false;
        } else if self.success_rate >= 80.0 {
            self.healthy = true;
        }
    }

    /// 获取健康评分（0-100）
    pub fn health_score(&self) -> u64 {
        if !self.healthy {
            return 0;
        }

        // 基于成功率和延迟计算评分
        let latency_score = if self.avg_latency_ms < 100 {
            100
        } else if self.avg_latency_ms < 500 {
            80
        } else if self.avg_latency_ms < 1000 {
            60
        } else {
            40
        };

        ((self.success_rate as u64 * 60 + latency_score as u64 * 40) / 100).min(100)
    }
}

/// Provider 健康状态存储
pub struct ProviderHealthStore {
    health_map: DashMap<String, ProviderHealth>,
    /// Account-scoped health used by routing. The legacy provider map is kept
    /// for gateway diagnostics, but is no longer used to select targets.
    account_health_map: Arc<DashMap<Uuid, AccountHealth>>,
    /// IDs known to come from the database. This lets us reject synthetic
    /// fallback IDs without dropping a real account event after a reset has
    /// temporarily removed its health snapshot.
    tracked_account_ids: Arc<DashSet<Uuid>>,
    account_db: OnceLock<Arc<DbRouter>>,
    /// Runtime events waiting for persistence. Events are kept in order per
    /// account so the database can atomically merge concurrent replicas rather
    /// than overwrite counters with stale in-memory snapshots.
    pending_account_health: Arc<DashMap<Uuid, PendingAccountHealth>>,
    account_health_notify: Arc<Notify>,
    persistence_worker_started: AtomicBool,
    next_health_event_id: AtomicU64,
    /// Serializes local persistence with an administrator reset. A reset must
    /// wait for an in-flight event and prevent the worker from starting the
    /// next queued event until the reset has fenced the old queue.
    persistence_lock: Arc<Mutex<()>>,
    /// 全局 fallback 计数器
    fallback_count: AtomicU64,
}

#[derive(Clone)]
struct PendingAccountHealth {
    events: VecDeque<PendingAccountHealthEvent>,
}

#[derive(Clone)]
enum PendingAccountHealthEvent {
    Success {
        id: u64,
        generation: i64,
        configuration_updated_at: Option<DateTime<Utc>>,
        latency_ms: i64,
        occurred_at: DateTime<Utc>,
    },
    Failure {
        id: u64,
        generation: i64,
        configuration_updated_at: Option<DateTime<Utc>>,
        reason: String,
        hard_failure: bool,
        occurred_at: DateTime<Utc>,
    },
    Probe {
        id: u64,
        expected_updated_at: DateTime<Utc>,
        expected_health_updated_at: DateTime<Utc>,
        expected_health_generation: i64,
        probed_at: DateTime<Utc>,
        latency_ms: i64,
        status: String,
        error_code: Option<String>,
        health: AccountHealth,
    },
}

impl PendingAccountHealthEvent {
    fn id(&self) -> u64 {
        match self {
            Self::Success { id, .. } | Self::Failure { id, .. } | Self::Probe { id, .. } => *id,
        }
    }

    fn occurred_at(&self) -> DateTime<Utc> {
        match self {
            Self::Success { occurred_at, .. } | Self::Failure { occurred_at, .. } => *occurred_at,
            Self::Probe { probed_at, .. } => *probed_at,
        }
    }

    fn rebase_generation(&mut self, generation: i64) -> bool {
        match self {
            Self::Success {
                generation: event_generation,
                ..
            }
            | Self::Failure {
                generation: event_generation,
                ..
            } => {
                *event_generation = generation;
                true
            }
            Self::Probe { .. } => false,
        }
    }
}

impl std::fmt::Debug for ProviderHealthStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderHealthStore")
            .field("health_map", &self.health_map)
            .field("account_health_map", &self.account_health_map)
            .field("tracked_account_ids", &self.tracked_account_ids.len())
            .field("account_db", &self.account_db.get().map(|_| "DbRouter"))
            .field("pending_account_health", &self.pending_account_health.len())
            .field("next_health_event_id", &self.next_health_event_id)
            .field("fallback_count", &self.fallback_count)
            .finish()
    }
}

impl Default for ProviderHealthStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderHealthStore {
    /// 创建新的健康状态存储
    pub fn new() -> Self {
        Self {
            health_map: DashMap::new(),
            account_health_map: Arc::new(DashMap::new()),
            tracked_account_ids: Arc::new(DashSet::new()),
            account_db: OnceLock::new(),
            pending_account_health: Arc::new(DashMap::new()),
            account_health_notify: Arc::new(Notify::new()),
            persistence_worker_started: AtomicBool::new(false),
            next_health_event_id: AtomicU64::new(1),
            persistence_lock: Arc::new(Mutex::new(())),
            fallback_count: AtomicU64::new(0),
        }
    }

    /// Create a health store that persists account transitions to the writer.
    pub fn with_account_db(pool: Arc<DbRouter>) -> Self {
        let store = Self::new();
        let _ = store.account_db.set(pool);
        store.start_persistence_worker();
        store
    }

    /// Attach the account persistence backend once during application setup.
    pub fn attach_account_db(&self, pool: Arc<DbRouter>) {
        let _ = self.account_db.set(pool);
        self.start_persistence_worker();
    }

    pub fn account_health_for(&self, account: &Account) -> AccountHealth {
        let persisted_updated_at = postgres_timestamp(account.health_updated_at);
        let persisted_configuration_updated_at = postgres_timestamp(account.updated_at);
        self.account_health_map
            .get(&account.id)
            .filter(|health| {
                (health.updated_at > persisted_updated_at
                    || (health.updated_at == persisted_updated_at
                        && health.generation >= account.health_generation))
                    && health
                        .configuration_updated_at
                        .is_none_or(|updated_at| updated_at == persisted_configuration_updated_at)
            })
            .map(|health| health.clone())
            .unwrap_or_else(|| AccountHealth::from_account(account))
    }

    /// Seed the in-memory state from the persisted account snapshot before a
    /// routing or probe event mutates it. This preserves consecutive-failure
    /// thresholds across restarts and across requests that are routed from a
    /// freshly loaded database row.
    pub fn hydrate_account_health(&self, account: &Account) -> AccountHealth {
        self.tracked_account_ids.insert(account.id);
        let persisted = AccountHealth::from_account(account);
        match self.account_health_map.entry(account.id) {
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(persisted.clone());
                persisted
            }
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                let local = entry.get();
                let configuration_changed = local
                    .configuration_updated_at
                    .is_some_and(|updated_at| updated_at != postgres_timestamp(account.updated_at));
                if configuration_changed
                    || entry.get().updated_at < persisted.updated_at
                    || (entry.get().updated_at == persisted.updated_at
                        && entry.get().generation < persisted.generation)
                {
                    entry.insert(persisted.clone());
                    persisted
                } else {
                    entry.get().clone()
                }
            }
        }
    }

    pub fn account_is_routable(&self, account: &Account) -> bool {
        account.enabled && self.account_health_for(account).is_routable()
    }

    pub fn account_health(&self, account_id: &Uuid) -> Option<AccountHealth> {
        self.account_health_map
            .get(account_id)
            .map(|health| health.clone())
    }

    pub fn discard_account_health_override(&self, account_id: &Uuid) {
        self.account_health_map.remove(account_id);
    }

    /// Forget an account entirely after it has been deleted. Unlike discarding
    /// a stale probe override, this also removes the ID from the real-account
    /// allowlist used to reject synthetic fallback IDs.
    pub fn forget_account_health(&self, account_id: &Uuid) {
        self.account_health_map.remove(account_id);
        self.tracked_account_ids.remove(account_id);
        self.pending_account_health.remove(account_id);
    }

    /// Drop a stale probe result only when no newer live/probe event has
    /// replaced it in memory.
    pub fn discard_account_health_override_if_current(
        &self,
        account_id: &Uuid,
        updated_at: DateTime<Utc>,
    ) {
        if self
            .account_health_map
            .get(account_id)
            .is_some_and(|health| health.updated_at == updated_at)
        {
            self.account_health_map.remove(account_id);
        }
    }

    fn start_persistence_worker(&self) {
        let Some(pool) = self.account_db.get().cloned() else {
            return;
        };
        if self
            .persistence_worker_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let pending = Arc::clone(&self.pending_account_health);
        let notify = Arc::clone(&self.account_health_notify);
        let persistence_lock = Arc::clone(&self.persistence_lock);
        let account_health_map = Arc::clone(&self.account_health_map);
        let tracked_account_ids = Arc::clone(&self.tracked_account_ids);
        let Ok(handle) = Handle::try_current() else {
            self.persistence_worker_started
                .store(false, Ordering::Release);
            return;
        };
        handle.spawn(async move {
            loop {
                if pending.is_empty() {
                    notify.notified().await;
                    continue;
                }

                let ids: Vec<Uuid> = pending.iter().map(|entry| *entry.key()).collect();
                let mut retry_after_error = false;
                for account_id in ids {
                    let _persistence_guard = persistence_lock.lock().await;
                    let Some(event) = pending
                        .get(&account_id)
                        .and_then(|entry| entry.events.front().cloned())
                    else {
                        continue;
                    };

                    let event_id = event.id();
                    let result = match &event {
                        PendingAccountHealthEvent::Success {
                            generation,
                            latency_ms,
                            occurred_at,
                            ..
                        } => {
                            Account::record_runtime_success(
                                pool.write_conn(),
                                account_id,
                                *generation,
                                *latency_ms,
                                *occurred_at,
                            )
                            .await
                        }
                        PendingAccountHealthEvent::Failure {
                            generation,
                            reason,
                            hard_failure,
                            occurred_at,
                            ..
                        } => {
                            Account::record_runtime_failure(
                                pool.write_conn(),
                                account_id,
                                *generation,
                                reason,
                                *hard_failure,
                                *occurred_at,
                            )
                            .await
                        }
                        PendingAccountHealthEvent::Probe {
                            expected_updated_at,
                            expected_health_updated_at,
                            expected_health_generation,
                            probed_at,
                            latency_ms,
                            status,
                            error_code,
                            health,
                            ..
                        } => {
                            Account::record_probe_snapshot_if_config_current(
                                pool.write_conn(),
                                account_id,
                                *expected_updated_at,
                                *expected_health_updated_at,
                                *expected_health_generation,
                                *probed_at,
                                *latency_ms,
                                status,
                                error_code.as_deref(),
                                &health.status,
                                health.reason.as_deref(),
                                health.penalty,
                                health.consecutive_failures,
                                health.success_count,
                                health.failure_count,
                                health.avg_latency_ms,
                                health.last_success_at,
                                health.last_failure_at,
                            )
                            .await
                        }
                    };

                    match result {
                        Ok(persisted) => {
                            if persisted {
                                remove_pending_event(&pending, account_id, event_id);
                                continue;
                            }

                            // A false CAS is normal when another replica has
                            // advanced the health generation. Probe snapshots
                            // are deliberately stale and can be discarded, but
                            // a runtime event may have completed after that
                            // transition and must be rebased instead of being
                            // silently lost.
                            match &event {
                                PendingAccountHealthEvent::Probe { health, .. } => {
                                    discard_account_health_override_if_current_map(
                                        &account_health_map,
                                        &account_id,
                                        health.updated_at,
                                    );
                                    remove_pending_event(&pending, account_id, event_id);
                                }
                                PendingAccountHealthEvent::Success { .. }
                                | PendingAccountHealthEvent::Failure { .. } => {
                                    match Account::find_by_id(pool.write_conn(), account_id).await {
                                        Ok(Some(account)) => {
                                            let current = AccountHealth::from_account(&account);
                                            if runtime_event_configuration_is_stale(
                                                &event,
                                                current.configuration_updated_at,
                                            ) {
                                                // A configuration update fences every
                                                // event started against the old
                                                // endpoint/key, even if it completes
                                                // after the update timestamp.
                                                account_health_map.insert(account_id, current);
                                                remove_pending_event(
                                                    &pending,
                                                    account_id,
                                                    event_id,
                                                );
                                            } else if event.occurred_at() <= current.updated_at {
                                                // The event happened before the
                                                // external probe/reset/config
                                                // fence. Drop it and converge
                                                // the local override to the
                                                // writer snapshot when no newer
                                                // local event exists.
                                                reconcile_stale_account_health(
                                                    &account_health_map,
                                                    account_id,
                                                    &current,
                                                );
                                                remove_pending_event(
                                                    &pending,
                                                    account_id,
                                                    event_id,
                                                );
                                            } else if rebase_pending_runtime_event(
                                                &pending,
                                                account_id,
                                                event_id,
                                                current.generation,
                                            ) {
                                                rebase_local_account_health_generation(
                                                    &account_health_map,
                                                    account_id,
                                                    current.generation,
                                                );
                                                retry_after_error = true;
                                                tracing::debug!(
                                                    %account_id,
                                                    event_id,
                                                    generation = current.generation,
                                                    "rebased account health event after a concurrent generation change"
                                                );
                                            }
                                        }
                                        Ok(None) => {
                                            // A remote deletion is terminal for
                                            // both the pending event and local
                                            // allowlist state.
                                            remove_pending_event(&pending, account_id, event_id);
                                            account_health_map.remove(&account_id);
                                            tracked_account_ids.remove(&account_id);
                                        }
                                        Err(error) => {
                                            tracing::warn!(
                                                %account_id,
                                                %error,
                                                "failed to reload account health after a generation conflict"
                                            );
                                            retry_after_error = true;
                                        }
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                %account_id,
                                %error,
                                "failed to persist account health event"
                            );
                            retry_after_error = true;
                        }
                    }
                }

                if retry_after_error {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        });
    }

    fn enqueue_account_event(&self, account_id: Uuid, event: PendingAccountHealthEvent) {
        if self.account_db.get().is_none() {
            return;
        }
        let mut dropped = false;
        self.pending_account_health
            .entry(account_id)
            .and_modify(|pending| {
                let mut enqueue = true;
                if pending.events.len() >= MAX_PENDING_ACCOUNT_HEALTH_EVENTS {
                    // Preserve probe snapshots whenever possible: runtime
                    // events can be dropped under sustained outage, but
                    // dropping the generation fence would strand all newer
                    // events.
                    if let Some(index) = pending.events.iter().position(|queued| {
                        !matches!(queued, PendingAccountHealthEvent::Probe { .. })
                    }) {
                        pending.events.remove(index);
                    } else if matches!(&event, PendingAccountHealthEvent::Probe { .. }) {
                        // Do not remove the first probe: every later probe is
                        // fenced to the generation produced by its
                        // predecessor. Dropping the oldest one would make the
                        // whole remaining probe queue impossible to persist.
                        enqueue = false;
                    } else {
                        enqueue = false;
                    }
                    dropped = true;
                }
                if enqueue {
                    pending.events.push_back(event.clone());
                }
            })
            .or_insert_with(|| PendingAccountHealth {
                events: VecDeque::from([event]),
            });
        if dropped {
            tracing::warn!(
                %account_id,
                max_events = MAX_PENDING_ACCOUNT_HEALTH_EVENTS,
                "dropping an account health event after the persistence backlog reached its limit"
            );
        }
        self.account_health_notify.notify_one();
        self.start_persistence_worker();
    }

    fn bound_ephemeral_health_map(&self, account_id: Uuid) {
        if self.account_db.get().is_some()
            || self.account_health_map.contains_key(&account_id)
            || self.account_health_map.len() < MAX_EPHEMERAL_ACCOUNT_HEALTH_ENTRIES
        {
            return;
        }
        // Synthetic IDs are not routable from this store, so clearing the
        // ephemeral cache is preferable to retaining an unbounded set of
        // entries. Database-backed stores take the other branch above and do
        // not need this eviction policy.
        self.account_health_map.clear();
    }

    /// Record a successful live request for a specific account.
    pub fn record_account_success(&self, account_id: Uuid, latency_ms: u64) {
        // In database-backed mode only hydrated, real account IDs may update
        // health. This prevents random IDs from the no-account fallback path
        // from creating an unbounded in-memory map.
        if self.account_db.get().is_some() && !self.tracked_account_ids.contains(&account_id) {
            return;
        }
        self.bound_ephemeral_health_map(account_id);
        let occurred_at = postgres_timestamp(Utc::now());
        let mut health = self.account_health_map.entry(account_id).or_default();
        let generation = health.generation;
        let previous_updated_at = health.updated_at;
        health.mark_success_at(latency_ms as i64, occurred_at);
        health.advance_runtime_timestamp(previous_updated_at, occurred_at);
        let event_id = self.next_health_event_id.fetch_add(1, Ordering::SeqCst);
        self.enqueue_account_event(
            account_id,
            PendingAccountHealthEvent::Success {
                id: event_id,
                generation,
                configuration_updated_at: health.configuration_updated_at,
                latency_ms: latency_ms as i64,
                occurred_at,
            },
        );
    }

    /// Record a failed live request. Hard authentication/configuration errors
    /// quarantine immediately; transient failures degrade first and quarantine
    /// after three consecutive failures.
    pub fn record_account_failure(&self, account_id: Uuid, error: &KeyComputeError) {
        let Some((reason, hard_failure)) = classify_runtime_error(error) else {
            // User/request validation failures do not say anything about the
            // upstream account and must not poison its routing health.
            return;
        };
        if self.account_db.get().is_some() && !self.tracked_account_ids.contains(&account_id) {
            return;
        }
        self.bound_ephemeral_health_map(account_id);
        let occurred_at = postgres_timestamp(Utc::now());
        let mut health = self.account_health_map.entry(account_id).or_default();
        let generation = health.generation;
        let previous_updated_at = health.updated_at;
        health.mark_failure_at(reason, hard_failure, occurred_at);
        health.advance_runtime_timestamp(previous_updated_at, occurred_at);
        let event_id = self.next_health_event_id.fetch_add(1, Ordering::SeqCst);
        self.enqueue_account_event(
            account_id,
            PendingAccountHealthEvent::Failure {
                id: event_id,
                generation,
                configuration_updated_at: health.configuration_updated_at,
                reason: reason.to_string(),
                hard_failure,
                occurred_at,
            },
        );
    }

    /// Apply an explicit admin/automatic probe result to account health.
    pub fn record_account_probe(
        &self,
        account_id: Uuid,
        success: bool,
        latency_ms: i64,
        error_code: Option<&str>,
    ) -> AccountHealth {
        if self.account_db.get().is_some() {
            self.tracked_account_ids.insert(account_id);
        }
        let mut health = self.account_health_map.entry(account_id).or_default();
        let previous_updated_at = health.updated_at;
        if success {
            health.mark_success(latency_ms);
        } else {
            let (reason, hard_failure) = classify_probe_error(error_code);
            health.mark_failure(reason, hard_failure);
        }
        health.updated_at = previous_updated_at.max(health.updated_at);
        health.generation = health.generation.saturating_add(1);
        health.clone()
    }

    /// Apply a probe only when the health snapshot observed at probe start is
    /// still current. A live request completing during the probe therefore
    /// wins instead of being replaced by the late probe result.
    pub fn record_account_probe_if_current(
        &self,
        account_id: Uuid,
        expected_updated_at: DateTime<Utc>,
        success: bool,
        latency_ms: i64,
        error_code: Option<&str>,
    ) -> Option<AccountHealth> {
        let expected_updated_at = postgres_timestamp(expected_updated_at);
        let mut health = self.account_health_map.get_mut(&account_id)?;
        if health.updated_at != expected_updated_at {
            return None;
        }
        let previous_updated_at = health.updated_at;
        if success {
            health.mark_success(latency_ms);
        } else {
            let (reason, hard_failure) = classify_probe_error(error_code);
            health.mark_failure(reason, hard_failure);
        }
        health.updated_at = previous_updated_at.max(health.updated_at);
        health.generation = health.generation.saturating_add(1);
        Some(health.clone())
    }

    /// Apply a probe result and reserve its persistence position atomically
    /// with the in-memory transition. The caller may still persist the
    /// snapshot synchronously; the queued event acts as a retry when that
    /// write fails (and becomes a harmless generation-fenced no-op when the
    /// synchronous write succeeds first).
    #[allow(clippy::too_many_arguments)]
    pub fn record_account_probe_if_current_and_enqueue(
        &self,
        account_id: Uuid,
        expected_updated_at: DateTime<Utc>,
        expected_health_updated_at: DateTime<Utc>,
        expected_health_generation: i64,
        probed_at: DateTime<Utc>,
        latency_ms: i64,
        status: &str,
        error_code: Option<&str>,
        success: bool,
    ) -> Option<AccountHealth> {
        let expected_updated_at = postgres_timestamp(expected_updated_at);
        let expected_health_updated_at = postgres_timestamp(expected_health_updated_at);
        let probed_at = postgres_timestamp(probed_at);
        let mut health = self.account_health_map.get_mut(&account_id)?;
        if health.updated_at != expected_health_updated_at
            || health.generation != expected_health_generation
        {
            return None;
        }
        let effective_probed_at = health.updated_at.max(probed_at);
        if success {
            health.mark_success_at(latency_ms, effective_probed_at);
        } else {
            let (reason, hard_failure) = classify_probe_error(error_code);
            health.mark_failure_at(reason, hard_failure, effective_probed_at);
        }
        health.updated_at = effective_probed_at;
        health.generation = health.generation.saturating_add(1);
        let snapshot = health.clone();
        let event_id = self.next_health_event_id.fetch_add(1, Ordering::SeqCst);
        self.enqueue_account_event(
            account_id,
            PendingAccountHealthEvent::Probe {
                id: event_id,
                expected_updated_at,
                expected_health_updated_at,
                expected_health_generation,
                probed_at,
                latency_ms,
                status: status.to_string(),
                error_code: error_code.map(str::to_string),
                health: snapshot.clone(),
            },
        );
        Some(snapshot)
    }

    pub async fn reset_account_health(
        &self,
        account_id: Uuid,
    ) -> Result<(), keycompute_db::DbError> {
        let _persistence_guard = self.persistence_lock.lock().await;
        let reset_event_boundary = self.next_health_event_id.load(Ordering::SeqCst);
        let Some(pool) = self.account_db.get().cloned() else {
            let reset_completed_at = postgres_timestamp(Utc::now());
            self.account_health_map.remove(&account_id);
            remove_pending_events_before_id(
                &self.pending_account_health,
                account_id,
                reset_event_boundary,
            );
            remove_pending_events_before(
                &self.pending_account_health,
                account_id,
                reset_completed_at,
            );
            return Ok(());
        };

        // Use the exact row returned by PostgreSQL. In particular,
        // `health_updated_at` is the CAS token generated by the writer's
        // `NOW()` and cannot be reconstructed reliably with a second local
        // clock read.
        let reset_account = Account::reset_health_snapshot(pool.write_conn(), account_id).await?;
        if let Some(account) = reset_account {
            let reset_health = AccountHealth::from_account(&account);
            let reset_completed_at = reset_health.updated_at;
            self.account_health_map.insert(account_id, reset_health);
            self.tracked_account_ids.insert(account_id);
            remove_pending_events_before_id(
                &self.pending_account_health,
                account_id,
                reset_event_boundary,
            );
            remove_pending_events_before(
                &self.pending_account_health,
                account_id,
                reset_completed_at,
            );
        } else {
            // The account may have been deleted concurrently. Remove all
            // local state so a synthetic ID cannot enqueue more events.
            self.forget_account_health(&account_id);
        }
        Ok(())
    }

    pub async fn reset_all_account_health(&self) -> Result<(), keycompute_db::DbError> {
        let _persistence_guard = self.persistence_lock.lock().await;
        let reset_event_boundary = self.next_health_event_id.load(Ordering::SeqCst);
        let Some(pool) = self.account_db.get().cloned() else {
            let reset_completed_at = postgres_timestamp(Utc::now());
            self.account_health_map.clear();
            let ids: Vec<Uuid> = self
                .pending_account_health
                .iter()
                .map(|entry| *entry.key())
                .collect();
            for account_id in ids {
                remove_pending_events_before_id(
                    &self.pending_account_health,
                    account_id,
                    reset_event_boundary,
                );
                remove_pending_events_before(
                    &self.pending_account_health,
                    account_id,
                    reset_completed_at,
                );
            }
            return Ok(());
        };

        // Rehydrate every local entry from the rows actually updated by the
        // writer. This keeps both generation and timestamp CAS tokens exact,
        // including accounts that were not hydrated before the reset.
        let reset_accounts = Account::reset_all_health_snapshot(pool.write_conn()).await?;
        self.account_health_map.clear();
        self.tracked_account_ids.clear();
        let reset_completed_at = reset_accounts
            .iter()
            .map(|account| account.health_updated_at)
            .max()
            .unwrap_or_else(|| postgres_timestamp(Utc::now()));
        for account in reset_accounts {
            self.tracked_account_ids.insert(account.id);
            self.account_health_map
                .insert(account.id, AccountHealth::from_account(&account));
        }
        let ids: Vec<Uuid> = self
            .pending_account_health
            .iter()
            .map(|entry| *entry.key())
            .collect();
        for account_id in ids {
            remove_pending_events_before_id(
                &self.pending_account_health,
                account_id,
                reset_event_boundary,
            );
            remove_pending_events_before(
                &self.pending_account_health,
                account_id,
                reset_completed_at,
            );
        }
        Ok(())
    }

    /// 记录成功请求
    pub fn record_success(&self, provider: impl AsRef<str>, latency_ms: u64) {
        let provider = provider.as_ref();

        self.health_map
            .entry(provider.to_string())
            .and_modify(|health| health.record_success(latency_ms))
            .or_insert_with(|| {
                let mut health = ProviderHealth::new(provider);
                health.record_success(latency_ms);
                health
            });

        tracing::debug!(
            provider = %provider,
            latency_ms = latency_ms,
            "Provider request succeeded"
        );
    }

    /// 记录失败请求
    pub fn record_failure(&self, provider: impl AsRef<str>) {
        let provider = provider.as_ref();

        self.health_map
            .entry(provider.to_string())
            .and_modify(|health| health.record_failure())
            .or_insert_with(|| {
                let mut health = ProviderHealth::new(provider);
                health.record_failure();
                health
            });

        tracing::warn!(provider = %provider, "Provider request failed");
    }

    /// 获取协议级诊断状态（不参与账号路由）
    pub fn get_health(&self, provider: &str) -> Option<ProviderHealth> {
        self.health_map.get(provider).map(|h| h.clone())
    }

    /// 检查协议级诊断状态（不参与账号路由）
    pub fn is_healthy(&self, provider: &str) -> bool {
        self.health_map
            .get(provider)
            .map(|h| h.healthy)
            .unwrap_or(true) // 默认认为健康
    }

    /// 获取所有 Provider 健康状态
    pub fn all_health(&self) -> Vec<ProviderHealth> {
        self.health_map
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// 获取健康 Provider 列表
    pub fn healthy_providers(&self, providers: &[String]) -> Vec<String> {
        providers
            .iter()
            .filter(|p| self.is_healthy(p))
            .cloned()
            .collect()
    }

    /// 获取协议级兼容评分（不参与路由排序）
    pub fn get_score(&self, provider: &str) -> u64 {
        self.health_map
            .get(provider)
            .map(|h| h.health_score())
            .unwrap_or(50) // 默认中等评分
    }

    /// 更新 Provider 健康状态（手动设置）
    pub fn update_health(&self, provider: impl Into<String>, health: ProviderHealth) {
        let provider = provider.into();
        self.health_map.insert(provider, health);
    }

    /// 重置 Provider 统计
    pub fn reset_stats(&self, provider: &str) {
        self.health_map.remove(provider);
    }

    /// 清理长时间未更新的 Provider（可由后台任务调用）
    pub fn cleanup_stale(&self, max_age: Duration) {
        let now = Instant::now();
        let before = self.health_map.len();

        self.health_map
            .retain(|_, health| now.duration_since(health.last_updated) < max_age);

        let after = self.health_map.len();
        if before != after {
            tracing::debug!(
                removed = before - after,
                "Stale provider health entries cleaned up"
            );
        }
    }

    /// 记录 fallback 事件
    ///
    /// 当请求从 primary provider 切换到 fallback provider 时调用
    pub fn record_fallback(&self) {
        self.fallback_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 获取 fallback 总数
    pub fn get_fallback_count(&self) -> u64 {
        self.fallback_count.load(Ordering::Relaxed)
    }

    /// 重置 fallback 计数
    pub fn reset_fallback_count(&self) {
        self.fallback_count.store(0, Ordering::Relaxed);
    }
}

fn remove_pending_event(
    pending: &DashMap<Uuid, PendingAccountHealth>,
    account_id: Uuid,
    event_id: u64,
) {
    if let dashmap::mapref::entry::Entry::Occupied(mut entry) = pending.entry(account_id) {
        let should_remove = {
            let pending_health = entry.get_mut();
            if pending_health
                .events
                .front()
                .is_some_and(|event| event.id() == event_id)
            {
                pending_health.events.pop_front();
            }
            pending_health.events.is_empty()
        };
        if should_remove {
            entry.remove();
        }
    }
}

fn rebase_pending_runtime_event(
    pending: &DashMap<Uuid, PendingAccountHealth>,
    account_id: Uuid,
    event_id: u64,
    generation: i64,
) -> bool {
    let Some(mut entry) = pending.get_mut(&account_id) else {
        return false;
    };
    let Some(event) = entry.events.front_mut() else {
        return false;
    };
    if event.id() != event_id {
        return false;
    }
    event.rebase_generation(generation)
}

fn reconcile_stale_account_health(
    health_map: &DashMap<Uuid, AccountHealth>,
    account_id: Uuid,
    persisted: &AccountHealth,
) {
    let Some(mut local) = health_map.get_mut(&account_id) else {
        return;
    };
    if local.updated_at <= persisted.updated_at {
        drop(local);
        health_map.remove(&account_id);
    } else if local.generation < persisted.generation {
        local.generation = persisted.generation;
    }
}

fn rebase_local_account_health_generation(
    health_map: &DashMap<Uuid, AccountHealth>,
    account_id: Uuid,
    generation: i64,
) {
    if let Some(mut local) = health_map.get_mut(&account_id)
        && local.generation < generation
    {
        local.generation = generation;
    }
}

fn runtime_event_configuration_is_stale(
    event: &PendingAccountHealthEvent,
    current_configuration_updated_at: Option<DateTime<Utc>>,
) -> bool {
    let observed_configuration_updated_at = match event {
        PendingAccountHealthEvent::Success {
            configuration_updated_at,
            ..
        }
        | PendingAccountHealthEvent::Failure {
            configuration_updated_at,
            ..
        } => *configuration_updated_at,
        PendingAccountHealthEvent::Probe { .. } => return false,
    };
    observed_configuration_updated_at.is_some()
        && observed_configuration_updated_at != current_configuration_updated_at
}

fn discard_account_health_override_if_current_map(
    health_map: &DashMap<Uuid, AccountHealth>,
    account_id: &Uuid,
    updated_at: DateTime<Utc>,
) {
    if health_map
        .get(account_id)
        .is_some_and(|health| health.updated_at == updated_at)
    {
        health_map.remove(account_id);
    }
}

fn remove_pending_events_before(
    pending: &DashMap<Uuid, PendingAccountHealth>,
    account_id: Uuid,
    cutoff: DateTime<Utc>,
) {
    let should_remove = pending.get_mut(&account_id).is_some_and(|mut entry| {
        // Keep events at the exact cutoff. They may have been enqueued after
        // the reset but share PostgreSQL's microsecond timestamp; generation
        // fencing will discard an older event while preserving a new one.
        entry.events.retain(|event| event.occurred_at() >= cutoff);
        entry.events.is_empty()
    });
    if should_remove {
        pending.remove(&account_id);
    }
}

fn remove_pending_events_before_id(
    pending: &DashMap<Uuid, PendingAccountHealth>,
    account_id: Uuid,
    boundary: u64,
) {
    let should_remove = pending.get_mut(&account_id).is_some_and(|mut entry| {
        entry.events.retain(|event| event.id() >= boundary);
        entry.events.is_empty()
    });
    if should_remove {
        pending.remove(&account_id);
    }
}

fn classify_runtime_error(error: &KeyComputeError) -> Option<(&'static str, bool)> {
    match error {
        KeyComputeError::UpstreamFailure {
            status: Some(401 | 403),
            ..
        }
        | KeyComputeError::AuthError(_)
        | KeyComputeError::ConfigError(_) => Some(("configuration_or_auth_failure", true)),
        KeyComputeError::UpstreamFailure {
            status: Some(404), ..
        } => Some(("upstream_model_or_endpoint_not_found", true)),
        KeyComputeError::UpstreamFailure {
            status: Some(408 | 409 | 429),
            ..
        } => Some(("rate_limited_or_transient_request", false)),
        KeyComputeError::UpstreamFailure {
            status: Some(500..=599),
            ..
        } => Some(("transient_upstream_5xx", false)),
        KeyComputeError::UpstreamFailure {
            status: Some(400..=499),
            ..
        }
        | KeyComputeError::InvalidRequest(_)
        | KeyComputeError::ValidationError(_)
        | KeyComputeError::PermissionDenied(_)
        | KeyComputeError::NotFound(_) => None,
        KeyComputeError::RateLimitExceeded(_) => Some(("rate_limited", false)),
        KeyComputeError::ProviderTimeout(_, _)
        | KeyComputeError::NetworkError(_)
        | KeyComputeError::Timeout(_)
        | KeyComputeError::ServiceUnavailable(_) => Some(("transient_upstream_failure", false)),
        // ProviderError also represents request-shape, serialization, and
        // adapter validation failures. Without a stable upstream attribution
        // code, treating it as an account failure can quarantine every
        // fallback account for the same malformed client request.
        KeyComputeError::ProviderError(_) => None,
        KeyComputeError::UpstreamFailure { retryable, .. } => {
            Some(("upstream_failure", !retryable))
        }
        _ => None,
    }
}

fn classify_probe_error(error_code: Option<&str>) -> (&'static str, bool) {
    let Some(error_code) = error_code else {
        return ("probe_failed", false);
    };
    if matches!(
        error_code,
        "upstream_http_401"
            | "upstream_http_403"
            | "account_credentials_invalid"
            | "account_capability_invalid"
            | "probe_model_unavailable"
            | "provider_not_registered"
    ) {
        ("configuration_or_auth_failure", true)
    } else if error_code == "upstream_http_404" {
        ("upstream_model_or_endpoint_not_found", true)
    } else if error_code == "upstream_http_429" {
        ("rate_limited", false)
    } else {
        ("transient_probe_failure", false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::DatabaseConnection;

    fn disconnected_persistent_store() -> ProviderHealthStore {
        let store = ProviderHealthStore::new();
        assert!(
            store
                .account_db
                .set(DbRouter::single(DatabaseConnection::Disconnected))
                .is_ok(),
            "test account database should attach once"
        );
        // Keep the queue available for inspection instead of allowing the
        // background worker to consume it while the test is asserting order.
        store
            .persistence_worker_started
            .store(true, Ordering::Release);
        store
    }

    #[test]
    fn test_provider_health_new() {
        let health = ProviderHealth::new("openai");
        assert_eq!(health.name, "openai");
        assert!(health.healthy);
        assert_eq!(health.success_rate, 100.0);
    }

    #[test]
    fn test_record_success() {
        let mut health = ProviderHealth::new("openai");

        health.record_success(100);
        assert_eq!(health.total_requests, 1);
        assert_eq!(health.success_requests, 1);
        assert_eq!(health.avg_latency_ms, 100);

        // 使用 EWMA 算法 (alpha=0.3): 0.3 * 200 + 0.7 * 100 = 60 + 70 = 130
        health.record_success(200);
        assert_eq!(health.total_requests, 2);
        assert_eq!(health.avg_latency_ms, 130);

        // 验证 EWMA 对突发延迟更敏感
        // 第三次请求：0.3 * 300 + 0.7 * 130 = 90 + 91 = 181
        health.record_success(300);
        assert_eq!(health.total_requests, 3);
        assert_eq!(health.avg_latency_ms, 181);
    }

    #[test]
    fn test_record_failure() {
        let mut health = ProviderHealth::new("openai");

        // 10 次失败
        for _ in 0..10 {
            health.record_failure();
        }

        assert_eq!(health.total_requests, 10);
        assert_eq!(health.failed_requests, 10);
        assert!(!health.healthy); // 成功率低于 50%
    }

    #[test]
    fn test_health_score() {
        let mut health = ProviderHealth::new("openai");
        assert_eq!(health.health_score(), 100);

        // 多次失败降低评分
        for _ in 0..5 {
            health.record_failure();
        }

        assert!(health.health_score() < 100);
    }

    #[test]
    fn test_provider_health_store() {
        let store = ProviderHealthStore::new();

        store.record_success("openai", 100);
        store.record_success("openai", 200);
        store.record_failure("anthropic");

        assert!(store.is_healthy("openai"));
        // 健康评分基于成功率和延迟
        // 100% 成功率 + 低延迟(<100ms) = 100 分
        let score = store.get_score("openai");
        assert!(
            (90..=100).contains(&score),
            "Expected score around 100, got {}",
            score
        );

        // 不存在的 Provider 默认健康，评分中等
        assert!(store.is_healthy("unknown"));
        assert_eq!(store.get_score("unknown"), 50);
    }

    #[test]
    fn test_healthy_providers() {
        let store = ProviderHealthStore::new();

        // 让 anthropic 多次失败变得不健康
        for _ in 0..10 {
            store.record_failure("anthropic");
        }

        store.record_success("openai", 100);

        let providers = vec!["openai".to_string(), "anthropic".to_string()];
        let healthy = store.healthy_providers(&providers);

        assert_eq!(healthy.len(), 1);
        assert_eq!(healthy[0], "openai");
    }

    #[test]
    fn account_probe_hard_failure_quarantines_and_success_recovers() {
        let store = ProviderHealthStore::new();
        let account_id = Uuid::new_v4();

        let failed = store.record_account_probe(account_id, false, 40, Some("upstream_http_401"));
        assert_eq!(failed.status, ACCOUNT_UNHEALTHY);
        assert_eq!(failed.penalty, 100);
        assert!(!failed.is_routable());

        let recovered = store.record_account_probe(account_id, true, 20, None);
        assert_eq!(recovered.status, ACCOUNT_HEALTHY);
        assert_eq!(recovered.penalty, 0);
        assert!(recovered.is_routable());
        assert_eq!(recovered.consecutive_failures, 0);
    }

    #[test]
    fn transient_probe_failures_degrade_then_quarantine() {
        let store = ProviderHealthStore::new();
        let account_id = Uuid::new_v4();

        let first = store.record_account_probe(account_id, false, 40, None);
        assert_eq!(first.status, ACCOUNT_DEGRADED);
        assert!(first.is_routable());
        let second = store.record_account_probe(account_id, false, 40, None);
        assert_eq!(second.status, ACCOUNT_DEGRADED);
        assert!(second.is_routable());
        let third = store.record_account_probe(account_id, false, 40, None);
        assert_eq!(third.status, ACCOUNT_UNHEALTHY);
        assert!(!third.is_routable());
    }

    #[test]
    fn provider_adapter_errors_do_not_poison_account_health() {
        let store = ProviderHealthStore::new();
        let account_id = Uuid::new_v4();

        store.record_account_failure(
            account_id,
            &KeyComputeError::ProviderError("invalid request shape".to_string()),
        );

        assert!(store.account_health(&account_id).is_none());
    }

    #[test]
    fn credential_probe_failure_is_immediately_unhealthy() {
        let store = ProviderHealthStore::new();
        let account_id = Uuid::new_v4();

        let health =
            store.record_account_probe(account_id, false, 1, Some("account_credentials_invalid"));

        assert_eq!(health.status, ACCOUNT_UNHEALTHY);
        assert!(!health.is_routable());
    }

    #[test]
    fn stale_probe_does_not_replace_newer_runtime_transition() {
        let store = ProviderHealthStore::new();
        let account_id = Uuid::new_v4();

        let initial = store.record_account_probe(account_id, false, 1, None);
        let expected = initial.updated_at;
        store.record_account_success(account_id, 2);

        let stale = store.record_account_probe_if_current(
            account_id,
            expected,
            false,
            3,
            Some("upstream_http_401"),
        );

        assert!(stale.is_none());
        let current = store
            .account_health(&account_id)
            .expect("runtime health exists");
        assert_eq!(current.status, ACCOUNT_HEALTHY);
        assert_eq!(current.success_count, 1);
    }

    #[test]
    fn probe_timestamp_never_moves_health_version_backwards() {
        let store = ProviderHealthStore::new();
        let account_id = Uuid::new_v4();
        let initial = store.record_account_probe(account_id, true, 1, None);
        let older_probe_time = initial.updated_at - chrono::Duration::seconds(1);

        let snapshot = store
            .record_account_probe_if_current_and_enqueue(
                account_id,
                Utc::now(),
                initial.updated_at,
                initial.generation,
                older_probe_time,
                2,
                "succeeded",
                None,
                true,
            )
            .expect("probe should still be current");

        assert_eq!(snapshot.updated_at, initial.updated_at);
        assert_eq!(snapshot.last_success_at, initial.last_success_at);
        assert_eq!(snapshot.generation, initial.generation + 1);
    }

    #[test]
    fn probe_reservation_is_queued_before_following_runtime_event() {
        let store = disconnected_persistent_store();
        let account_id = Uuid::new_v4();
        let initial = store.record_account_probe(account_id, true, 10, None);
        let probed_at = initial.updated_at + chrono::Duration::seconds(1);

        let snapshot = store
            .record_account_probe_if_current_and_enqueue(
                account_id,
                Utc::now(),
                initial.updated_at,
                initial.generation,
                probed_at,
                20,
                "failed",
                Some("upstream_http_503"),
                false,
            )
            .expect("probe should still be current");
        store.record_account_success(account_id, 30);

        let pending = store
            .pending_account_health
            .get(&account_id)
            .expect("persistent events should be queued");
        assert_eq!(pending.events.len(), 2);
        assert!(matches!(
            pending.events.front(),
            Some(PendingAccountHealthEvent::Probe { health, .. })
                if health == &snapshot
        ));
        assert!(matches!(
            pending.events.get(1),
            Some(PendingAccountHealthEvent::Success { .. })
        ));
    }

    #[test]
    fn concurrent_runtime_events_keep_enqueue_order() {
        let store = Arc::new(disconnected_persistent_store());
        let account_id = Uuid::new_v4();
        store.record_account_probe(account_id, true, 10, None);

        let workers = (0..8)
            .map(|_| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        store.record_account_success(account_id, 10);
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().expect("runtime health worker should finish");
        }

        let pending = store
            .pending_account_health
            .get(&account_id)
            .expect("persistent events should be queued");
        let ids = pending
            .events
            .iter()
            .map(PendingAccountHealthEvent::id)
            .collect::<Vec<_>>();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[tokio::test]
    async fn failed_probe_persistence_remains_queued_for_retry() {
        let store = ProviderHealthStore::with_account_db(DbRouter::single(
            DatabaseConnection::Disconnected,
        ));
        let account_id = Uuid::new_v4();
        let initial = store.record_account_probe(account_id, true, 10, None);

        store
            .record_account_probe_if_current_and_enqueue(
                account_id,
                Utc::now(),
                initial.updated_at,
                initial.generation,
                Utc::now(),
                20,
                "failed",
                Some("upstream_http_503"),
                false,
            )
            .expect("probe should still be current");

        tokio::time::sleep(Duration::from_millis(25)).await;
        let pending = store
            .pending_account_health
            .get(&account_id)
            .expect("failed persistence must remain queued");
        assert!(matches!(
            pending.events.front(),
            Some(PendingAccountHealthEvent::Probe { .. })
        ));
    }

    #[test]
    fn persistent_runtime_backlog_is_bounded_per_account() {
        let store = disconnected_persistent_store();
        let account_id = Uuid::new_v4();
        let initial = store.record_account_probe(account_id, true, 10, None);
        store
            .record_account_probe_if_current_and_enqueue(
                account_id,
                Utc::now(),
                initial.updated_at,
                initial.generation,
                Utc::now(),
                20,
                "failed",
                Some("upstream_http_503"),
                false,
            )
            .expect("probe should still be current");

        for _ in 0..(MAX_PENDING_ACCOUNT_HEALTH_EVENTS + 25) {
            store.record_account_success(account_id, 10);
        }

        let pending = store
            .pending_account_health
            .get(&account_id)
            .expect("persistent events should remain queued");
        assert_eq!(pending.events.len(), MAX_PENDING_ACCOUNT_HEALTH_EVENTS);
        assert!(matches!(
            pending.events.front(),
            Some(PendingAccountHealthEvent::Probe { .. })
        ));
    }

    #[test]
    fn probe_backlog_overflow_keeps_the_generation_fence() {
        let store = disconnected_persistent_store();
        let account_id = Uuid::new_v4();
        let initial = store.record_account_probe(account_id, true, 10, None);
        let mut expected_updated_at = initial.updated_at;
        let mut expected_generation = initial.generation;

        for index in 0..(MAX_PENDING_ACCOUNT_HEALTH_EVENTS + 25) {
            let snapshot = store
                .record_account_probe_if_current_and_enqueue(
                    account_id,
                    Utc::now(),
                    expected_updated_at,
                    expected_generation,
                    initial.updated_at + chrono::Duration::microseconds(index as i64 + 1),
                    20,
                    "failed",
                    Some("upstream_http_503"),
                    false,
                )
                .expect("probe should remain current in the local store");
            expected_updated_at = snapshot.updated_at;
            expected_generation = snapshot.generation;
        }

        let pending = store
            .pending_account_health
            .get(&account_id)
            .expect("probe events should remain queued");
        assert_eq!(pending.events.len(), MAX_PENDING_ACCOUNT_HEALTH_EVENTS);
        assert!(matches!(
            pending.events.front(),
            Some(PendingAccountHealthEvent::Probe {
                expected_health_generation,
                ..
            }) if *expected_health_generation == initial.generation
        ));
    }

    #[test]
    fn runtime_event_generation_can_be_rebased_after_a_writer_fence() {
        let pending = DashMap::new();
        let account_id = Uuid::new_v4();
        pending.insert(
            account_id,
            PendingAccountHealth {
                events: VecDeque::from([PendingAccountHealthEvent::Success {
                    id: 7,
                    generation: 0,
                    configuration_updated_at: None,
                    latency_ms: 10,
                    occurred_at: Utc::now(),
                }]),
            },
        );

        assert!(rebase_pending_runtime_event(&pending, account_id, 7, 3));
        let event = pending
            .get(&account_id)
            .expect("rebased event should remain queued")
            .events
            .front()
            .cloned()
            .expect("rebased event should be present");
        assert!(matches!(
            event,
            PendingAccountHealthEvent::Success { generation: 3, .. }
        ));
    }

    #[test]
    fn runtime_event_from_an_old_configuration_is_fenced() {
        let old_configuration = Utc::now();
        let new_configuration = old_configuration + chrono::Duration::seconds(1);
        let event = PendingAccountHealthEvent::Success {
            id: 1,
            generation: 0,
            configuration_updated_at: Some(old_configuration),
            latency_ms: 10,
            occurred_at: new_configuration + chrono::Duration::seconds(1),
        };

        assert!(runtime_event_configuration_is_stale(
            &event,
            Some(new_configuration)
        ));
        assert!(!runtime_event_configuration_is_stale(
            &event,
            Some(old_configuration)
        ));
    }

    #[test]
    fn local_runtime_version_matches_the_persisted_microsecond_fence() {
        let store = ProviderHealthStore::new();
        let account_id = Uuid::new_v4();
        let previous_updated_at = postgres_timestamp(Utc::now()) + chrono::Duration::seconds(1);
        let initial = AccountHealth {
            updated_at: previous_updated_at,
            ..AccountHealth::default()
        };
        store.account_health_map.insert(account_id, initial);

        store.record_account_success(account_id, 10);

        let health = store
            .account_health(&account_id)
            .expect("runtime health should be recorded");
        let last_success_at = health
            .last_success_at
            .expect("success timestamp should be recorded");
        assert_eq!(
            health.updated_at,
            previous_updated_at.max(last_success_at) + chrono::Duration::microseconds(1)
        );
        assert_eq!(health.updated_at.timestamp_subsec_nanos() % 1_000, 0);
    }

    #[test]
    fn persisted_health_generation_wins_when_timestamps_are_equal() {
        let store = ProviderHealthStore::new();
        let account_id = Uuid::new_v4();
        let updated_at = postgres_timestamp(Utc::now());
        let mut account = test_account_for_health(account_id, updated_at, 2);
        let mut local = AccountHealth::from_account(&account);
        local.generation = 1;
        local.status = ACCOUNT_HEALTHY.to_string();
        store.account_health_map.insert(account_id, local);

        account.health_status = ACCOUNT_HEALTHY.to_string();
        account.health_generation = 2;
        let hydrated = store.hydrate_account_health(&account);

        assert_eq!(hydrated.generation, 2);
        assert_eq!(store.account_health(&account_id).unwrap().generation, 2);
    }

    fn test_account_for_health(
        id: Uuid,
        health_updated_at: DateTime<Utc>,
        health_generation: i64,
    ) -> Account {
        Account {
            id,
            tenant_id: Uuid::new_v4(),
            provider: "openai".to_string(),
            name: "health-test".to_string(),
            endpoint: "https://health-test.example/v1".to_string(),
            upstream_api_key_encrypted: "key".to_string(),
            upstream_api_key_preview: "key****".to_string(),
            rpm_limit: 60,
            tpm_limit: 100_000,
            priority: 0,
            enabled: true,
            models_supported: vec!["model".to_string()],
            api_capabilities: vec!["chat_completions".to_string()],
            visibility: "tenant".to_string(),
            health_status: ACCOUNT_HEALTH_UNKNOWN.to_string(),
            health_reason: None,
            health_penalty: 0,
            health_consecutive_failures: 0,
            health_success_count: 0,
            health_failure_count: 0,
            health_avg_latency_ms: None,
            health_last_success_at: None,
            health_last_failure_at: None,
            health_updated_at,
            health_generation,
            last_probe_at: None,
            last_probe_latency_ms: None,
            last_probe_status: None,
            last_probe_error_code: None,
            created_at: health_updated_at,
            updated_at: health_updated_at,
        }
    }

    #[test]
    fn ephemeral_account_health_map_is_bounded_without_database() {
        let store = ProviderHealthStore::new();
        for _ in 0..(MAX_EPHEMERAL_ACCOUNT_HEALTH_ENTRIES + 25) {
            store.record_account_success(Uuid::new_v4(), 10);
        }

        assert!(store.account_health_map.len() <= MAX_EPHEMERAL_ACCOUNT_HEALTH_ENTRIES);
    }

    #[test]
    fn unknown_database_backed_account_ids_are_not_tracked() {
        let store = disconnected_persistent_store();
        let account_id = Uuid::new_v4();

        store.record_account_success(account_id, 10);

        assert!(store.account_health(&account_id).is_none());
        assert!(store.pending_account_health.get(&account_id).is_none());
    }

    #[test]
    fn forgetting_an_account_clears_health_and_pending_events() {
        let store = disconnected_persistent_store();
        let account_id = Uuid::new_v4();
        store.record_account_probe(account_id, true, 10, None);
        store.record_account_success(account_id, 10);
        assert!(store.pending_account_health.get(&account_id).is_some());

        store.forget_account_health(&account_id);

        assert!(store.account_health(&account_id).is_none());
        assert!(store.pending_account_health.get(&account_id).is_none());
        assert!(!store.tracked_account_ids.contains(&account_id));
    }

    #[test]
    fn discarded_account_health_override_is_removed() {
        let store = ProviderHealthStore::new();
        let account_id = Uuid::new_v4();
        store.record_account_probe(account_id, false, 1, Some("upstream_http_401"));
        assert!(store.account_health(&account_id).is_some());

        store.discard_account_health_override(&account_id);

        assert!(store.account_health(&account_id).is_none());
    }

    #[tokio::test]
    async fn reset_all_account_health_clears_local_state_without_database() {
        let store = ProviderHealthStore::new();
        let account_id = Uuid::new_v4();
        store.record_account_probe(account_id, false, 1, Some("upstream_http_401"));

        store.reset_all_account_health().await.unwrap();

        assert!(store.account_health(&account_id).is_none());
    }

    #[tokio::test]
    async fn reset_account_health_is_scoped_and_keeps_new_events() {
        let pending = DashMap::new();
        let first_account = Uuid::new_v4();
        let second_account = Uuid::new_v4();
        let cutoff = Utc::now();
        let old_event = PendingAccountHealthEvent::Success {
            id: 1,
            generation: 0,
            configuration_updated_at: None,
            latency_ms: 10,
            occurred_at: cutoff - chrono::Duration::seconds(1),
        };
        let new_event = PendingAccountHealthEvent::Success {
            id: 2,
            generation: 0,
            configuration_updated_at: None,
            latency_ms: 20,
            occurred_at: cutoff + chrono::Duration::seconds(1),
        };
        pending.insert(
            first_account,
            PendingAccountHealth {
                events: VecDeque::from([old_event, new_event.clone()]),
            },
        );
        pending.insert(
            second_account,
            PendingAccountHealth {
                events: VecDeque::from([new_event.clone()]),
            },
        );

        let store = ProviderHealthStore::new();
        for entry in pending {
            store.pending_account_health.insert(entry.0, entry.1);
        }
        store
            .reset_account_health(first_account)
            .await
            .expect("in-memory account health reset should succeed");

        let first = store
            .pending_account_health
            .get(&first_account)
            .expect("new event should remain for reset account");
        assert_eq!(first.events.len(), 1);
        assert_eq!(
            first.events.front().map(PendingAccountHealthEvent::id),
            Some(2)
        );
        assert_eq!(
            store
                .pending_account_health
                .get(&second_account)
                .unwrap()
                .events
                .len(),
            1
        );
    }

    #[test]
    fn pending_event_at_reset_cutoff_is_retained_for_generation_fencing() {
        let pending = DashMap::new();
        let account_id = Uuid::new_v4();
        let cutoff = Utc::now();
        pending.insert(
            account_id,
            PendingAccountHealth {
                events: VecDeque::from([PendingAccountHealthEvent::Success {
                    id: 1,
                    generation: 0,
                    configuration_updated_at: None,
                    latency_ms: 10,
                    occurred_at: cutoff,
                }]),
            },
        );

        remove_pending_events_before(&pending, account_id, cutoff);

        assert!(pending.get(&account_id).is_some());
    }

    #[test]
    fn reset_event_id_boundary_drops_delayed_old_events() {
        let pending = DashMap::new();
        let account_id = Uuid::new_v4();
        let now = Utc::now();
        pending.insert(
            account_id,
            PendingAccountHealth {
                events: VecDeque::from([
                    PendingAccountHealthEvent::Success {
                        id: 1,
                        generation: 0,
                        configuration_updated_at: None,
                        latency_ms: 10,
                        // A delayed event can have a future wall-clock time;
                        // the monotonic ID still identifies it as pre-reset.
                        occurred_at: now + chrono::Duration::hours(1),
                    },
                    PendingAccountHealthEvent::Success {
                        id: 2,
                        generation: 1,
                        configuration_updated_at: None,
                        latency_ms: 20,
                        occurred_at: now,
                    },
                ]),
            },
        );

        remove_pending_events_before_id(&pending, account_id, 2);

        let remaining = pending
            .get(&account_id)
            .expect("post-reset event should remain");
        assert_eq!(remaining.events.len(), 1);
        assert_eq!(
            remaining.events.front().map(PendingAccountHealthEvent::id),
            Some(2)
        );
    }
}
