//! Rate Limit Module
//!
//! 限流模块，支持内存后端和 Redis 后端，按 user/tenant/key 多维度限流。
//! 支持从租户配置动态加载 RPM/TPM 限制。

use async_trait::async_trait;
use dashmap::DashMap;
use keycompute_types::{KeyComputeError, Result};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};
use uuid::Uuid;

#[cfg(feature = "redis")]
pub mod redis;

#[cfg(feature = "redis")]
pub use redis::RedisRateLimiter;

/// 默认限流参数（当租户未配置时使用）
pub const DEFAULT_RPM_LIMIT: u32 = 60;
pub const DEFAULT_TPM_LIMIT: u32 = 100_000;
pub const WINDOW_SECS: u64 = 60;
/// 并发请求限制（供未来使用）
#[allow(dead_code)]
const CONCURRENCY_LIMIT: u32 = 10;

/// 限流配置（包含租户特定的限制）
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// RPM (Requests Per Minute) 限制
    pub rpm_limit: u32,
    /// TPM (Tokens Per Minute) 限制
    pub tpm_limit: u32,
    /// 窗口大小（秒）
    pub window_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            rpm_limit: DEFAULT_RPM_LIMIT,
            tpm_limit: DEFAULT_TPM_LIMIT,
            window_secs: WINDOW_SECS,
        }
    }
}

impl RateLimitConfig {
    /// 创建新的限流配置
    pub fn new(rpm_limit: u32, tpm_limit: u32) -> Self {
        Self {
            rpm_limit,
            tpm_limit,
            window_secs: WINDOW_SECS,
        }
    }

    /// 从租户字段创建
    pub fn from_tenant(rpm_limit: i32, tpm_limit: i32) -> Self {
        Self {
            rpm_limit: rpm_limit.max(1) as u32,
            tpm_limit: tpm_limit.max(1) as u32,
            window_secs: WINDOW_SECS,
        }
    }
}

/// 限流键
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct RateLimitKey {
    /// 租户 ID
    pub tenant_id: Uuid,
    /// 用户 ID
    pub user_id: Uuid,
    /// API Key ID
    pub api_key_id: Uuid,
}

impl RateLimitKey {
    /// 创建新的限流键
    pub fn new(tenant_id: Uuid, user_id: Uuid, api_key_id: Uuid) -> Self {
        Self {
            tenant_id,
            user_id,
            api_key_id,
        }
    }
}

/// 限流计数器（单一 key 的完整状态）。
#[derive(Debug)]
struct RateLimitEntry {
    /// 请求计数
    request_count: AtomicU64,
    /// Sliding TPM events and request IDs whose terminal usage has already been
    /// recorded in the active horizon.
    token_records: std::sync::Mutex<TokenRecordCache>,
    /// RPM 窗口开始时间
    window_start: std::sync::Mutex<Instant>,
    /// 窗口大小
    window_size: Duration,
}

#[derive(Debug, Default)]
struct TokenRecordCache {
    records: HashMap<Uuid, TokenRecord>,
    expirations: BinaryHeap<Reverse<(Instant, Uuid)>>,
    total_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenRecordKind {
    Reserved,
    Terminal,
}

#[derive(Debug, Clone, Copy)]
struct TokenRecord {
    tokens: u64,
    expires_at: Instant,
    kind: TokenRecordKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenReservationOutcome {
    Reserved,
    LimitExceeded,
    AlreadyTerminal,
    PredictionMismatch { existing_tokens: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenReservationRenewalOutcome {
    Renewed,
    Missing,
    AlreadyTerminal,
    PredictionMismatch { existing_tokens: u64 },
}

impl TokenRecordCache {
    fn prune(&mut self, now: Instant) {
        while self
            .expirations
            .peek()
            .is_some_and(|Reverse((expires_at, _))| *expires_at <= now)
        {
            if let Some(Reverse((_, expired_id))) = self.expirations.pop()
                && self
                    .records
                    .get(&expired_id)
                    .is_some_and(|record| record.expires_at <= now)
                && let Some(record) = self.records.remove(&expired_id)
            {
                self.total_tokens = self.total_tokens.saturating_sub(record.tokens);
            }
        }
    }

    fn insert_record(
        &mut self,
        request_id: Uuid,
        tokens: u64,
        kind: TokenRecordKind,
        now: Instant,
        remaining_horizon: Duration,
    ) {
        if let Some(previous) = self.records.remove(&request_id) {
            self.total_tokens = self.total_tokens.saturating_sub(previous.tokens);
        }
        let expires_at = now.checked_add(remaining_horizon).unwrap_or(now);
        self.records.insert(
            request_id,
            TokenRecord {
                tokens,
                expires_at,
                kind,
            },
        );
        self.expirations.push(Reverse((expires_at, request_id)));
        self.total_tokens = self.total_tokens.saturating_add(tokens);
    }

    fn reserve(
        &mut self,
        reservation_id: Uuid,
        terminal_id: Uuid,
        tokens: u64,
        limit: u64,
        now: Instant,
        window: Duration,
    ) -> TokenReservationOutcome {
        self.prune(now);
        if self
            .records
            .get(&terminal_id)
            .is_some_and(|record| record.kind == TokenRecordKind::Terminal)
        {
            return TokenReservationOutcome::AlreadyTerminal;
        }
        if let Some(existing) = self.records.get(&reservation_id) {
            return match existing.kind {
                TokenRecordKind::Terminal => TokenReservationOutcome::AlreadyTerminal,
                TokenRecordKind::Reserved if existing.tokens == tokens => {
                    TokenReservationOutcome::Reserved
                }
                TokenRecordKind::Reserved => TokenReservationOutcome::PredictionMismatch {
                    existing_tokens: existing.tokens,
                },
            };
        }
        if self.total_tokens.saturating_add(tokens) > limit {
            return TokenReservationOutcome::LimitExceeded;
        }
        self.insert_record(
            reservation_id,
            tokens,
            TokenRecordKind::Reserved,
            now,
            window,
        );
        TokenReservationOutcome::Reserved
    }

    /// Refresh an existing reservation lease, or restore the same previously
    /// admitted lease from durable settlement state after a process restart.
    /// Neither mode can overwrite a terminal record or a different prediction.
    fn renew(
        &mut self,
        reservation_id: Uuid,
        terminal_id: Uuid,
        tokens: u64,
        now: Instant,
        window: Duration,
        restore_missing: bool,
    ) -> TokenReservationRenewalOutcome {
        self.prune(now);
        if self
            .records
            .get(&terminal_id)
            .is_some_and(|record| record.kind == TokenRecordKind::Terminal)
        {
            return TokenReservationRenewalOutcome::AlreadyTerminal;
        }
        if let Some(existing) = self.records.get_mut(&reservation_id) {
            return match existing.kind {
                TokenRecordKind::Terminal => TokenReservationRenewalOutcome::AlreadyTerminal,
                TokenRecordKind::Reserved if existing.tokens == tokens => {
                    let expires_at = now.checked_add(window).unwrap_or(now);
                    existing.expires_at = expires_at;
                    self.expirations.push(Reverse((expires_at, reservation_id)));
                    TokenReservationRenewalOutcome::Renewed
                }
                TokenRecordKind::Reserved => TokenReservationRenewalOutcome::PredictionMismatch {
                    existing_tokens: existing.tokens,
                },
            };
        }
        if !restore_missing {
            return TokenReservationRenewalOutcome::Missing;
        }
        self.insert_record(
            reservation_id,
            tokens,
            TokenRecordKind::Reserved,
            now,
            window,
        );
        TokenReservationRenewalOutcome::Renewed
    }

    fn reconcile_at(
        &mut self,
        reservation_id: Uuid,
        terminal_id: Uuid,
        tokens: u64,
        now: Instant,
        remaining_horizon: Option<Duration>,
    ) -> bool {
        self.prune(now);
        if self
            .records
            .get(&reservation_id)
            .is_some_and(|record| record.kind == TokenRecordKind::Reserved)
            && let Some(record) = self.records.remove(&reservation_id)
        {
            self.total_tokens = self.total_tokens.saturating_sub(record.tokens);
        }
        if self
            .records
            .get(&terminal_id)
            .is_some_and(|record| record.kind == TokenRecordKind::Terminal)
        {
            return true;
        }
        if self.records.contains_key(&terminal_id) {
            return false;
        }
        let Some(remaining_horizon) = remaining_horizon.filter(|horizon| !horizon.is_zero()) else {
            return true;
        };
        self.insert_record(
            terminal_id,
            tokens,
            TokenRecordKind::Terminal,
            now,
            remaining_horizon,
        );
        true
    }

    fn release(&mut self, request_id: Uuid, now: Instant) {
        self.prune(now);
        if self
            .records
            .get(&request_id)
            .is_some_and(|record| record.kind == TokenRecordKind::Reserved)
            && let Some(record) = self.records.remove(&request_id)
        {
            self.total_tokens = self.total_tokens.saturating_sub(record.tokens);
        }
    }

    fn total(&mut self, now: Instant) -> u64 {
        self.prune(now);
        self.total_tokens
    }

    fn is_empty(&mut self, now: Instant) -> bool {
        self.prune(now);
        self.records.is_empty()
    }
}

impl RateLimitEntry {
    fn new(window_size: Duration) -> Self {
        Self {
            request_count: AtomicU64::new(0),
            token_records: std::sync::Mutex::new(TokenRecordCache::default()),
            window_start: std::sync::Mutex::new(Instant::now()),
            window_size,
        }
    }

    fn is_expired(&self) -> bool {
        let start = self.window_start.lock().unwrap();
        let now = Instant::now();
        let rpm_expired = now.duration_since(*start) > self.window_size;
        drop(start);
        rpm_expired && self.token_records.lock().unwrap().is_empty(now)
    }

    /// 重置计数器（原子操作）
    ///
    /// 返回 true 表示执行了重置，false 表示窗口未过期
    fn reset_if_expired(&self) -> bool {
        let mut start = self.window_start.lock().unwrap();
        if Instant::now().duration_since(*start) > self.window_size {
            self.request_count.store(0, Ordering::Relaxed);
            *start = Instant::now();
            true
        } else {
            false
        }
    }

    fn increment_request(&self) -> u64 {
        self.request_count.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn reconcile_tokens_once_at(
        &self,
        reservation_id: Uuid,
        terminal_id: Uuid,
        tokens: u64,
        occurred_at: SystemTime,
    ) -> bool {
        let wall_now = SystemTime::now();
        let age = wall_now.duration_since(occurred_at).unwrap_or_default();
        let now = Instant::now();
        let mut records = self.token_records.lock().unwrap();
        records.reconcile_at(
            reservation_id,
            terminal_id,
            tokens,
            now,
            self.window_size.checked_sub(age),
        )
    }

    fn reserve_tokens(
        &self,
        reservation_id: Uuid,
        terminal_id: Uuid,
        tokens: u64,
        limit: u64,
    ) -> TokenReservationOutcome {
        self.token_records.lock().unwrap().reserve(
            reservation_id,
            terminal_id,
            tokens,
            limit,
            Instant::now(),
            self.window_size,
        )
    }

    fn renew_tokens(
        &self,
        reservation_id: Uuid,
        terminal_id: Uuid,
        tokens: u64,
        restore_missing: bool,
    ) -> TokenReservationRenewalOutcome {
        self.token_records.lock().unwrap().renew(
            reservation_id,
            terminal_id,
            tokens,
            Instant::now(),
            self.window_size,
            restore_missing,
        )
    }

    fn release_tokens(&self, request_id: Uuid) {
        self.token_records
            .lock()
            .unwrap()
            .release(request_id, Instant::now());
    }

    fn request_count(&self) -> u64 {
        self.request_count.load(Ordering::Relaxed)
    }

    fn token_count(&self) -> u64 {
        self.token_records.lock().unwrap().total(Instant::now())
    }

    fn decrement_request(&self) {
        self.request_count.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 限流器 trait
#[async_trait]
pub trait RateLimiter: Send + Sync + std::fmt::Debug {
    /// 检查是否允许请求（使用默认限制）
    async fn check(&self, key: &RateLimitKey) -> Result<bool>;

    /// 检查是否允许请求（使用租户特定限制）
    async fn check_with_config(&self, key: &RateLimitKey, config: &RateLimitConfig)
    -> Result<bool>;

    /// 记录请求（通过后调用）
    async fn record(&self, key: &RateLimitKey) -> Result<()>;

    /// 原子地检查并记录请求（使用租户特定限制）
    ///
    /// 默认实现先检查再记录，非原子操作。分布式场景应覆盖此方法。
    async fn check_and_record_with_config(
        &self,
        key: &RateLimitKey,
        config: &RateLimitConfig,
    ) -> Result<()> {
        if !self.check_with_config(key, config).await? {
            return Err(KeyComputeError::RateLimitExceeded(format!(
                "RPM limit exceeded for tenant {}",
                key.tenant_id
            )));
        }
        self.record(key).await
    }

    /// 记录 Token 使用量
    async fn record_tokens(&self, key: &RateLimitKey, tokens: u32) -> Result<()>;

    /// Atomically reserve a predicted Token budget for one physical attempt,
    /// while fencing execution if its logical terminal identity already exists.
    /// Repeating the same physical ID and prediction is idempotent.
    async fn reserve_tokens(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        predicted_tokens: u32,
        limit: u32,
    ) -> Result<()>;

    /// Atomically renew a still-active predicted-token reservation. Returns
    /// `false` when the reservation is already absent or terminal. A different
    /// prediction for the same physical ID is an error and is never overwritten.
    async fn renew_token_reservation(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        predicted_tokens: u32,
    ) -> Result<bool>;

    /// Restore an already-admitted reservation from durable settlement state.
    /// This intentionally does not run admission again: recovered work is
    /// already executing, and restoring it above the current limit must block
    /// new work rather than silently forget its capacity. Returns `false` when
    /// logical terminal usage already exists.
    async fn restore_token_reservation(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        predicted_tokens: u32,
    ) -> Result<bool>;

    /// Release a request's still-pending Token reservation. A terminal record
    /// with the same request ID is immutable and must not be removed.
    async fn release_token_reservation(&self, key: &RateLimitKey, request_id: Uuid) -> Result<()>;

    /// Idempotently record one request's terminal Token usage.
    async fn record_tokens_once(
        &self,
        key: &RateLimitKey,
        request_id: Uuid,
        tokens: u32,
    ) -> Result<()> {
        self.record_tokens_once_at(key, request_id, tokens, SystemTime::now())
            .await
    }

    /// Idempotently record terminal Token usage at its actual occurrence time.
    async fn record_tokens_once_at(
        &self,
        key: &RateLimitKey,
        request_id: Uuid,
        tokens: u32,
        occurred_at: SystemTime,
    ) -> Result<()> {
        self.reconcile_tokens_once_at(key, request_id, request_id, tokens, occurred_at)
            .await
    }

    /// Atomically remove one physical attempt's prediction and record terminal
    /// usage once under the stable logical billing identity.
    async fn reconcile_tokens_once_at(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        tokens: u32,
        occurred_at: SystemTime,
    ) -> Result<()>;

    /// 获取当前计数
    async fn get_count(&self, key: &RateLimitKey) -> Result<u64>;

    /// 获取当前 Token 使用量
    async fn get_token_count(&self, key: &RateLimitKey) -> Result<u64>;
}

/// 内存限流器
#[derive(Debug)]
pub struct MemoryRateLimiter {
    /// 限流条目（包含 RPM 固定窗口和 TPM 滑动窗口）
    entries: DashMap<RateLimitKey, RateLimitEntry>,
    window_size: Duration,
}

impl MemoryRateLimiter {
    /// 创建新的内存限流器
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            window_size: Duration::from_secs(WINDOW_SECS),
        }
    }

    /// 清理过期计数器
    pub fn cleanup(&self) {
        self.entries.retain(|_, entry| !entry.is_expired());
    }

    /// 获取或创建限流条目
    fn get_or_create_entry(
        &self,
        key: &RateLimitKey,
    ) -> dashmap::mapref::one::Ref<'_, RateLimitKey, RateLimitEntry> {
        self.entries
            .entry(key.clone())
            .or_insert_with(|| RateLimitEntry::new(self.window_size))
            .downgrade()
    }
}

impl Default for MemoryRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RateLimiter for MemoryRateLimiter {
    async fn check(&self, key: &RateLimitKey) -> Result<bool> {
        self.check_with_config(key, &RateLimitConfig::default())
            .await
    }

    async fn check_with_config(
        &self,
        key: &RateLimitKey,
        config: &RateLimitConfig,
    ) -> Result<bool> {
        let entry = self.get_or_create_entry(key);
        entry.reset_if_expired();
        Ok(entry.request_count() < config.rpm_limit as u64)
    }

    async fn record(&self, key: &RateLimitKey) -> Result<()> {
        let entry = self.get_or_create_entry(key);
        entry.increment_request();
        Ok(())
    }

    async fn record_tokens(&self, key: &RateLimitKey, tokens: u32) -> Result<()> {
        let entry = self.get_or_create_entry(key);
        let request_id = Uuid::new_v4();
        if entry.reconcile_tokens_once_at(request_id, request_id, tokens as u64, SystemTime::now())
        {
            Ok(())
        } else {
            Err(KeyComputeError::Internal(
                "TPM terminal identity conflicts with an active reservation".to_string(),
            ))
        }
    }

    async fn reserve_tokens(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        predicted_tokens: u32,
        limit: u32,
    ) -> Result<()> {
        let entry = self.get_or_create_entry(key);
        match entry.reserve_tokens(
            reservation_id,
            terminal_id,
            predicted_tokens as u64,
            limit as u64,
        ) {
            TokenReservationOutcome::Reserved => Ok(()),
            TokenReservationOutcome::LimitExceeded => {
                Err(KeyComputeError::RateLimitExceeded(format!(
                    "TPM limit exceeded for tenant {} (limit: {}, requested: {})",
                    key.tenant_id, limit, predicted_tokens
                )))
            }
            TokenReservationOutcome::AlreadyTerminal => Err(KeyComputeError::Internal(format!(
                "TPM logical request {terminal_id} already has terminal usage"
            ))),
            TokenReservationOutcome::PredictionMismatch { existing_tokens } => {
                Err(KeyComputeError::Internal(format!(
                    "TPM reservation {reservation_id} prediction changed from {existing_tokens} to {predicted_tokens}"
                )))
            }
        }
    }

    async fn renew_token_reservation(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        predicted_tokens: u32,
    ) -> Result<bool> {
        let entry = self.get_or_create_entry(key);
        match entry.renew_tokens(reservation_id, terminal_id, predicted_tokens as u64, false) {
            TokenReservationRenewalOutcome::Renewed => Ok(true),
            TokenReservationRenewalOutcome::Missing
            | TokenReservationRenewalOutcome::AlreadyTerminal => Ok(false),
            TokenReservationRenewalOutcome::PredictionMismatch { existing_tokens } => {
                Err(KeyComputeError::Internal(format!(
                    "TPM reservation {reservation_id} prediction changed from {existing_tokens} to {predicted_tokens}"
                )))
            }
        }
    }

    async fn restore_token_reservation(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        predicted_tokens: u32,
    ) -> Result<bool> {
        let entry = self.get_or_create_entry(key);
        match entry.renew_tokens(reservation_id, terminal_id, predicted_tokens as u64, true) {
            TokenReservationRenewalOutcome::Renewed => Ok(true),
            TokenReservationRenewalOutcome::AlreadyTerminal => Ok(false),
            TokenReservationRenewalOutcome::Missing => Err(KeyComputeError::Internal(
                "TPM durable reservation restore returned an impossible missing state".to_string(),
            )),
            TokenReservationRenewalOutcome::PredictionMismatch { existing_tokens } => {
                Err(KeyComputeError::Internal(format!(
                    "TPM reservation {reservation_id} prediction changed from {existing_tokens} to {predicted_tokens}"
                )))
            }
        }
    }

    async fn release_token_reservation(&self, key: &RateLimitKey, request_id: Uuid) -> Result<()> {
        let entry = self.get_or_create_entry(key);
        entry.release_tokens(request_id);
        Ok(())
    }

    async fn reconcile_tokens_once_at(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        tokens: u32,
        occurred_at: SystemTime,
    ) -> Result<()> {
        let entry = self.get_or_create_entry(key);
        if entry.reconcile_tokens_once_at(reservation_id, terminal_id, tokens as u64, occurred_at) {
            Ok(())
        } else {
            Err(KeyComputeError::Internal(format!(
                "TPM terminal identity {terminal_id} conflicts with an active reservation"
            )))
        }
    }

    async fn get_count(&self, key: &RateLimitKey) -> Result<u64> {
        let entry = self.get_or_create_entry(key);
        entry.reset_if_expired();
        Ok(entry.request_count())
    }

    async fn get_token_count(&self, key: &RateLimitKey) -> Result<u64> {
        let entry = self.get_or_create_entry(key);
        Ok(entry.token_count())
    }

    /// 原子地检查并记录请求（使用租户特定限制）
    ///
    /// 实现策略：先原子增加，再检查是否超限，超限则回滚。
    /// 使用单一 DashMap 条目保证窗口同步和操作原子性。
    async fn check_and_record_with_config(
        &self,
        key: &RateLimitKey,
        config: &RateLimitConfig,
    ) -> Result<()> {
        let entry = self.get_or_create_entry(key);

        // 原子地检查并重置过期窗口
        entry.reset_if_expired();

        // 原子地增加计数
        let new_count = entry.increment_request();

        // 检查是否超限
        if new_count > config.rpm_limit as u64 {
            // 超限，回滚计数
            entry.decrement_request();
            return Err(KeyComputeError::RateLimitExceeded(format!(
                "RPM limit exceeded for tenant {} (limit: {}, current: {}) ",
                key.tenant_id, config.rpm_limit, new_count
            )));
        }

        Ok(())
    }
}

/// 限流服务
pub struct RateLimitService {
    limiter: std::sync::Arc<dyn RateLimiter>,
    backend: RateLimitBackend,
}

/// 限流后端类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitBackend {
    /// 内存后端
    Memory,
    /// Redis 后端
    Redis,
}

impl std::fmt::Debug for RateLimitService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimitService")
            .field("backend", &self.backend)
            .finish()
    }
}

impl Clone for RateLimitService {
    fn clone(&self) -> Self {
        Self {
            limiter: Arc::clone(&self.limiter),
            backend: self.backend,
        }
    }
}

impl RateLimitService {
    /// 创建新的限流服务
    pub fn new(limiter: std::sync::Arc<dyn RateLimiter>, backend: RateLimitBackend) -> Self {
        Self { limiter, backend }
    }

    /// 创建默认的内存限流服务
    pub fn default_memory() -> Self {
        Self::new(
            std::sync::Arc::new(MemoryRateLimiter::default()),
            RateLimitBackend::Memory,
        )
    }

    /// 获取后端类型
    pub fn backend(&self) -> RateLimitBackend {
        self.backend
    }

    /// 检查并记录请求（使用默认限制）
    pub async fn check_and_record(&self, key: &RateLimitKey) -> Result<()> {
        if !self.limiter.check(key).await? {
            return Err(KeyComputeError::RateLimitExceeded(format!(
                "RPM limit exceeded for tenant {}",
                key.tenant_id
            )));
        }
        self.limiter.record(key).await
    }

    /// 检查并记录请求（使用租户特定限制）
    ///
    /// 这是主要入口，用于应用租户的配额限制
    pub async fn check_and_record_with_config(
        &self,
        key: &RateLimitKey,
        config: &RateLimitConfig,
    ) -> Result<()> {
        // 使用 limiter 的原子方法（分布式后端会覆盖实现）
        self.limiter
            .check_and_record_with_config(key, config)
            .await
            .map_err(|e| {
                if matches!(e, KeyComputeError::RateLimitExceeded(_)) {
                    tracing::warn!(
                        tenant_id = %key.tenant_id,
                        user_id = %key.user_id,
                        rpm_limit = config.rpm_limit,
                        "RPM limit exceeded"
                    );
                }
                e
            })
    }

    /// 仅检查不限流
    pub async fn check_only(&self, key: &RateLimitKey) -> Result<bool> {
        self.limiter.check(key).await
    }

    /// 仅检查不限流（使用租户特定限制）
    pub async fn check_only_with_config(
        &self,
        key: &RateLimitKey,
        config: &RateLimitConfig,
    ) -> Result<bool> {
        self.limiter.check_with_config(key, config).await
    }

    /// 记录 Token 使用量（用于 TPM 限制）
    pub async fn record_token_usage(&self, key: &RateLimitKey, tokens: u32) -> Result<()> {
        self.limiter.record_tokens(key, tokens).await
    }

    /// Atomically reserve predicted TPM capacity for one logical request.
    pub async fn reserve_token_usage(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        predicted_tokens: u32,
        config: &RateLimitConfig,
    ) -> Result<()> {
        self.limiter
            .reserve_tokens(
                key,
                reservation_id,
                terminal_id,
                predicted_tokens,
                config.tpm_limit,
            )
            .await
            .map_err(|error| {
                if matches!(error, KeyComputeError::RateLimitExceeded(_)) {
                    tracing::warn!(
                        tenant_id = %key.tenant_id,
                        user_id = %key.user_id,
                        reservation_id = %reservation_id,
                        terminal_id = %terminal_id,
                        predicted_tokens,
                        tpm_limit = config.tpm_limit,
                        "TPM reservation rejected"
                    );
                }
                error
            })
    }

    /// Renew an existing in-flight TPM reservation without changing its
    /// aggregate amount. This operation never recreates a missing record.
    pub async fn renew_token_reservation(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        predicted_tokens: u32,
    ) -> Result<bool> {
        self.limiter
            .renew_token_reservation(key, reservation_id, terminal_id, predicted_tokens)
            .await
    }

    /// Restore an already-admitted lease from a durable settlement outbox.
    pub async fn restore_token_reservation(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        predicted_tokens: u32,
    ) -> Result<bool> {
        self.limiter
            .restore_token_reservation(key, reservation_id, terminal_id, predicted_tokens)
            .await
    }

    /// Release predicted capacity when a request cannot reach terminal
    /// settlement. This is idempotent and cannot erase terminal usage.
    pub async fn release_token_reservation(
        &self,
        key: &RateLimitKey,
        request_id: Uuid,
    ) -> Result<()> {
        self.limiter
            .release_token_reservation(key, request_id)
            .await
    }

    /// Idempotently reconcile one request's predicted reservation to terminal
    /// usage. If no reservation exists, this records the terminal usage once.
    pub async fn record_token_usage_once(
        &self,
        key: &RateLimitKey,
        request_id: Uuid,
        tokens: u32,
    ) -> Result<()> {
        self.limiter
            .record_tokens_once(key, request_id, tokens)
            .await
    }

    /// Idempotently reconcile terminal usage at its actual occurrence time.
    pub async fn record_token_usage_once_at(
        &self,
        key: &RateLimitKey,
        request_id: Uuid,
        tokens: u32,
        occurred_at: SystemTime,
    ) -> Result<()> {
        self.limiter
            .record_tokens_once_at(key, request_id, tokens, occurred_at)
            .await
    }

    /// Reconcile one physical attempt's prediction to the logical request's
    /// terminal usage in one backend-atomic operation.
    pub async fn reconcile_token_usage_once_at(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        tokens: u32,
        occurred_at: SystemTime,
    ) -> Result<()> {
        self.limiter
            .reconcile_tokens_once_at(key, reservation_id, terminal_id, tokens, occurred_at)
            .await
    }

    /// 检查 TPM 限制
    pub async fn check_tpm(&self, key: &RateLimitKey, config: &RateLimitConfig) -> Result<bool> {
        let current_tokens = self.limiter.get_token_count(key).await?;
        Ok(current_tokens < config.tpm_limit as u64)
    }

    /// 获取当前 RPM 计数
    pub async fn get_rpm_count(&self, key: &RateLimitKey) -> Result<u64> {
        self.limiter.get_count(key).await
    }

    /// 获取当前 TPM 计数
    pub async fn get_tpm_count(&self, key: &RateLimitKey) -> Result<u64> {
        self.limiter.get_token_count(key).await
    }
}

#[cfg(feature = "redis")]
impl RateLimitService {
    /// 通过 URL 创建 Redis 限流服务（内部创建连接池）
    pub fn new_redis(redis_url: &str) -> Result<Self> {
        let cfg = deadpool_redis::Config::from_url(redis_url);
        let pool = cfg
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .map_err(|e| {
                KeyComputeError::Internal(format!("Failed to create Redis pool: {}", e))
            })?;
        let limiter = RedisRateLimiter::new(pool);
        Ok(Self::new(
            std::sync::Arc::new(limiter),
            RateLimitBackend::Redis,
        ))
    }

    /// 通过 URL 创建带前缀的 Redis 限流服务（内部创建连接池）
    pub fn new_redis_with_prefix(redis_url: &str, prefix: impl Into<String>) -> Result<Self> {
        let cfg = deadpool_redis::Config::from_url(redis_url);
        let pool = cfg
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .map_err(|e| {
                KeyComputeError::Internal(format!("Failed to create Redis pool: {}", e))
            })?;
        let limiter = RedisRateLimiter::with_prefix(pool, prefix);
        Ok(Self::new(
            std::sync::Arc::new(limiter),
            RateLimitBackend::Redis,
        ))
    }

    /// 使用已有连接池创建 Redis 限流服务
    ///
    /// 与 `new_redis` 的区别在于接受外部创建的连接池，
    /// 用于与其他 Redis 消费者共享同一连接池。
    pub fn with_redis_pool(pool: deadpool_redis::Pool) -> Self {
        let limiter = RedisRateLimiter::new(pool);
        Self::new(std::sync::Arc::new(limiter), RateLimitBackend::Redis)
    }

    /// 使用已有连接池 + 前缀创建 Redis 限流服务
    pub fn with_redis_pool_and_prefix(
        pool: deadpool_redis::Pool,
        prefix: impl Into<String>,
    ) -> Self {
        let limiter = RedisRateLimiter::with_prefix(pool, prefix);
        Self::new(std::sync::Arc::new(limiter), RateLimitBackend::Redis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit_key() {
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        assert!(!key.tenant_id.is_nil());
    }

    #[test]
    fn test_rate_limit_constants() {
        assert_eq!(DEFAULT_RPM_LIMIT, 60);
        assert_eq!(DEFAULT_TPM_LIMIT, 100_000);
        assert_eq!(CONCURRENCY_LIMIT, 10);
        assert_eq!(WINDOW_SECS, 60);
    }

    #[test]
    fn test_rate_limit_config() {
        let config = RateLimitConfig::default();
        assert_eq!(config.rpm_limit, 60);
        assert_eq!(config.tpm_limit, 100_000);

        let config = RateLimitConfig::new(100, 200_000);
        assert_eq!(config.rpm_limit, 100);
        assert_eq!(config.tpm_limit, 200_000);

        let config = RateLimitConfig::from_tenant(120, 150_000);
        assert_eq!(config.rpm_limit, 120);
        assert_eq!(config.tpm_limit, 150_000);
    }

    #[tokio::test]
    async fn test_memory_rate_limiter() {
        let limiter = MemoryRateLimiter::default();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let config = RateLimitConfig::default();

        // 第一次检查应该通过
        assert!(limiter.check_with_config(&key, &config).await.unwrap());

        // 记录请求
        limiter.record(&key).await.unwrap();

        // 检查仍应通过（未达到限制）
        assert!(limiter.check_with_config(&key, &config).await.unwrap());
    }

    #[tokio::test]
    async fn test_rate_limit_service() {
        let service = RateLimitService::default_memory();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let config = RateLimitConfig::default();

        // 第一次请求应该成功
        assert!(
            service
                .check_and_record_with_config(&key, &config)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_rate_limit_service_with_custom_config() {
        let service = RateLimitService::default_memory();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        // 使用低限制配置
        let config = RateLimitConfig::new(2, 1000);

        // 前两次请求应该成功
        assert!(
            service
                .check_and_record_with_config(&key, &config)
                .await
                .is_ok()
        );
        assert!(
            service
                .check_and_record_with_config(&key, &config)
                .await
                .is_ok()
        );

        // 第三次请求应该被拒绝
        assert!(
            service
                .check_and_record_with_config(&key, &config)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_token_tracking() {
        let service = RateLimitService::default_memory();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        // 记录 Token 使用量
        service.record_token_usage(&key, 100).await.unwrap();
        service.record_token_usage(&key, 50).await.unwrap();

        // 检查 Token 计数
        let count = service.get_tpm_count(&key).await.unwrap();
        assert_eq!(count, 150);
    }

    #[tokio::test]
    async fn terminal_token_tracking_is_idempotent_by_request() {
        let service = RateLimitService::default_memory();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let request_id = Uuid::new_v4();

        service
            .record_token_usage_once(&key, request_id, 100)
            .await
            .unwrap();
        service
            .record_token_usage_once(&key, request_id, 100)
            .await
            .unwrap();
        service
            .record_token_usage_once(&key, Uuid::new_v4(), 50)
            .await
            .unwrap();

        assert_eq!(service.get_tpm_count(&key).await.unwrap(), 150);
    }

    #[tokio::test]
    async fn late_terminal_tokens_expire_at_the_original_window_boundary() {
        let service = RateLimitService::default_memory();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let occurred_at = SystemTime::now()
            .checked_sub(Duration::from_millis(WINDOW_SECS * 1_000 - 250))
            .unwrap();

        service
            .record_token_usage_once_at(&key, Uuid::new_v4(), 100, occurred_at)
            .await
            .unwrap();
        assert_eq!(service.get_tpm_count(&key).await.unwrap(), 100);

        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(service.get_tpm_count(&key).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn live_reservation_reconcile_keeps_original_window_boundary() {
        let service = RateLimitService::default_memory();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let config = RateLimitConfig::new(100, 100);
        let reservation_id = Uuid::new_v4();
        let terminal_id = Uuid::new_v4();
        let occurred_at = SystemTime::now()
            .checked_sub(Duration::from_secs(WINDOW_SECS - 1))
            .unwrap();

        service
            .reserve_token_usage(&key, reservation_id, terminal_id, 80, &config)
            .await
            .unwrap();
        service
            .reconcile_token_usage_once_at(&key, reservation_id, terminal_id, 30, occurred_at)
            .await
            .unwrap();
        assert_eq!(service.get_tpm_count(&key).await.unwrap(), 30);
        assert!(
            !service
                .restore_token_reservation(&key, reservation_id, terminal_id, 80)
                .await
                .unwrap(),
            "the terminal record must fence a restore inside its original window"
        );

        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(
            service.get_tpm_count(&key).await.unwrap(),
            0,
            "reconciliation must not restart a full window at replay time"
        );
    }

    #[test]
    fn terminal_token_cache_expires_by_occurrence_horizon() {
        let mut cache = TokenRecordCache::default();
        let started_at = Instant::now();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();

        assert_eq!(
            cache.reserve(first, first, 10, 100, started_at, Duration::from_secs(60)),
            TokenReservationOutcome::Reserved
        );
        assert_eq!(
            cache.reserve(first, first, 10, 100, started_at, Duration::from_secs(60)),
            TokenReservationOutcome::Reserved
        );
        assert!(cache.reconcile_at(
            second,
            second,
            20,
            started_at,
            Some(Duration::from_secs(10)),
        ));
        assert_eq!(cache.total(started_at), 30);
        assert_eq!(cache.total(started_at + Duration::from_secs(11)), 10);
        assert_eq!(cache.total(started_at + Duration::from_secs(61)), 0);
        assert!(cache.reconcile_at(
            first,
            first,
            30,
            started_at + Duration::from_secs(61),
            Some(Duration::from_secs(60)),
        ));

        assert_eq!(cache.records.len(), 1);
        assert_eq!(cache.total_tokens, 30);
        assert_eq!(
            cache.records.get(&first).map(|record| record.kind),
            Some(TokenRecordKind::Terminal)
        );
    }

    #[test]
    fn active_token_reservation_renewal_extends_only_the_matching_lease() {
        let started_at = Instant::now();
        let window = Duration::from_secs(60);
        let physical_id = Uuid::new_v4();
        let logical_id = Uuid::new_v4();

        let mut mismatch_cache = TokenRecordCache::default();
        assert_eq!(
            mismatch_cache.reserve(physical_id, logical_id, 10, 100, started_at, window),
            TokenReservationOutcome::Reserved
        );
        assert_eq!(
            mismatch_cache.renew(
                physical_id,
                logical_id,
                11,
                started_at + Duration::from_secs(50),
                window,
                false,
            ),
            TokenReservationRenewalOutcome::PredictionMismatch {
                existing_tokens: 10
            }
        );
        assert_eq!(
            mismatch_cache.total(started_at + Duration::from_secs(61)),
            0,
            "a mismatched heartbeat must not refresh or mutate the lease"
        );

        let mut renewed_cache = TokenRecordCache::default();
        assert_eq!(
            renewed_cache.reserve(physical_id, logical_id, 10, 100, started_at, window),
            TokenReservationOutcome::Reserved
        );
        assert_eq!(
            renewed_cache.renew(
                physical_id,
                logical_id,
                10,
                started_at + Duration::from_secs(50),
                window,
                false,
            ),
            TokenReservationRenewalOutcome::Renewed
        );
        assert_eq!(
            renewed_cache.total(started_at + Duration::from_secs(61)),
            10
        );
        assert_eq!(
            renewed_cache.total(started_at + Duration::from_secs(111)),
            0
        );
        assert_eq!(
            renewed_cache.renew(
                physical_id,
                logical_id,
                10,
                started_at + Duration::from_secs(111),
                window,
                false,
            ),
            TokenReservationRenewalOutcome::Missing,
            "strict heartbeats must never resurrect an expired reservation"
        );
    }

    #[test]
    fn durable_token_reservation_restore_is_terminal_fenced() {
        let mut cache = TokenRecordCache::default();
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let physical_id = Uuid::new_v4();
        let logical_id = Uuid::new_v4();

        assert_eq!(
            cache.renew(physical_id, logical_id, 80, now, window, true),
            TokenReservationRenewalOutcome::Renewed
        );
        assert_eq!(cache.total(now), 80);
        assert!(cache.reconcile_at(
            physical_id,
            logical_id,
            30,
            now + Duration::from_secs(1),
            Some(window),
        ));
        assert_eq!(
            cache.renew(
                physical_id,
                logical_id,
                80,
                now + Duration::from_secs(2),
                window,
                true,
            ),
            TokenReservationRenewalOutcome::AlreadyTerminal,
            "durable recovery must not resurrect completed logical work"
        );
        assert_eq!(cache.total(now + Duration::from_secs(2)), 30);
    }

    #[tokio::test]
    async fn concurrent_tpm_reservations_never_exceed_limit() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let service = Arc::new(RateLimitService::default_memory());
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let config = Arc::new(RateLimitConfig::new(100, 50));
        let admitted = Arc::new(AtomicUsize::new(0));
        let mut tasks = tokio::task::JoinSet::new();

        for _ in 0..20 {
            let service = Arc::clone(&service);
            let key = key.clone();
            let config = Arc::clone(&config);
            let admitted = Arc::clone(&admitted);
            tasks.spawn(async move {
                let physical_id = Uuid::new_v4();
                if service
                    .reserve_token_usage(&key, physical_id, physical_id, 10, &config)
                    .await
                    .is_ok()
                {
                    admitted.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        while tasks.join_next().await.is_some() {}

        assert_eq!(admitted.load(Ordering::Relaxed), 5);
        assert_eq!(service.get_tpm_count(&key).await.unwrap(), 50);
    }

    #[tokio::test]
    async fn tpm_reconcile_replaces_prediction_and_release_cannot_erase_terminal() {
        let service = RateLimitService::default_memory();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let config = RateLimitConfig::new(100, 100);
        let physical_id = Uuid::new_v4();
        let logical_id = Uuid::new_v4();

        service
            .reserve_token_usage(&key, physical_id, logical_id, 80, &config)
            .await
            .unwrap();
        assert_eq!(service.get_tpm_count(&key).await.unwrap(), 80);
        service
            .reconcile_token_usage_once_at(&key, physical_id, logical_id, 30, SystemTime::now())
            .await
            .unwrap();
        assert_eq!(service.get_tpm_count(&key).await.unwrap(), 30);

        service
            .release_token_reservation(&key, physical_id)
            .await
            .unwrap();
        service
            .reconcile_token_usage_once_at(&key, physical_id, logical_id, 90, SystemTime::now())
            .await
            .unwrap();
        assert_eq!(service.get_tpm_count(&key).await.unwrap(), 30);
        assert!(
            service
                .reserve_token_usage(&key, Uuid::new_v4(), logical_id, 10, &config)
                .await
                .is_err(),
            "a terminal logical request must fence any later execution"
        );
    }

    #[tokio::test]
    async fn tpm_zero_reconcile_and_attempt_scoped_release_are_safe() {
        let service = RateLimitService::default_memory();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let config = RateLimitConfig::new(100, 100);
        let old_attempt = Uuid::new_v4();
        let new_attempt = Uuid::new_v4();
        let logical_id = Uuid::new_v4();

        service
            .reserve_token_usage(&key, old_attempt, logical_id, 40, &config)
            .await
            .unwrap();
        service
            .reserve_token_usage(&key, new_attempt, logical_id, 40, &config)
            .await
            .unwrap();
        service
            .release_token_reservation(&key, old_attempt)
            .await
            .unwrap();
        assert_eq!(service.get_tpm_count(&key).await.unwrap(), 40);

        service
            .reconcile_token_usage_once_at(&key, new_attempt, logical_id, 0, SystemTime::now())
            .await
            .unwrap();
        assert_eq!(service.get_tpm_count(&key).await.unwrap(), 0);
        assert!(
            service
                .reserve_token_usage(&key, Uuid::new_v4(), logical_id, 1, &config)
                .await
                .is_err(),
            "zero-token terminal usage still provides an idempotency fence"
        );
    }

    #[tokio::test]
    async fn tpm_same_attempt_prediction_mismatch_is_fail_closed() {
        let service = RateLimitService::default_memory();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let config = RateLimitConfig::new(100, 100);
        let physical_id = Uuid::new_v4();
        let logical_id = Uuid::new_v4();

        service
            .reserve_token_usage(&key, physical_id, logical_id, 25, &config)
            .await
            .unwrap();
        assert!(
            service
                .reserve_token_usage(&key, physical_id, logical_id, 30, &config)
                .await
                .is_err()
        );
        assert_eq!(service.get_tpm_count(&key).await.unwrap(), 25);
        service
            .release_token_reservation(&key, physical_id)
            .await
            .unwrap();
        assert_eq!(service.get_tpm_count(&key).await.unwrap(), 0);
    }

    /// 测试并发场景下的原子限流
    /// 验证在高并发情况下，限流计数准确，不会超出限制
    #[tokio::test]
    async fn test_concurrent_atomic_rate_limit() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        use tokio::task::JoinSet;

        let service = Arc::new(RateLimitService::default_memory());
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        // 设置限制为 5，并发 20 个请求
        let config = Arc::new(RateLimitConfig::new(5, 1000));
        let concurrent_requests = 20;

        let success_count = Arc::new(AtomicU32::new(0));
        let reject_count = Arc::new(AtomicU32::new(0));

        let mut tasks = JoinSet::new();

        for _ in 0..concurrent_requests {
            let service = Arc::clone(&service);
            let key = key.clone();
            let config = Arc::clone(&config);
            let success_count = Arc::clone(&success_count);
            let reject_count = Arc::clone(&reject_count);

            tasks.spawn(async move {
                let result = service.check_and_record_with_config(&key, &config).await;
                match result {
                    Ok(()) => success_count.fetch_add(1, Ordering::Relaxed),
                    Err(_) => reject_count.fetch_add(1, Ordering::Relaxed),
                };
            });
        }

        // 等待所有任务完成
        while tasks.join_next().await.is_some() {}

        let success = success_count.load(Ordering::Relaxed);
        let reject = reject_count.load(Ordering::Relaxed);

        // 验证：成功数应恰好等于限制数 5，拒绝数应为 15
        assert_eq!(
            success, 5,
            "Expected exactly 5 successful requests, got {}",
            success
        );
        assert_eq!(
            reject, 15,
            "Expected exactly 15 rejected requests, got {}",
            reject
        );

        // 验证最终计数准确
        let final_count = service.get_rpm_count(&key).await.unwrap();
        assert_eq!(
            final_count, 5,
            "Final count should be exactly 5, got {}",
            final_count
        );
    }
}
