//! Redis 限流器实现
//!
//! 基于 Redis 的分布式限流后端，支持多实例共享限流状态。
//! 使用 `deadpool-redis` 连接池管理 Redis 连接。
//!
//! # 连接池管理
//!
//! 通过 `RedisRateLimiter::new(pool)` 接受外部 `deadpool_redis::Pool`，
//! 可通过 `RateLimitService::with_redis_pool()` 在 `state.rs` 中与其他 Redis
//! 消费者（如 `RedisRuntimeStore`）共享同一连接池。
//!
//! # TPM 支持
//!
//! 通过 Lua 脚本和三个共享 Redis Cluster hash slot 的 key 实现按请求的
//! 滑动窗口 Token 计数：HASH 保存记录状态/数量，ZSET 按绝对过期时间索引，
//! STRING 保存聚合总额。reserve/reconcile/release 在 Redis 中均为原子操作，
//! 并且每次只处理已过期记录，而不扫描整个活跃窗口。

use crate::{DEFAULT_RPM_LIMIT, RateLimitKey, RateLimiter, WINDOW_SECS};
use async_trait::async_trait;
use deadpool_redis::redis::AsyncCommands;
use keycompute_types::{KeyComputeError, Result};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// Redis 限流器
///
/// 使用 Redis 实现分布式限流，支持：
/// - 滑动窗口限流
/// - 多实例共享限流状态
/// - 自动过期清理
/// - Token 计数（TPM）
#[derive(Debug, Clone)]
pub struct RedisRateLimiter {
    pool: deadpool_redis::Pool,
    window_size: Duration,
    key_prefix: String,
}

impl RedisRateLimiter {
    /// 使用已有连接池创建 Redis 限流器
    pub fn new(pool: deadpool_redis::Pool) -> Self {
        Self {
            pool,
            window_size: Duration::from_secs(WINDOW_SECS),
            key_prefix: "ratelimit".to_string(),
        }
    }

    /// 使用已有连接池 + 自定义前缀创建 Redis 限流器
    pub fn with_prefix(pool: deadpool_redis::Pool, prefix: impl Into<String>) -> Self {
        Self {
            pool,
            window_size: Duration::from_secs(WINDOW_SECS),
            key_prefix: prefix.into(),
        }
    }

    /// 构建限流 Redis Key
    fn build_key(&self, key: &RateLimitKey, suffix: &str) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.key_prefix, key.tenant_id, key.user_id, key.api_key_id, suffix
        )
    }

    /// 构建 RPM 的 Redis Key
    fn build_rpm_key(&self, key: &RateLimitKey) -> String {
        self.build_key(key, "rpm")
    }

    /// Build the three TPM v3 keys. The identity is enclosed in a Redis
    /// Cluster hash tag so every key touched by one Lua script is guaranteed
    /// to live in the same slot.
    fn build_tpm_keys(&self, key: &RateLimitKey) -> (String, String, String) {
        // v3 intentionally avoids both the legacy TPM ZSET and the v2
        // single-HASH representation. TPM state has a one-minute horizon, so
        // old ephemeral keys can expire naturally without an online migration.
        let base = format!(
            "{}:{{{}:{}:{}}}:tpm-v3",
            self.key_prefix, key.tenant_id, key.user_id, key.api_key_id
        );
        (
            format!("{base}:records"),
            format!("{base}:expirations"),
            format!("{base}:total"),
        )
    }

    /// 获取 Redis 连接
    async fn get_conn(&self) -> Result<deadpool_redis::Connection> {
        self.pool
            .get()
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis connection error: {}", e)))
    }

    fn check_and_record_script() -> &'static deadpool_redis::redis::Script {
        static SCRIPT: std::sync::OnceLock<deadpool_redis::redis::Script> =
            std::sync::OnceLock::new();
        SCRIPT.get_or_init(|| deadpool_redis::redis::Script::new(Self::CHECK_AND_RECORD_SCRIPT))
    }

    fn reserve_tokens_script() -> &'static deadpool_redis::redis::Script {
        static SCRIPT: std::sync::OnceLock<deadpool_redis::redis::Script> =
            std::sync::OnceLock::new();
        SCRIPT.get_or_init(|| deadpool_redis::redis::Script::new(Self::RESERVE_TOKENS_SCRIPT))
    }

    fn reconcile_tokens_script() -> &'static deadpool_redis::redis::Script {
        static SCRIPT: std::sync::OnceLock<deadpool_redis::redis::Script> =
            std::sync::OnceLock::new();
        SCRIPT.get_or_init(|| deadpool_redis::redis::Script::new(Self::RECONCILE_TOKENS_SCRIPT))
    }

    fn release_tokens_script() -> &'static deadpool_redis::redis::Script {
        static SCRIPT: std::sync::OnceLock<deadpool_redis::redis::Script> =
            std::sync::OnceLock::new();
        SCRIPT.get_or_init(|| deadpool_redis::redis::Script::new(Self::RELEASE_TOKENS_SCRIPT))
    }

    fn get_token_count_script() -> &'static deadpool_redis::redis::Script {
        static SCRIPT: std::sync::OnceLock<deadpool_redis::redis::Script> =
            std::sync::OnceLock::new();
        SCRIPT.get_or_init(|| deadpool_redis::redis::Script::new(Self::GET_TOKEN_COUNT_SCRIPT))
    }

    /// Prepare one TPM script with the three colocated physical keys. The
    /// `Script` invocation uses EVALSHA and automatically loads the source only
    /// after Redis reports NOSCRIPT, avoiding multi-kilobyte EVAL payloads on
    /// every request and heartbeat.
    fn prepare_tpm_script<'a>(
        &self,
        key: &RateLimitKey,
        script: &'a deadpool_redis::redis::Script,
    ) -> deadpool_redis::redis::ScriptInvocation<'a> {
        let (records_key, expirations_key, total_key) = self.build_tpm_keys(key);
        let mut invocation = script.prepare_invoke();
        invocation
            .key(records_key)
            .key(expirations_key)
            .key(total_key);
        invocation
    }

    async fn invoke_script(
        &self,
        invocation: deadpool_redis::redis::ScriptInvocation<'_>,
    ) -> Result<i64> {
        let mut conn = self.get_conn().await?;
        invocation
            .invoke_async(&mut conn)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))
    }

    /// Run the shared atomic reservation state transition. Keeping reserve,
    /// renew and durable restore on one script prevents a heartbeat from
    /// racing terminal reconciliation or changing the admitted prediction.
    async fn mutate_token_reservation(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        predicted_tokens: u32,
        limit: u32,
        mode: &'static str,
    ) -> Result<i64> {
        let mut invocation = self.prepare_tpm_script(key, Self::reserve_tokens_script());
        invocation
            .arg(self.window_size.as_secs() as i64)
            .arg(limit as i64)
            .arg(reservation_id.to_string())
            .arg(terminal_id.to_string())
            .arg(predicted_tokens as i64)
            .arg(self.expire_secs())
            .arg(mode);
        self.invoke_script(invocation).await
    }

    /// 获取当前 Unix 时间戳（秒）
    fn now_timestamp() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before epoch")
            .as_secs() as i64
    }

    /// 清理过期条目并返回当前窗口计数（RPM）
    async fn window_count(
        conn: &mut deadpool_redis::Connection,
        redis_key: &str,
        window_size: Duration,
    ) -> Result<u64> {
        let now = Self::now_timestamp();
        let window_start = now - window_size.as_secs() as i64;

        let _: () = conn
            .zrembyscore(redis_key, 0, window_start)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        let count: u64 = conn
            .zcard(redis_key)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        Ok(count)
    }

    /// 获取过期时间（窗口大小的 2 倍，确保滑动窗口安全）
    fn expire_secs(&self) -> i64 {
        (self.window_size.as_secs() * 2) as i64
    }

    /// Lua 脚本：原子地检查并记录请求
    /// 返回 1 表示成功，0 表示限流
    const CHECK_AND_RECORD_SCRIPT: &str = r#"
        local key = KEYS[1]
        local now = tonumber(ARGV[1])
        local window_start = tonumber(ARGV[2])
        local limit = tonumber(ARGV[3])
        local member = ARGV[4]
        local expire_secs = tonumber(ARGV[5])

        -- 清理过期条目
        redis.call('ZREMRANGEBYSCORE', key, 0, window_start)

        -- 获取当前计数
        local count = redis.call('ZCARD', key)

        -- 检查是否超限
        if count >= limit then
            return 0
        end

        -- 添加新条目
        redis.call('ZADD', key, now, member)
        redis.call('EXPIRE', key, expire_secs)

        return 1
    "#;

    /// Atomically reserve, renew, or durably restore predicted capacity for one
    /// request. Records are
    /// encoded as `kind:tokens`, where kind is `r` (reserved) or `t`
    /// (terminal). Expiry lives in a separate sorted set and the aggregate is
    /// maintained incrementally, so cleanup is O(number of expired records).
    const RESERVE_TOKENS_SCRIPT: &str = r#"
        local records_key = KEYS[1]
        local expirations_key = KEYS[2]
        local total_key = KEYS[3]
        local redis_time = redis.call('TIME')
        local now = tonumber(redis_time[1])
        local window_secs = tonumber(ARGV[1])
        local limit = tonumber(ARGV[2])
        local reservation_id = ARGV[3]
        local terminal_id = ARGV[4]
        local tokens = tonumber(ARGV[5])
        local expire_secs = tonumber(ARGV[6])
        local mode = ARGV[7]

        if mode ~= 'reserve' and mode ~= 'renew' and mode ~= 'restore' then
            return -3
        end

        local function parse_record(value)
            if not value then
                return nil, nil
            end
            local kind, raw_tokens = string.match(value, '^([rt]):(%d+)$')
            if not kind then
                return nil, nil
            end
            return kind, tonumber(raw_tokens)
        end

        local function prune_expired()
            local raw_total = redis.call('GET', total_key)
            local record_count = redis.call('HLEN', records_key)
            local expiration_count = redis.call('ZCARD', expirations_key)
            if not raw_total then
                if record_count == 0 and expiration_count == 0 then
                    return 0
                end
                return nil
            end
            if not string.match(raw_total, '^%d+$') then
                return nil
            end
            local total = tonumber(raw_total)
            if not total then
                return nil
            end

            local expired = redis.call('ZRANGEBYSCORE', expirations_key, '-inf', now)
            local expired_total = 0
            local expired_records = {}
            for _, request_id in ipairs(expired) do
                local value = redis.call('HGET', records_key, request_id)
                if value then
                    local kind, record_tokens = parse_record(value)
                    if not kind then
                        return nil
                    end
                    expired_total = expired_total + record_tokens
                    expired_records[#expired_records + 1] = request_id
                end
            end
            if expired_total > total then
                return nil
            end

            if #expired > 0 then
                redis.call('ZREMRANGEBYSCORE', expirations_key, '-inf', now)
                for _, request_id in ipairs(expired_records) do
                    redis.call('HDEL', records_key, request_id)
                end
                total = total - expired_total
            end

            record_count = redis.call('HLEN', records_key)
            expiration_count = redis.call('ZCARD', expirations_key)
            if record_count ~= expiration_count then
                return nil
            end
            if record_count == 0 then
                if total ~= 0 then
                    return nil
                end
                redis.call('DEL', records_key, expirations_key, total_key)
            else
                redis.call('SET', total_key, tostring(total))
                redis.call('EXPIRE', records_key, expire_secs)
                redis.call('EXPIRE', expirations_key, expire_secs)
                redis.call('EXPIRE', total_key, expire_secs)
            end
            return total
        end

        local total = prune_expired()
        if not total then
            return -3
        end

        local terminal = redis.call('HGET', records_key, terminal_id)
        if terminal then
            local terminal_kind = parse_record(terminal)
            if not terminal_kind then
                return -3
            end
            if terminal_kind == 't' then
                return -1
            end
        end

        local existing = redis.call('HGET', records_key, reservation_id)
        if existing then
            local existing_kind, existing_tokens = parse_record(existing)
            if not existing_kind then
                return -3
            end
            if existing_kind == 't' then
                return -1
            end
            if existing_kind == 'r' and existing_tokens == tokens then
                if mode ~= 'reserve' then
                    redis.call('ZADD', expirations_key, now + window_secs, reservation_id)
                    -- Refresh the physical Redis keys together with the logical
                    -- lease. Otherwise an active heartbeat could extend the
                    -- ZSET score past the keys' own eviction deadline.
                    redis.call('EXPIRE', records_key, expire_secs)
                    redis.call('EXPIRE', expirations_key, expire_secs)
                    redis.call('EXPIRE', total_key, expire_secs)
                end
                return 1
            end
            return -2
        end
        if mode == 'renew' then
            return 2
        end
        if mode == 'reserve' and total + tokens > limit then
            return 0
        end

        total = total + tokens
        redis.call('HSET', records_key, reservation_id, 'r:' .. tostring(tokens))
        redis.call('ZADD', expirations_key, now + window_secs, reservation_id)
        redis.call('SET', total_key, tostring(total))
        redis.call('EXPIRE', records_key, expire_secs)
        redis.call('EXPIRE', expirations_key, expire_secs)
        redis.call('EXPIRE', total_key, expire_secs)
        return 1
    "#;

    /// Atomically replace a pending reservation with actual terminal usage, or
    /// insert terminal usage when no reservation exists. A repeated terminal
    /// replay is a no-op, including the zero-token case.
    const RECONCILE_TOKENS_SCRIPT: &str = r#"
        local records_key = KEYS[1]
        local expirations_key = KEYS[2]
        local total_key = KEYS[3]
        local redis_time = redis.call('TIME')
        local now = tonumber(redis_time[1])
        local occurred_at = math.min(tonumber(ARGV[1]), now)
        local window_secs = tonumber(ARGV[2])
        local window_start = now - window_secs
        local reservation_id = ARGV[3]
        local terminal_id = ARGV[4]
        local tokens = tonumber(ARGV[5])
        local expire_secs = tonumber(ARGV[6])

        local function parse_record(value)
            if not value then
                return nil, nil
            end
            local kind, raw_tokens = string.match(value, '^([rt]):(%d+)$')
            if not kind then
                return nil, nil
            end
            return kind, tonumber(raw_tokens)
        end

        local function prune_expired()
            local raw_total = redis.call('GET', total_key)
            local record_count = redis.call('HLEN', records_key)
            local expiration_count = redis.call('ZCARD', expirations_key)
            if not raw_total then
                if record_count == 0 and expiration_count == 0 then
                    return 0
                end
                return nil
            end
            if not string.match(raw_total, '^%d+$') then
                return nil
            end
            local total = tonumber(raw_total)
            if not total then
                return nil
            end

            local expired = redis.call('ZRANGEBYSCORE', expirations_key, '-inf', now)
            local expired_total = 0
            local expired_records = {}
            for _, request_id in ipairs(expired) do
                local value = redis.call('HGET', records_key, request_id)
                if value then
                    local kind, record_tokens = parse_record(value)
                    if not kind then
                        return nil
                    end
                    expired_total = expired_total + record_tokens
                    expired_records[#expired_records + 1] = request_id
                end
            end
            if expired_total > total then
                return nil
            end

            if #expired > 0 then
                redis.call('ZREMRANGEBYSCORE', expirations_key, '-inf', now)
                for _, request_id in ipairs(expired_records) do
                    redis.call('HDEL', records_key, request_id)
                end
                total = total - expired_total
            end

            record_count = redis.call('HLEN', records_key)
            expiration_count = redis.call('ZCARD', expirations_key)
            if record_count ~= expiration_count then
                return nil
            end
            if record_count == 0 then
                if total ~= 0 then
                    return nil
                end
                redis.call('DEL', records_key, expirations_key, total_key)
            else
                redis.call('SET', total_key, tostring(total))
                redis.call('EXPIRE', records_key, expire_secs)
                redis.call('EXPIRE', expirations_key, expire_secs)
                redis.call('EXPIRE', total_key, expire_secs)
            end
            return total
        end

        local function persist(total)
            local record_count = redis.call('HLEN', records_key)
            if record_count ~= redis.call('ZCARD', expirations_key) or total < 0 then
                return false
            end
            if record_count == 0 then
                if total ~= 0 then
                    return false
                end
                redis.call('DEL', records_key, expirations_key, total_key)
            else
                redis.call('SET', total_key, tostring(total))
                redis.call('EXPIRE', records_key, expire_secs)
                redis.call('EXPIRE', expirations_key, expire_secs)
                redis.call('EXPIRE', total_key, expire_secs)
            end
            return true
        end

        local total = prune_expired()
        if not total then
            return -2
        end

        local reservation = redis.call('HGET', records_key, reservation_id)
        local reservation_kind = nil
        local reservation_tokens = nil
        if reservation then
            reservation_kind, reservation_tokens = parse_record(reservation)
            if not reservation_kind then
                return -2
            end
            -- A live or restored prediction is not a new terminal occurrence.
            -- Keep the caller's future-clamped timestamp so recovery cannot
            -- restart the logical request's quota window.
        end

        local terminal = redis.call('HGET', records_key, terminal_id)
        if reservation_id == terminal_id and reservation_kind == 'r' then
            -- The owned reservation will be removed before the terminal lookup.
            terminal = nil
        end
        local terminal_kind = nil
        if terminal then
            terminal_kind = parse_record(terminal)
            if not terminal_kind then
                return -2
            end
        end

        if reservation_kind == 'r' then
            if reservation_tokens > total then
                return -2
            end
            if redis.call('HLEN', records_key) == 1 and total ~= reservation_tokens then
                return -2
            end
            redis.call('HDEL', records_key, reservation_id)
            redis.call('ZREM', expirations_key, reservation_id)
            total = total - reservation_tokens
        end

        if terminal_kind == 't' then
            if not persist(total) then
                return -2
            end
            return 0
        end
        if terminal_kind == 'r' then
            if not persist(total) then
                return -2
            end
            return -1
        end

        -- Delayed settlement outside the active horizon releases any still-live
        -- prediction but must not shift old usage into the current window.
        if occurred_at <= window_start then
            if not persist(total) then
                return -2
            end
            return 0
        end

        total = total + tokens
        redis.call('HSET', records_key, terminal_id, 't:' .. tostring(tokens))
        redis.call('ZADD', expirations_key, occurred_at + window_secs, terminal_id)
        if not persist(total) then
            return -2
        end
        return 1
    "#;

    /// Idempotently release a still-pending reservation. Terminal usage is an
    /// immutable dedupe tombstone for the remainder of its TPM window.
    const RELEASE_TOKENS_SCRIPT: &str = r#"
        local records_key = KEYS[1]
        local expirations_key = KEYS[2]
        local total_key = KEYS[3]
        local redis_time = redis.call('TIME')
        local now = tonumber(redis_time[1])
        local request_id = ARGV[1]
        local expire_secs = tonumber(ARGV[2])

        local function parse_record(value)
            if not value then
                return nil, nil
            end
            local kind, raw_tokens = string.match(value, '^([rt]):(%d+)$')
            if not kind then
                return nil, nil
            end
            return kind, tonumber(raw_tokens)
        end

        local function prune_expired()
            local raw_total = redis.call('GET', total_key)
            local record_count = redis.call('HLEN', records_key)
            local expiration_count = redis.call('ZCARD', expirations_key)
            if not raw_total then
                if record_count == 0 and expiration_count == 0 then
                    return 0
                end
                return nil
            end
            if not string.match(raw_total, '^%d+$') then
                return nil
            end
            local total = tonumber(raw_total)
            if not total then
                return nil
            end

            local expired = redis.call('ZRANGEBYSCORE', expirations_key, '-inf', now)
            local expired_total = 0
            local expired_records = {}
            for _, expired_id in ipairs(expired) do
                local value = redis.call('HGET', records_key, expired_id)
                if value then
                    local kind, record_tokens = parse_record(value)
                    if not kind then
                        return nil
                    end
                    expired_total = expired_total + record_tokens
                    expired_records[#expired_records + 1] = expired_id
                end
            end
            if expired_total > total then
                return nil
            end

            if #expired > 0 then
                redis.call('ZREMRANGEBYSCORE', expirations_key, '-inf', now)
                for _, expired_id in ipairs(expired_records) do
                    redis.call('HDEL', records_key, expired_id)
                end
                total = total - expired_total
            end

            record_count = redis.call('HLEN', records_key)
            expiration_count = redis.call('ZCARD', expirations_key)
            if record_count ~= expiration_count then
                return nil
            end
            if record_count == 0 then
                if total ~= 0 then
                    return nil
                end
                redis.call('DEL', records_key, expirations_key, total_key)
            else
                redis.call('SET', total_key, tostring(total))
                redis.call('EXPIRE', records_key, expire_secs)
                redis.call('EXPIRE', expirations_key, expire_secs)
                redis.call('EXPIRE', total_key, expire_secs)
            end
            return total
        end

        local total = prune_expired()
        if not total then
            return -1
        end

        local existing = redis.call('HGET', records_key, request_id)
        if not existing then
            return 0
        end
        local kind, tokens = parse_record(existing)
        if not kind then
            return -1
        end
        if kind == 't' then
            return 0
        end
        if tokens > total then
            return -1
        end
        if redis.call('HLEN', records_key) == 1 and total ~= tokens then
            return -1
        end

        redis.call('HDEL', records_key, request_id)
        redis.call('ZREM', expirations_key, request_id)
        total = total - tokens
        local record_count = redis.call('HLEN', records_key)
        if record_count ~= redis.call('ZCARD', expirations_key) then
            return -1
        end
        if record_count == 0 then
            if total ~= 0 then
                return -1
            end
            redis.call('DEL', records_key, expirations_key, total_key)
        else
            redis.call('SET', total_key, tostring(total))
            redis.call('EXPIRE', records_key, expire_secs)
            redis.call('EXPIRE', expirations_key, expire_secs)
            redis.call('EXPIRE', total_key, expire_secs)
        end
        return 1
    "#;

    /// Lua 脚本：获取当前窗口 Token 总和
    /// Prune only expired request records and return the maintained aggregate.
    const GET_TOKEN_COUNT_SCRIPT: &str = r#"
        local records_key = KEYS[1]
        local expirations_key = KEYS[2]
        local total_key = KEYS[3]
        local redis_time = redis.call('TIME')
        local now = tonumber(redis_time[1])
        local expire_secs = tonumber(ARGV[1])

        local function parse_record(value)
            if not value then
                return nil
            end
            local kind, raw_tokens = string.match(value, '^([rt]):(%d+)$')
            if not kind then
                return nil
            end
            return tonumber(raw_tokens)
        end

        local raw_total = redis.call('GET', total_key)
        local record_count = redis.call('HLEN', records_key)
        local expiration_count = redis.call('ZCARD', expirations_key)
        if not raw_total then
            if record_count == 0 and expiration_count == 0 then
                return 0
            end
            return -1
        end
        if not string.match(raw_total, '^%d+$') then
            return -1
        end
        local total = tonumber(raw_total)
        if not total then
            return -1
        end

        local expired = redis.call('ZRANGEBYSCORE', expirations_key, '-inf', now)
        local expired_total = 0
        local expired_records = {}
        for _, request_id in ipairs(expired) do
            local value = redis.call('HGET', records_key, request_id)
            if value then
                local record_tokens = parse_record(value)
                if not record_tokens then
                    return -1
                end
                expired_total = expired_total + record_tokens
                expired_records[#expired_records + 1] = request_id
            end
        end
        if expired_total > total then
            return -1
        end

        if #expired > 0 then
            redis.call('ZREMRANGEBYSCORE', expirations_key, '-inf', now)
            for _, request_id in ipairs(expired_records) do
                redis.call('HDEL', records_key, request_id)
            end
            total = total - expired_total
        end

        record_count = redis.call('HLEN', records_key)
        expiration_count = redis.call('ZCARD', expirations_key)
        if record_count ~= expiration_count then
            return -1
        end
        if record_count == 0 then
            if total ~= 0 then
                return -1
            end
            redis.call('DEL', records_key, expirations_key, total_key)
        else
            redis.call('SET', total_key, tostring(total))
            redis.call('EXPIRE', records_key, expire_secs)
            redis.call('EXPIRE', expirations_key, expire_secs)
            redis.call('EXPIRE', total_key, expire_secs)
        end

        return total
    "#;
}

#[async_trait]
impl RateLimiter for RedisRateLimiter {
    async fn check(&self, key: &RateLimitKey) -> Result<bool> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_rpm_key(key);
        let count = Self::window_count(&mut conn, &redis_key, self.window_size).await?;
        Ok(count < DEFAULT_RPM_LIMIT as u64)
    }

    async fn check_with_config(
        &self,
        key: &RateLimitKey,
        config: &crate::RateLimitConfig,
    ) -> Result<bool> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_rpm_key(key);
        let count = Self::window_count(&mut conn, &redis_key, self.window_size).await?;
        Ok(count < config.rpm_limit as u64)
    }

    async fn record(&self, key: &RateLimitKey) -> Result<()> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_rpm_key(key);

        let now = Self::now_timestamp();

        // 使用 UUID 作为唯一成员，避免同一秒内的请求被去重
        let unique_member = format!("{}:{}", now, Uuid::new_v4().simple());

        let _: () = conn
            .zadd(&redis_key, &unique_member, now)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        let _: () = conn
            .expire(&redis_key, self.expire_secs())
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        Ok(())
    }

    async fn check_and_record_with_config(
        &self,
        key: &RateLimitKey,
        config: &crate::RateLimitConfig,
    ) -> Result<()> {
        let redis_key = self.build_rpm_key(key);

        let now = Self::now_timestamp();

        let window_start = now - self.window_size.as_secs() as i64;
        let unique_member = format!("{}:{}", now, Uuid::new_v4().simple());

        // 使用 Lua 脚本原子执行检查和记录
        let mut invocation = Self::check_and_record_script().prepare_invoke();
        invocation
            .key(redis_key)
            .arg(now)
            .arg(window_start)
            .arg(config.rpm_limit as i64)
            .arg(&unique_member)
            .arg(self.expire_secs());
        let result = self.invoke_script(invocation).await?;

        if result == 1 {
            Ok(())
        } else {
            Err(KeyComputeError::RateLimitExceeded(format!(
                "Redis rate limit exceeded for tenant {}",
                key.tenant_id
            )))
        }
    }

    async fn record_tokens(&self, key: &RateLimitKey, tokens: u32) -> Result<()> {
        self.record_tokens_once_at(key, Uuid::new_v4(), tokens, SystemTime::now())
            .await
    }

    async fn reserve_tokens(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        predicted_tokens: u32,
        limit: u32,
    ) -> Result<()> {
        let result = self
            .mutate_token_reservation(
                key,
                reservation_id,
                terminal_id,
                predicted_tokens,
                limit,
                "reserve",
            )
            .await?;

        match result {
            1 => Ok(()),
            0 => Err(KeyComputeError::RateLimitExceeded(format!(
                "TPM limit exceeded for tenant {} (limit: {}, requested: {})",
                key.tenant_id, limit, predicted_tokens
            ))),
            -1 => Err(KeyComputeError::Internal(format!(
                "TPM logical request {terminal_id} already has terminal usage"
            ))),
            -2 => Err(KeyComputeError::Internal(format!(
                "TPM reservation {reservation_id} prediction changed"
            ))),
            -3 => Err(KeyComputeError::Internal(format!(
                "Redis TPM state is inconsistent for tenant {}",
                key.tenant_id
            ))),
            other => Err(KeyComputeError::Internal(format!(
                "Unexpected Redis TPM reservation result: {other}"
            ))),
        }
    }

    async fn renew_token_reservation(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        predicted_tokens: u32,
    ) -> Result<bool> {
        let result = self
            .mutate_token_reservation(
                key,
                reservation_id,
                terminal_id,
                predicted_tokens,
                0,
                "renew",
            )
            .await?;
        match result {
            1 => Ok(true),
            -1 | 2 => Ok(false),
            -2 => Err(KeyComputeError::Internal(format!(
                "TPM reservation {reservation_id} prediction changed"
            ))),
            -3 => Err(KeyComputeError::Internal(format!(
                "Redis TPM state is inconsistent for tenant {}",
                key.tenant_id
            ))),
            other => Err(KeyComputeError::Internal(format!(
                "Unexpected Redis TPM renewal result: {other}"
            ))),
        }
    }

    async fn restore_token_reservation(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        predicted_tokens: u32,
    ) -> Result<bool> {
        let result = self
            .mutate_token_reservation(
                key,
                reservation_id,
                terminal_id,
                predicted_tokens,
                0,
                "restore",
            )
            .await?;
        match result {
            1 => Ok(true),
            -1 => Ok(false),
            -2 => Err(KeyComputeError::Internal(format!(
                "TPM reservation {reservation_id} prediction changed"
            ))),
            -3 => Err(KeyComputeError::Internal(format!(
                "Redis TPM state is inconsistent for tenant {}",
                key.tenant_id
            ))),
            other => Err(KeyComputeError::Internal(format!(
                "Unexpected Redis TPM durable restore result: {other}"
            ))),
        }
    }

    async fn release_token_reservation(&self, key: &RateLimitKey, request_id: Uuid) -> Result<()> {
        let mut invocation = self.prepare_tpm_script(key, Self::release_tokens_script());
        invocation
            .arg(request_id.to_string())
            .arg(self.expire_secs());
        let result = self.invoke_script(invocation).await?;

        match result {
            0 | 1 => Ok(()),
            -1 => Err(KeyComputeError::Internal(format!(
                "Redis TPM state is inconsistent for tenant {}",
                key.tenant_id
            ))),
            other => Err(KeyComputeError::Internal(format!(
                "Unexpected Redis TPM release result: {other}"
            ))),
        }
    }

    async fn reconcile_tokens_once_at(
        &self,
        key: &RateLimitKey,
        reservation_id: Uuid,
        terminal_id: Uuid,
        tokens: u32,
        occurred_at: SystemTime,
    ) -> Result<()> {
        let occurred_at = occurred_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .try_into()
            .unwrap_or(i64::MAX);
        let mut invocation = self.prepare_tpm_script(key, Self::reconcile_tokens_script());
        invocation
            .arg(occurred_at)
            .arg(self.window_size.as_secs() as i64)
            .arg(reservation_id.to_string())
            .arg(terminal_id.to_string())
            .arg(tokens as i64)
            .arg(self.expire_secs());
        let result = self.invoke_script(invocation).await?;
        match result {
            -1 => Err(KeyComputeError::Internal(format!(
                "TPM terminal identity {terminal_id} conflicts with an active reservation"
            ))),
            -2 => Err(KeyComputeError::Internal(format!(
                "Redis TPM state is inconsistent for tenant {}",
                key.tenant_id
            ))),
            0 | 1 => Ok(()),
            other => Err(KeyComputeError::Internal(format!(
                "Unexpected Redis TPM reconciliation result: {other}"
            ))),
        }
    }

    async fn get_count(&self, key: &RateLimitKey) -> Result<u64> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_rpm_key(key);
        Self::window_count(&mut conn, &redis_key, self.window_size).await
    }

    /// 获取当前窗口 Token 总和
    ///
    /// NOTE: 作为副作用，此方法会清理过期条目并刷新 key 的 TTL，
    /// 防止活跃 key 被提前驱逐。
    async fn get_token_count(&self, key: &RateLimitKey) -> Result<u64> {
        let mut invocation = self.prepare_tpm_script(key, Self::get_token_count_script());
        invocation.arg(self.expire_secs());
        let count = self.invoke_script(invocation).await?;

        if count < 0 {
            Err(KeyComputeError::Internal(format!(
                "Redis TPM state is inconsistent for tenant {}",
                key.tenant_id
            )))
        } else {
            Ok(count as u64)
        }
    }
}

impl RedisRateLimiter {
    /// 清理所有限流数据（用于测试或重置）
    pub async fn flush_all(&self) -> Result<()> {
        let pattern = format!("{}:*", self.key_prefix);

        let mut keys = Vec::new();
        {
            let mut conn = self.get_conn().await?;
            let mut iter: deadpool_redis::redis::AsyncIter<String> = conn
                .scan_match(&pattern)
                .await
                .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

            while let Some(key) = iter.next_item().await {
                keys.push(key);
            }
        }

        if !keys.is_empty() {
            let mut conn = self.get_conn().await?;
            let _: () = conn
                .del(&keys)
                .await
                .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn redis_unavailable<T>(message: impl std::fmt::Display) -> Option<T> {
        if std::env::var_os("CI").is_some() {
            panic!("Redis is required in CI but unavailable: {message}");
        }
        eprintln!("Warning: Redis not available: {message}; skipping Redis test");
        None
    }

    async fn create_test_pool() -> Option<deadpool_redis::Pool> {
        let redis_url =
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let cfg = deadpool_redis::Config::from_url(redis_url);
        let pool = match cfg.create_pool(Some(deadpool_redis::Runtime::Tokio1)) {
            Ok(pool) => pool,
            Err(error) => return redis_unavailable(error),
        };
        // 验证实际连接可用
        let mut conn = match pool.get().await {
            Ok(conn) => conn,
            Err(error) => return redis_unavailable(error),
        };
        if let Err(error) = deadpool_redis::redis::cmd("PING")
            .query_async::<()>(&mut conn)
            .await
        {
            return redis_unavailable(error);
        }
        drop(conn);
        Some(pool)
    }

    async fn create_test_limiter() -> Option<RedisRateLimiter> {
        let pool = create_test_pool().await?;
        // Never use the production/default prefix in a test. REDIS_URL may
        // point at a shared developer instance, and flush_all deliberately
        // deletes every key under its limiter prefix.
        Some(RedisRateLimiter::with_prefix(
            pool,
            format!("test-ratelimit-{}", Uuid::new_v4()),
        ))
    }

    fn tpm_test_keys(limiter: &RedisRateLimiter, key: &RateLimitKey) -> [String; 3] {
        let (records, expirations, total) = limiter.build_tpm_keys(key);
        [records, expirations, total]
    }

    async fn set_tpm_key_ttls(limiter: &RedisRateLimiter, key: &RateLimitKey, ttl_millis: i64) {
        let mut conn = limiter.pool.get().await.unwrap();
        for redis_key in tpm_test_keys(limiter, key) {
            let updated: i64 = deadpool_redis::redis::cmd("PEXPIRE")
                .arg(redis_key)
                .arg(ttl_millis)
                .query_async(&mut conn)
                .await
                .unwrap();
            assert_eq!(updated, 1, "expected all TPM v3 keys to exist");
        }
    }

    async fn assert_tpm_key_ttls_refreshed(limiter: &RedisRateLimiter, key: &RateLimitKey) {
        let mut conn = limiter.pool.get().await.unwrap();
        let minimum_expected_millis = (limiter.expire_secs() - 5) * 1_000;
        for redis_key in tpm_test_keys(limiter, key) {
            let ttl: i64 = deadpool_redis::redis::cmd("PTTL")
                .arg(&redis_key)
                .query_async(&mut conn)
                .await
                .unwrap();
            assert!(
                ttl >= minimum_expected_millis,
                "TPM key {redis_key} TTL was not refreshed: {ttl}ms"
            );
        }
    }

    async fn assert_tpm_keys_absent(limiter: &RedisRateLimiter, key: &RateLimitKey) {
        let mut conn = limiter.pool.get().await.unwrap();
        for redis_key in tpm_test_keys(limiter, key) {
            let exists: i64 = deadpool_redis::redis::cmd("EXISTS")
                .arg(&redis_key)
                .query_async(&mut conn)
                .await
                .unwrap();
            assert_eq!(exists, 0, "empty TPM state left key {redis_key} behind");
        }
    }

    #[test]
    fn tpm_v3_scripts_never_scan_the_active_record_hash() {
        for script in [
            RedisRateLimiter::RESERVE_TOKENS_SCRIPT,
            RedisRateLimiter::RECONCILE_TOKENS_SCRIPT,
            RedisRateLimiter::RELEASE_TOKENS_SCRIPT,
            RedisRateLimiter::GET_TOKEN_COUNT_SCRIPT,
        ] {
            assert!(!script.contains("HGETALL"));
            assert!(script.contains("ZRANGEBYSCORE"));
        }
    }

    #[test]
    fn tpm_v3_scripts_share_the_redis_clock() {
        for script in [
            RedisRateLimiter::RESERVE_TOKENS_SCRIPT,
            RedisRateLimiter::RECONCILE_TOKENS_SCRIPT,
            RedisRateLimiter::RELEASE_TOKENS_SCRIPT,
            RedisRateLimiter::GET_TOKEN_COUNT_SCRIPT,
        ] {
            assert!(
                script.contains("redis.call('TIME')"),
                "distributed TPM state must not depend on an application node's clock"
            );
            assert!(
                !script.contains("local now = tonumber(ARGV"),
                "the current TPM window must be derived inside Redis"
            );
        }
    }

    #[test]
    fn lua_script_handles_are_process_cached() {
        type ScriptGetter = fn() -> &'static deadpool_redis::redis::Script;
        let scripts: [(ScriptGetter, &str); 5] = [
            (
                RedisRateLimiter::check_and_record_script,
                RedisRateLimiter::CHECK_AND_RECORD_SCRIPT,
            ),
            (
                RedisRateLimiter::reserve_tokens_script,
                RedisRateLimiter::RESERVE_TOKENS_SCRIPT,
            ),
            (
                RedisRateLimiter::reconcile_tokens_script,
                RedisRateLimiter::RECONCILE_TOKENS_SCRIPT,
            ),
            (
                RedisRateLimiter::release_tokens_script,
                RedisRateLimiter::RELEASE_TOKENS_SCRIPT,
            ),
            (
                RedisRateLimiter::get_token_count_script,
                RedisRateLimiter::GET_TOKEN_COUNT_SCRIPT,
            ),
        ];

        for (getter, source) in scripts {
            let first = getter();
            let second = getter();
            assert!(
                std::ptr::eq(first, second),
                "each hot-path Lua source should have one reusable Script handle"
            );
            assert_eq!(
                first.get_hash(),
                deadpool_redis::redis::Script::new(source).get_hash(),
                "the cached EVALSHA handle must represent the current Lua source"
            );
        }
    }

    #[test]
    fn tpm_renewal_script_refreshes_every_physical_key_ttl() {
        for key in ["records_key", "expirations_key", "total_key"] {
            let refresh = format!("redis.call('EXPIRE', {key}, expire_secs)");
            assert!(
                RedisRateLimiter::RESERVE_TOKENS_SCRIPT
                    .matches(&refresh)
                    .count()
                    >= 3,
                "renewal, pruning, and insertion paths must all refresh {key}"
            );
        }
    }

    #[test]
    fn tpm_v3_keys_share_one_redis_cluster_hash_slot() {
        fn hash_tag(redis_key: &str) -> &str {
            let start = redis_key.find('{').unwrap() + 1;
            let end = redis_key[start..].find('}').unwrap() + start;
            &redis_key[start..end]
        }

        let cfg = deadpool_redis::Config::from_url("redis://127.0.0.1:6379");
        let pool = cfg
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .unwrap();
        let limiter = RedisRateLimiter::with_prefix(pool, "cluster-slot-test");
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let keys = tpm_test_keys(&limiter, &key);

        assert_eq!(hash_tag(&keys[0]), hash_tag(&keys[1]));
        assert_eq!(hash_tag(&keys[0]), hash_tag(&keys[2]));
        assert!(keys.iter().all(|key| key.contains(":tpm-v3:")));
    }

    #[tokio::test]
    async fn test_redis_rate_limiter_check_and_record() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };

        let _ = limiter.flush_all().await;

        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        assert!(limiter.check(&key).await.unwrap());

        limiter.record(&key).await.unwrap();

        assert!(limiter.check(&key).await.unwrap());
    }

    #[tokio::test]
    async fn test_redis_tpm_token_recording() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };

        let _ = limiter.flush_all().await;

        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        // 初始 token 计数应为 0
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(count, 0, "Initial token count should be 0");

        // 记录 100 tokens
        limiter.record_tokens(&key, 100).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 100,
            "After recording 100 tokens, count should be 100"
        );

        // 再记录 50 tokens
        limiter.record_tokens(&key, 50).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 150,
            "After recording 50 more tokens, count should be 150"
        );
    }

    #[tokio::test]
    async fn test_redis_tpm_terminal_recording_is_idempotent() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };
        let _ = limiter.flush_all().await;
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let request_id = Uuid::new_v4();

        limiter
            .record_tokens_once(&key, request_id, 100)
            .await
            .unwrap();
        limiter
            .record_tokens_once(&key, request_id, 100)
            .await
            .unwrap();

        assert_eq!(limiter.get_token_count(&key).await.unwrap(), 100);
    }

    #[tokio::test]
    async fn test_redis_late_terminal_tokens_keep_original_window_boundary() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };
        let _ = limiter.flush_all().await;
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let request_id = Uuid::new_v4();
        let occurred_at = SystemTime::now()
            .checked_sub(Duration::from_secs(WINDOW_SECS - 1))
            .unwrap();

        limiter
            .record_tokens_once_at(&key, request_id, 100, occurred_at)
            .await
            .unwrap();
        assert_eq!(limiter.get_token_count(&key).await.unwrap(), 100);

        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(limiter.get_token_count(&key).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn redis_live_reservation_reconcile_keeps_original_window_boundary() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };
        let _ = limiter.flush_all().await;
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let reservation_id = Uuid::new_v4();
        let terminal_id = Uuid::new_v4();
        let occurred_at = SystemTime::now()
            .checked_sub(Duration::from_secs(WINDOW_SECS - 1))
            .unwrap();

        limiter
            .reserve_tokens(&key, reservation_id, terminal_id, 80, 100)
            .await
            .unwrap();
        limiter
            .reconcile_tokens_once_at(&key, reservation_id, terminal_id, 30, occurred_at)
            .await
            .unwrap();
        assert_eq!(
            limiter.get_token_count(&key).await.unwrap(),
            30,
            "a live prediction should be replaced by actual terminal usage"
        );
        assert!(
            !limiter
                .restore_token_reservation(&key, reservation_id, terminal_id, 80)
                .await
                .unwrap(),
            "the terminal record must fence a restore inside its original window"
        );

        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(
            limiter.get_token_count(&key).await.unwrap(),
            0,
            "reconciliation must not restart a full window at replay time"
        );
    }

    #[tokio::test]
    async fn test_redis_tpm_edge_cases() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };

        let _ = limiter.flush_all().await;

        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        // 零 token 记录
        limiter.record_tokens(&key, 0).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 0,
            "After recording 0 tokens, count should still be 0"
        );

        // 单 token 记录
        limiter.record_tokens(&key, 1).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(count, 1, "After recording 1 token, count should be 1");

        // 大 token 值（接近 u32::MAX）
        limiter.record_tokens(&key, 999_999_999).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 1_000_000_000,
            "After recording 999999999 tokens, count should be 1000000000"
        );
    }

    #[tokio::test]
    async fn test_redis_tpm_boundary() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };

        let _ = limiter.flush_all().await;

        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let limit: u64 = 100;

        // 记录恰好 limit-1 tokens
        limiter.record_tokens(&key, 99).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert!(
            count < limit,
            "99 tokens < 100 limit, should be below limit"
        );

        // 再记录 1 token → 达到 limit
        limiter.record_tokens(&key, 1).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(count, limit, "100 tokens = 100 limit, should exactly match");

        // 再记录 1 token → 超出 limit
        limiter.record_tokens(&key, 1).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert!(count > limit, "101 tokens > 100 limit, should exceed limit");
    }

    #[tokio::test]
    async fn test_redis_tpm_key_isolation() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };

        let _ = limiter.flush_all().await;

        // 两个不同的 tenant 共享同一 Redis 限流器（共享同一连接池）
        let tenant_a_key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let tenant_b_key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        // Tenant A 记录 200 tokens
        limiter.record_tokens(&tenant_a_key, 200).await.unwrap();
        let count_a = limiter.get_token_count(&tenant_a_key).await.unwrap();
        assert_eq!(count_a, 200, "Tenant A should have 200 tokens");

        // Tenant B 的 token 计数应为 0（完全隔离）
        let count_b = limiter.get_token_count(&tenant_b_key).await.unwrap();
        assert_eq!(
            count_b, 0,
            "Tenant B should have 0 tokens (isolated from A)"
        );

        // Tenant B 记录 50 tokens，不影响 Tenant A
        limiter.record_tokens(&tenant_b_key, 50).await.unwrap();
        let count_b = limiter.get_token_count(&tenant_b_key).await.unwrap();
        assert_eq!(count_b, 50, "Tenant B should have 50 tokens");
        let count_a = limiter.get_token_count(&tenant_a_key).await.unwrap();
        assert_eq!(count_a, 200, "Tenant A should still have 200 tokens");
    }

    #[tokio::test]
    async fn test_redis_tpm_concurrent_recording() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };

        let _ = limiter.flush_all().await;

        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let limiter = std::sync::Arc::new(limiter);

        // 并发记录不同 token 值，验证 Lua 脚本原子性
        let mut handles = Vec::new();
        for i in 0..10 {
            let limiter = std::sync::Arc::clone(&limiter);
            let key = key.clone();
            handles.push(tokio::spawn(async move {
                limiter.record_tokens(&key, (i + 1) * 10).await.unwrap();
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        // 总和应为 10+20+...+100 = 550
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 550,
            "After concurrent recording of 10+20+...+100, count should be 550, got {}",
            count
        );
    }

    /// 测试 TPM 滑动窗口时间边界
    ///
    /// 验证窗口外的过期 ZSET/HASH 条目被 GET_TOKEN_COUNT_SCRIPT
    /// 正确排除，窗口内的 token 被正确计入。
    #[tokio::test]
    async fn test_redis_tpm_window_boundary() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };

        let _ = limiter.flush_all().await;

        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (records_key, expirations_key, total_key) = limiter.build_tpm_keys(&key);
        let expired_id = Uuid::new_v4().to_string();

        // 通过直接 Redis 连接注入一条内部一致、但已超出窗口的数据。
        {
            let mut conn = limiter.pool.get().await.unwrap();
            let _: () = deadpool_redis::redis::cmd("HSET")
                .arg(&records_key)
                .arg(&expired_id)
                .arg("t:999")
                .query_async(&mut conn)
                .await
                .unwrap();
            let _: () = deadpool_redis::redis::cmd("ZADD")
                .arg(&expirations_key)
                .arg(1_000_000)
                .arg(&expired_id)
                .query_async(&mut conn)
                .await
                .unwrap();
            let _: () = deadpool_redis::redis::cmd("SET")
                .arg(&total_key)
                .arg(999)
                .query_async(&mut conn)
                .await
                .unwrap();
        }

        // get_token_count 应排除窗口外的 999 tokens，返回 0
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 0,
            "Window-expired tokens should be excluded from count"
        );

        // 记录 100 tokens 在当前窗口内
        limiter.record_tokens(&key, 100).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 100,
            "Current window tokens should be counted correctly"
        );

        // 确认窗口外旧条目仍被排除，仅窗口内 token 被计入
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 100,
            "Only window-in tokens should be counted after re-check"
        );
    }

    #[tokio::test]
    async fn redis_tpm_v3_refreshes_ttl_on_every_live_state_return_path() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let physical_id = Uuid::new_v4();
        let logical_id = Uuid::new_v4();

        limiter
            .reserve_tokens(&key, physical_id, logical_id, 20, 100)
            .await
            .unwrap();
        assert_tpm_key_ttls_refreshed(&limiter, &key).await;

        // Successful heartbeat renewal refreshes both the logical lease score
        // and every physical key TTL.
        set_tpm_key_ttls(&limiter, &key, 1_000).await;
        assert!(
            limiter
                .renew_token_reservation(&key, physical_id, logical_id, 20)
                .await
                .unwrap()
        );
        assert_tpm_key_ttls_refreshed(&limiter, &key).await;

        // Idempotent reservation early return.
        set_tpm_key_ttls(&limiter, &key, 1_000).await;
        limiter
            .reserve_tokens(&key, physical_id, logical_id, 20, 100)
            .await
            .unwrap();
        assert_tpm_key_ttls_refreshed(&limiter, &key).await;

        // Capacity rejection early return.
        set_tpm_key_ttls(&limiter, &key, 1_000).await;
        assert!(matches!(
            limiter
                .reserve_tokens(&key, Uuid::new_v4(), Uuid::new_v4(), 81, 100)
                .await,
            Err(KeyComputeError::RateLimitExceeded(_))
        ));
        assert_tpm_key_ttls_refreshed(&limiter, &key).await;

        // Missing release early return while another record keeps state live.
        set_tpm_key_ttls(&limiter, &key, 1_000).await;
        limiter
            .release_token_reservation(&key, Uuid::new_v4())
            .await
            .unwrap();
        assert_tpm_key_ttls_refreshed(&limiter, &key).await;

        // Reconcile to a zero-token terminal tombstone. The aggregate remains
        // zero, but all three keys must remain live to fence logical replay.
        set_tpm_key_ttls(&limiter, &key, 1_000).await;
        limiter
            .reconcile_tokens_once_at(&key, physical_id, logical_id, 0, SystemTime::now())
            .await
            .unwrap();
        assert_tpm_key_ttls_refreshed(&limiter, &key).await;
        {
            let (records_key, _, total_key) = limiter.build_tpm_keys(&key);
            let mut conn = limiter.pool.get().await.unwrap();
            let terminal: Option<String> = deadpool_redis::redis::cmd("HGET")
                .arg(records_key)
                .arg(logical_id.to_string())
                .query_async(&mut conn)
                .await
                .unwrap();
            let total: Option<String> = deadpool_redis::redis::cmd("GET")
                .arg(total_key)
                .query_async(&mut conn)
                .await
                .unwrap();
            assert_eq!(terminal.as_deref(), Some("t:0"));
            assert_eq!(total.as_deref(), Some("0"));
        }

        // Terminal reconcile replay, terminal release, and logical execution
        // rejection are all early returns that must preserve the tombstone TTL.
        set_tpm_key_ttls(&limiter, &key, 1_000).await;
        limiter
            .reconcile_tokens_once_at(&key, physical_id, logical_id, 99, SystemTime::now())
            .await
            .unwrap();
        assert_tpm_key_ttls_refreshed(&limiter, &key).await;

        set_tpm_key_ttls(&limiter, &key, 1_000).await;
        limiter
            .release_token_reservation(&key, logical_id)
            .await
            .unwrap();
        assert_tpm_key_ttls_refreshed(&limiter, &key).await;

        set_tpm_key_ttls(&limiter, &key, 1_000).await;
        assert!(
            limiter
                .reserve_tokens(&key, Uuid::new_v4(), logical_id, 1, 100)
                .await
                .is_err()
        );
        assert_tpm_key_ttls_refreshed(&limiter, &key).await;

        // Removing an active reservation refreshes the remaining tombstone.
        let expiring_physical = Uuid::new_v4();
        limiter
            .reserve_tokens(&key, expiring_physical, Uuid::new_v4(), 7, 100)
            .await
            .unwrap();
        set_tpm_key_ttls(&limiter, &key, 1_000).await;
        limiter
            .release_token_reservation(&key, expiring_physical)
            .await
            .unwrap();
        assert_tpm_key_ttls_refreshed(&limiter, &key).await;

        // Expiry pruning subtracts only the expired record from the aggregate
        // and refreshes the surviving zero-token terminal tombstone.
        let pruned_physical = Uuid::new_v4();
        limiter
            .reserve_tokens(&key, pruned_physical, Uuid::new_v4(), 7, 100)
            .await
            .unwrap();
        {
            let (_, expirations_key, _) = limiter.build_tpm_keys(&key);
            let mut conn = limiter.pool.get().await.unwrap();
            let _: () = deadpool_redis::redis::cmd("ZADD")
                .arg(expirations_key)
                .arg(RedisRateLimiter::now_timestamp() - 1)
                .arg(pruned_physical.to_string())
                .query_async(&mut conn)
                .await
                .unwrap();
        }
        set_tpm_key_ttls(&limiter, &key, 1_000).await;
        assert_eq!(limiter.get_token_count(&key).await.unwrap(), 0);
        assert_tpm_key_ttls_refreshed(&limiter, &key).await;

        // Once the final tombstone expires, pruning removes all three keys
        // rather than leaving an immortal zero aggregate behind.
        {
            let (_, expirations_key, _) = limiter.build_tpm_keys(&key);
            let mut conn = limiter.pool.get().await.unwrap();
            let _: () = deadpool_redis::redis::cmd("ZADD")
                .arg(expirations_key)
                .arg(RedisRateLimiter::now_timestamp() - 1)
                .arg(logical_id.to_string())
                .query_async(&mut conn)
                .await
                .unwrap();
        }
        assert_eq!(limiter.get_token_count(&key).await.unwrap(), 0);
        assert_tpm_keys_absent(&limiter, &key).await;
    }

    #[tokio::test]
    async fn redis_tpm_renewal_extends_only_a_matching_active_reservation() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let physical_id = Uuid::new_v4();
        let logical_id = Uuid::new_v4();
        limiter
            .reserve_tokens(&key, physical_id, logical_id, 20, 100)
            .await
            .unwrap();

        let (_, expirations_key, _) = limiter.build_tpm_keys(&key);
        let short_expiry = RedisRateLimiter::now_timestamp() + 1;
        {
            let mut conn = limiter.pool.get().await.unwrap();
            let _: () = deadpool_redis::redis::cmd("ZADD")
                .arg(&expirations_key)
                .arg(short_expiry)
                .arg(physical_id.to_string())
                .query_async(&mut conn)
                .await
                .unwrap();
        }

        assert!(
            limiter
                .renew_token_reservation(&key, physical_id, logical_id, 21)
                .await
                .is_err(),
            "a different prediction must fail closed"
        );
        {
            let mut conn = limiter.pool.get().await.unwrap();
            let expiry: i64 = deadpool_redis::redis::cmd("ZSCORE")
                .arg(&expirations_key)
                .arg(physical_id.to_string())
                .query_async(&mut conn)
                .await
                .unwrap();
            assert_eq!(expiry, short_expiry, "mismatch must not refresh the lease");
        }

        assert!(
            limiter
                .renew_token_reservation(&key, physical_id, logical_id, 20)
                .await
                .unwrap()
        );
        {
            let mut conn = limiter.pool.get().await.unwrap();
            let expiry: i64 = deadpool_redis::redis::cmd("ZSCORE")
                .arg(&expirations_key)
                .arg(physical_id.to_string())
                .query_async(&mut conn)
                .await
                .unwrap();
            assert!(
                expiry
                    >= RedisRateLimiter::now_timestamp() + i64::try_from(WINDOW_SECS).unwrap() - 1,
                "matching heartbeat did not restore a full lease"
            );
        }

        limiter
            .release_token_reservation(&key, physical_id)
            .await
            .unwrap();
        assert!(
            !limiter
                .renew_token_reservation(&key, physical_id, logical_id, 20)
                .await
                .unwrap(),
            "strict renewal must not resurrect a released reservation"
        );
    }

    #[tokio::test]
    async fn redis_tpm_durable_restore_is_unlimited_but_terminal_fenced() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        limiter
            .record_tokens_once(&key, Uuid::new_v4(), 90)
            .await
            .unwrap();

        let physical_id = Uuid::new_v4();
        let logical_id = Uuid::new_v4();
        assert!(
            limiter
                .restore_token_reservation(&key, physical_id, logical_id, 40)
                .await
                .unwrap()
        );
        assert_eq!(
            limiter.get_token_count(&key).await.unwrap(),
            130,
            "already-admitted durable work must be restored even above a new admission cap"
        );
        assert!(
            limiter
                .restore_token_reservation(&key, physical_id, logical_id, 41)
                .await
                .is_err()
        );
        assert_eq!(limiter.get_token_count(&key).await.unwrap(), 130);

        limiter
            .reconcile_tokens_once_at(&key, physical_id, logical_id, 10, SystemTime::now())
            .await
            .unwrap();
        assert!(
            !limiter
                .restore_token_reservation(&key, physical_id, logical_id, 40)
                .await
                .unwrap(),
            "terminal reconciliation must fence durable restore"
        );
        assert_eq!(limiter.get_token_count(&key).await.unwrap(), 100);
    }

    #[tokio::test]
    async fn redis_tpm_v3_corrupt_expired_record_fails_closed() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (records_key, expirations_key, total_key) = limiter.build_tpm_keys(&key);
        let request_id = Uuid::new_v4().to_string();
        let mut conn = limiter.pool.get().await.unwrap();
        let _: () = deadpool_redis::redis::cmd("HSET")
            .arg(records_key)
            .arg(&request_id)
            .arg("not-a-valid-record")
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = deadpool_redis::redis::cmd("ZADD")
            .arg(expirations_key)
            .arg(RedisRateLimiter::now_timestamp() - 1)
            .arg(request_id)
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = deadpool_redis::redis::cmd("SET")
            .arg(total_key)
            .arg(1)
            .query_async(&mut conn)
            .await
            .unwrap();
        drop(conn);

        let result = limiter.get_token_count(&key).await;
        assert!(matches!(result, Err(KeyComputeError::Internal(_))));
        limiter.flush_all().await.unwrap();
    }

    #[tokio::test]
    async fn redis_tpm_reservation_admission_is_atomic_under_concurrency() {
        let Some(pool) = create_test_pool().await else {
            return;
        };
        let limiter = std::sync::Arc::new(RedisRateLimiter::with_prefix(
            pool,
            format!("test-tpm-reserve-{}", Uuid::new_v4()),
        ));
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let admitted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut tasks = tokio::task::JoinSet::new();

        for _ in 0..20 {
            let limiter = std::sync::Arc::clone(&limiter);
            let key = key.clone();
            let admitted = std::sync::Arc::clone(&admitted);
            tasks.spawn(async move {
                let physical_id = Uuid::new_v4();
                if limiter
                    .reserve_tokens(&key, physical_id, physical_id, 10, 50)
                    .await
                    .is_ok()
                {
                    admitted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
        while tasks.join_next().await.is_some() {}

        assert_eq!(admitted.load(std::sync::atomic::Ordering::Relaxed), 5);
        assert_eq!(limiter.get_token_count(&key).await.unwrap(), 50);
        limiter.flush_all().await.unwrap();
    }

    #[tokio::test]
    async fn redis_tpm_release_and_reconcile_replace_only_the_owned_attempt() {
        let Some(pool) = create_test_pool().await else {
            return;
        };
        let limiter =
            RedisRateLimiter::with_prefix(pool, format!("test-tpm-reconcile-{}", Uuid::new_v4()));
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let logical_id = Uuid::new_v4();
        let old_attempt = Uuid::new_v4();
        let new_attempt = Uuid::new_v4();

        limiter
            .reserve_tokens(&key, old_attempt, logical_id, 40, 100)
            .await
            .unwrap();
        limiter
            .reserve_tokens(&key, new_attempt, logical_id, 40, 100)
            .await
            .unwrap();
        limiter
            .release_token_reservation(&key, old_attempt)
            .await
            .unwrap();
        assert_eq!(limiter.get_token_count(&key).await.unwrap(), 40);

        limiter
            .reconcile_tokens_once_at(&key, new_attempt, logical_id, 0, SystemTime::now())
            .await
            .unwrap();
        assert_eq!(limiter.get_token_count(&key).await.unwrap(), 0);
        limiter
            .reconcile_tokens_once_at(&key, new_attempt, logical_id, 99, SystemTime::now())
            .await
            .unwrap();
        assert_eq!(limiter.get_token_count(&key).await.unwrap(), 0);
        assert!(
            limiter
                .reserve_tokens(&key, Uuid::new_v4(), logical_id, 1, 100)
                .await
                .is_err(),
            "zero-token terminal state must fence replayed execution"
        );
        limiter.flush_all().await.unwrap();
    }

    /// 测试 Redis 不可用时 fail-closed 错误传播
    ///
    /// 验证当 Redis 连接池不可用时：
    /// - `check_and_record_with_config` 返回 `Err(KeyComputeError::Internal)` 而不是 `Err(RateLimitExceeded)`
    /// - `record_tokens` 返回 `Err`
    /// - `get_token_count` 返回 `Err`
    ///
    /// 这保证了 middleware 层（rate_limit_middleware / public_auth_rate_limit_middleware）
    /// 能通过 match 捕获到非 RateLimitExceeded 错误并返回 503（fail-closed）。
    #[tokio::test]
    async fn test_redis_unavailable_fail_closed() {
        // 使用一个不会连接成功的 Redis URL 创建池
        let cfg = deadpool_redis::Config::from_url("redis://127.0.0.1:16379/");
        let pool = match cfg.create_pool(Some(deadpool_redis::Runtime::Tokio1)) {
            Ok(p) => p,
            Err(_) => return, // 创建池本身不应失败，只是连接会超时
        };

        let limiter = RedisRateLimiter::new(pool);
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let config = crate::RateLimitConfig::new(10, 1000);

        // check_and_record_with_config 应返回 Internal 错误（不是 RateLimitExceeded）
        // 这是 fail-closed 的核心验证：Redis 不可用时禁止放行请求
        let result = limiter.check_and_record_with_config(&key, &config).await;
        match result {
            Err(KeyComputeError::Internal(_)) => {
                // 期望行为：Redis 连接失败，返回 Internal 错误
            }
            Err(KeyComputeError::RateLimitExceeded(_)) => {
                panic!(
                    "FAIL-CLOSED VIOLATION: Redis unavailable but got RateLimitExceeded, \
                     not Internal error. Request would have been incorrectly allowed through."
                );
            }
            Ok(_) => {
                panic!(
                    "FAIL-CLOSED VIOLATION: Redis unavailable but check_and_record succeeded. \
                     Request was allowed through without rate limiting."
                );
            }
            Err(other) => {
                // 其他错误类型也可以（只要不是 RateLimitExceeded）
                eprintln!("Got unexpected error type: {:?}", other);
            }
        }

        // record_tokens 也应返回错误（Redis 不可用时无法写入）
        let record_result = limiter.record_tokens(&key, 100).await;
        assert!(
            record_result.is_err(),
            "FAIL-CLOSED VIOLATION: Redis unavailable but record_tokens succeeded"
        );

        // get_token_count 也应返回错误（Redis 不可用时无法读取）
        let count_result = limiter.get_token_count(&key).await;
        assert!(
            count_result.is_err(),
            "FAIL-CLOSED VIOLATION: Redis unavailable but get_token_count succeeded"
        );
    }
}
