//! 读写分离数据库路由器 (DbRouter)
//!
//! 实现 SeaORM 的 `ConnectionTrait` 和 `TransactionTrait`，提供透明的一主多从读写分离。
//!
//! # 路由规则
//!
//! - `execute` / `execute_unprepared` → 始终路由到写库
//! - `query_one` / `query_all` → 路由到读库（带重试、熔断、回退到写库）
//! - `SELECT ... FOR UPDATE / FOR SHARE` → 强制到写库
//! - `begin` / `transaction` → 始终在写库
//!
//! # 退化模式
//!
//! 未配置读库时，`DbRouter` 透明退化为单库透传模式。
//!
//! # 参考
//!
//! 本实现参考 webshelf 项目的 `AutoRouter` 架构：
//! <https://github.com/aiqubits/webshelf/blob/main/server/src/utils/db_router.rs>

use async_trait::async_trait;
use parking_lot::Mutex;
use rand::Rng;
use sea_orm::{
    AccessMode, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DatabaseTransaction,
    DbBackend, DbErr, ExecResult, IsolationLevel, QueryResult, Statement, TransactionError,
    TransactionTrait,
};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

// ============================================================================
// 策略与状态
// ============================================================================

/// 读库选择策略
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReadStrategy {
    /// 轮询
    RoundRobin,
    /// 随机
    Random,
    /// 加权随机
    Weighted,
}

/// 单个读库副本
struct ReadReplica {
    conn: DatabaseConnection,
    weight: u32,
}

/// 熔断状态追踪
struct HealthState {
    down_until: Vec<Option<Instant>>,
}

// ============================================================================
// DbRouter 核心结构
// ============================================================================

/// 应用层数据库路由器
///
/// 实现 SeaORM 的 `ConnectionTrait` 和 `TransactionTrait`，自动将读请求分发到读库，
/// 写请求和事务分发到写库。支持熔断、重试、健康检查和多种读库选择策略。
pub struct DbRouter {
    /// 写库连接
    write: DatabaseConnection,
    /// 读库副本列表
    reads: Vec<ReadReplica>,
    /// 读库选择策略
    strategy: ReadStrategy,
    /// 轮询计数器
    rr_counter: AtomicUsize,
    /// 熔断状态
    health: Mutex<HealthState>,
    /// 熔断时长
    circuit_break: Duration,
    /// 额外重试次数
    retry_attempts: usize,
    /// 是否回退到写库
    fallback_to_write: bool,
}

/// 读库连接超时保护（秒）
const CONNECT_TIMEOUT_SECS: u64 = 30;

impl DbRouter {
    /// 创建多库路由器（一写 + N 读）
    pub async fn new(
        write_url: &str,
        read_urls: &[String],
        write_config: &DatabaseConfig,
        read_config: &DatabaseReadConfig,
        routing_config: &DatabaseRoutingConfig,
    ) -> Result<Arc<Self>, DbErr> {
        // 1. 连接写库
        let write = connect_db(write_url, write_config).await?;

        // 校验 weights 长度是否匹配
        if !routing_config.read_weights.is_empty()
            && routing_config.read_weights.len() != read_urls.len()
        {
            return Err(DbErr::Custom(format!(
                "read_weights length ({}) does not match database_read_urls count ({})",
                routing_config.read_weights.len(),
                read_urls.len(),
            )));
        }

        // 2. 并行连接读库
        let urls_with_idx: Vec<(usize, String)> = read_urls
            .iter()
            .enumerate()
            .map(|(i, u)| (i, u.clone()))
            .collect();
        let mut reads: Vec<ReadReplica> = Vec::with_capacity(urls_with_idx.len());
        let handles: Vec<_> = urls_with_idx
            .into_iter()
            .map(|(i, url)| {
                let cfg = read_config.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(
                        Duration::from_secs(CONNECT_TIMEOUT_SECS),
                        connect_db_read(&url, &cfg),
                    )
                    .await
                    {
                        Ok(result) => (i, result),
                        Err(_) => (
                            i,
                            Err(DbErr::Custom("read replica connection timed out".into())),
                        ),
                    }
                })
            })
            .collect();

        for handle in handles {
            match handle.await {
                Ok((i, Ok(conn))) => {
                    let weight = if !routing_config.read_weights.is_empty() {
                        routing_config
                            .read_weights
                            .get(i)
                            .copied()
                            .unwrap_or(1)
                            .max(1)
                    } else {
                        1
                    };
                    reads.push(ReadReplica { conn, weight });
                }
                Ok((i, Err(e))) => {
                    tracing::warn!("Read replica {} failed to connect: {}", i, e);
                }
                Err(e) => {
                    tracing::error!("Read replica connection task panicked: {:?}", e);
                }
            }
        }

        // 3. 无读库时退化到单库模式
        if reads.is_empty() {
            if read_urls.is_empty() {
                tracing::warn!("No read replicas configured — running in single-database mode");
            } else {
                return Err(DbErr::Custom(format!(
                    "All {} configured read replica(s) failed to connect — check KC__DATABASE_READ_URLS configuration",
                    read_urls.len(),
                )));
            }
            return Ok(Arc::new(Self::new_internal(
                write,
                Vec::new(),
                routing_config,
            )));
        }

        tracing::info!("DbRouter initialized with {} read replica(s)", reads.len());

        Ok(Arc::new(Self::new_internal(write, reads, routing_config)))
    }

    /// 创建单库路由器（无读写分离）
    pub fn single(write: DatabaseConnection) -> Arc<Self> {
        Arc::new(Self {
            write,
            reads: Vec::new(),
            strategy: ReadStrategy::RoundRobin,
            rr_counter: AtomicUsize::new(0),
            health: Mutex::new(HealthState {
                down_until: Vec::new(),
            }),
            circuit_break: Duration::from_secs(30),
            retry_attempts: 2,
            // 单库模式下 reads 为空，读请求在 execute_read_retry 中直接短路到写库；
            // 设为 true 确保语义上在未来代码变动时也能正确回退
            fallback_to_write: true,
        })
    }

    fn new_internal(
        write: DatabaseConnection,
        reads: Vec<ReadReplica>,
        routing: &DatabaseRoutingConfig,
    ) -> Self {
        let read_count = reads.len();
        let strategy = match routing.strategy.to_lowercase().as_str() {
            "random" => ReadStrategy::Random,
            "weighted" => ReadStrategy::Weighted,
            _ => ReadStrategy::RoundRobin,
        };
        Self {
            write,
            reads,
            strategy,
            rr_counter: AtomicUsize::new(0),
            health: Mutex::new(HealthState {
                down_until: vec![None; read_count],
            }),
            circuit_break: Duration::from_millis(routing.circuit_break_ms),
            retry_attempts: routing.retry_attempts,
            fallback_to_write: routing.fallback_to_write,
        }
    }

    /// 返回写库连接引用
    pub fn write_conn(&self) -> &DatabaseConnection {
        &self.write
    }

    /// 返回写库后端类型
    pub fn write_backend(&self) -> DbBackend {
        self.write.get_database_backend()
    }

    /// 启动后台健康检查任务
    pub fn start_health_check(self: Arc<Self>, interval: Duration) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                self.probe_reads().await;
            }
        });
    }

    async fn probe_reads(&self) {
        let to_probe: Vec<usize> = {
            let health = self.health.lock();
            let now = Instant::now();
            let mut result = Vec::new();
            for i in 0..self.reads.len() {
                if let Some(until) = health.down_until[i]
                    && now >= until
                {
                    result.push(i);
                }
            }
            result
        };

        for &i in &to_probe {
            if self.reads[i].conn.ping().await.is_ok() {
                let mut health = self.health.lock();
                // 防止 TOCTOU：仅在熔断窗口已过期且未被并发 mark_down 重新标记时恢复
                if let Some(until) = health.down_until[i]
                    && Instant::now() >= until
                {
                    health.down_until[i] = None;
                    tracing::info!("Read replica {} recovered", i);
                }
            } else {
                let mut health = self.health.lock();
                health.down_until[i] = Some(Instant::now() + self.circuit_break);
            }
        }
    }

    // ---- 内部路由 ----

    /// 选择下一个健康的读库索引（排除已尝试过的）
    fn pick_next_read(&self, exclude: &HashSet<usize>) -> Option<usize> {
        if self.reads.is_empty() {
            return None;
        }

        let now = Instant::now();
        let mut healthy: Vec<usize> = Vec::new();
        {
            let mut health = self.health.lock();
            for (i, _) in self.reads.iter().enumerate() {
                if exclude.contains(&i) {
                    continue;
                }
                // 自动恢复过期熔断
                if let Some(until) = health.down_until[i] {
                    if now >= until {
                        health.down_until[i] = None;
                    } else {
                        continue;
                    }
                }
                healthy.push(i);
            }
        }

        if healthy.is_empty() {
            return None;
        }

        let chosen = match self.strategy {
            ReadStrategy::RoundRobin => {
                // 使用 Relaxed ordering：对于负载轮询分配不要求严格一致性，
                // 允许线程间计数器值轻微乱序反而有助于分散"惊群"效应
                let idx = self.rr_counter.fetch_add(1, Ordering::Relaxed);
                healthy[idx % healthy.len()]
            }
            ReadStrategy::Random => {
                let mut rng = rand::thread_rng();
                healthy[rng.gen_range(0..healthy.len())]
            }
            ReadStrategy::Weighted => {
                let weights: Vec<u32> = healthy.iter().map(|&i| self.reads[i].weight).collect();
                select_weighted_index(&healthy, &weights, &self.rr_counter)
            }
        };

        Some(chosen)
    }

    /// 执行带重试的读操作
    async fn execute_read_retry<T, F, Fut>(&self, stmt: Statement, op: F) -> Result<T, DbErr>
    where
        F: Fn(DatabaseConnection, Statement) -> Fut + Copy,
        Fut: std::future::Future<Output = Result<T, DbErr>>,
    {
        if self.reads.is_empty() {
            return op(self.write.clone(), stmt).await;
        }

        let mut tried: HashSet<usize> = HashSet::new();
        let mut last_err: Option<DbErr> = None;

        // Phase 1: 轮询每个未尝试过的读库
        for _ in 0..self.reads.len() {
            let Some(idx) = self.pick_next_read(&tried) else {
                break;
            };
            tried.insert(idx);

            match op(self.reads[idx].conn.clone(), stmt.clone()).await {
                Ok(v) => return Ok(v),
                Err(e) if is_connection_error(&e) => {
                    self.mark_down(idx);
                    tracing::warn!("Read replica {} failed, marked down: {}", idx, e);
                    last_err = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        // Phase 2: 额外重试回合
        if last_err.is_some() && self.retry_attempts > 0 {
            for retry in 0..self.retry_attempts {
                let idx = (tried.len() + retry) % self.reads.len();
                match op(self.reads[idx].conn.clone(), stmt.clone()).await {
                    Ok(v) => return Ok(v),
                    Err(e) if is_connection_error(&e) => {
                        self.mark_down(idx);
                        tracing::warn!(
                            "Read replica {} failed during retry, marked down: {}",
                            idx,
                            e
                        );
                        last_err = Some(e);
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        // Phase 3: 回退到写库
        if self.fallback_to_write {
            tracing::warn!("All read replicas failed — falling back to writer");
            return op(self.write.clone(), stmt).await;
        }

        Err(last_err.unwrap_or_else(|| DbErr::Custom("all read attempts exhausted".into())))
    }

    fn mark_down(&self, idx: usize) {
        let mut health = self.health.lock();
        if let Some(slot) = health.down_until.get_mut(idx) {
            *slot = Some(Instant::now() + self.circuit_break);
        }
    }
}

/// 从健康读库列表中按加权随机选择
fn select_weighted_index(healthy: &[usize], weights: &[u32], rr_counter: &AtomicUsize) -> usize {
    let total: u64 = weights.iter().map(|&w| w as u64).sum();
    if total == 0 {
        let idx = rr_counter.fetch_add(1, Ordering::Relaxed);
        healthy[idx % healthy.len()]
    } else {
        let mut rng = rand::thread_rng();
        let mut roll = rng.gen_range(0..total);
        let mut chosen = healthy[0];
        for (&idx, &w) in healthy.iter().zip(weights.iter()) {
            if roll < w as u64 {
                chosen = idx;
                break;
            }
            roll -= w as u64;
        }
        chosen
    }
}

// ============================================================================
// ConnectionTrait 实现
// ============================================================================

#[async_trait]
impl ConnectionTrait for DbRouter {
    fn get_database_backend(&self) -> DbBackend {
        self.write.get_database_backend()
    }

    async fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
        self.write.execute_unprepared(sql).await
    }

    fn support_returning(&self) -> bool {
        self.write.support_returning()
    }

    async fn execute(&self, stmt: Statement) -> Result<ExecResult, DbErr> {
        // 所有写操作（INSERT / UPDATE / DELETE）统一路由到写库。
        // 防御性检测：如果非写语句通过 execute() 路由，记录警告
        // 防止 SeaORM 版本升级或调用方误用时 SELECT 意外路由到写库
        if !is_write_statement(&stmt) {
            tracing::warn!(
                target: "write",
                sql = %stmt,
                "Non-write statement routed through execute() — routed to write pool; if this is a read query, use query_one/query_all"
            );
        }
        self.write.execute(stmt).await
    }

    async fn query_one(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
        if is_write_statement(&stmt) || is_locking_select(&stmt) || self.reads.is_empty() {
            tracing::trace!(target: "write", "query_one routed to write");
            return self.write.query_one(stmt).await;
        }
        tracing::trace!(target: "read", "query_one routed to read replicas");
        self.execute_read_retry(stmt, |conn, s| async move { conn.query_one(s).await })
            .await
    }

    async fn query_all(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
        if is_write_statement(&stmt) || is_locking_select(&stmt) || self.reads.is_empty() {
            tracing::trace!(target: "write", "query_all routed to write");
            return self.write.query_all(stmt).await;
        }
        tracing::trace!(target: "read", "query_all routed to read replicas");
        self.execute_read_retry(stmt, |conn, s| async move { conn.query_all(s).await })
            .await
    }

    fn is_mock_connection(&self) -> bool {
        false
    }
}

// ============================================================================
// TransactionTrait 实现
// ============================================================================

#[async_trait]
impl TransactionTrait for DbRouter {
    async fn begin(&self) -> Result<DatabaseTransaction, DbErr> {
        self.write.begin().await
    }

    async fn begin_with_config(
        &self,
        isolation_level: Option<IsolationLevel>,
        access_mode: Option<AccessMode>,
    ) -> Result<DatabaseTransaction, DbErr> {
        self.write
            .begin_with_config(isolation_level, access_mode)
            .await
    }

    async fn transaction<F, T, E>(&self, txn: F) -> Result<T, TransactionError<E>>
    where
        F: for<'c> FnOnce(
                &'c DatabaseTransaction,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'c>,
            > + Send,
        T: Send,
        E: std::fmt::Debug + std::fmt::Display + Send,
    {
        self.write.transaction(txn).await
    }

    async fn transaction_with_config<F, T, E>(
        &self,
        txn: F,
        isolation_level: Option<IsolationLevel>,
        access_mode: Option<AccessMode>,
    ) -> Result<T, TransactionError<E>>
    where
        F: for<'c> FnOnce(
                &'c DatabaseTransaction,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'c>,
            > + Send,
        T: Send,
        E: std::fmt::Debug + std::fmt::Display + Send,
    {
        self.write
            .transaction_with_config(txn, isolation_level, access_mode)
            .await
    }
}

// ============================================================================
// SQL 检测
// ============================================================================

/// 检测锁语句（FOR UPDATE / FOR SHARE 等）
fn is_locking_select(stmt: &Statement) -> bool {
    let sql = stmt.to_string();
    if sql.is_empty() {
        return false;
    }
    let up = sql.to_ascii_uppercase();
    up.contains("FOR UPDATE")
        || up.contains("FOR SHARE")
        || up.contains("FOR NO KEY UPDATE")
        || up.contains("FOR KEY SHARE")
        || up.contains("LOCK IN SHARE MODE")
}

/// 检测写语句（通过 `query_one/query_all` 路由的 RETURNING 写入）
fn is_write_statement(stmt: &Statement) -> bool {
    let sql = stmt.to_string();
    if sql.is_empty() {
        return false;
    }
    let trimmed = sql.trim_start();
    let up = trimmed.to_ascii_uppercase();

    if up.starts_with("INSERT ")
        || up.starts_with("UPDATE ")
        || up.starts_with("DELETE ")
        || up.starts_with("REPLACE ")
    {
        return true;
    }

    if up.starts_with("WITH ") {
        return cte_main_stmt_is_write(&up);
    }

    false
}

/// 检测 CTE 中主语句是否为写操作
fn cte_main_stmt_is_write(uppercase_sql: &str) -> bool {
    let rest = uppercase_sql.strip_prefix("WITH ").unwrap();
    let rest = rest.strip_prefix("RECURSIVE ").unwrap_or(rest);

    let bytes = rest.as_bytes();
    let mut depth: u32 = 0;
    let mut i = 0;

    while i < bytes.len() {
        let remaining = &bytes[i..];
        match bytes[i] {
            b'(' => depth += 1,
            b')' if depth > 0 => depth -= 1,
            b'I' if depth == 0 && remaining.starts_with(b"INSERT ") => return true,
            b'U' if depth == 0 && remaining.starts_with(b"UPDATE ") => return true,
            b'D' if depth == 0 && remaining.starts_with(b"DELETE ") => return true,
            b'R' if depth == 0 && remaining.starts_with(b"REPLACE ") => return true,
            _ => {}
        }
        i += 1;
    }

    false
}

/// 判断是否为连接级错误（用于重试/熔断）
fn is_connection_error(e: &DbErr) -> bool {
    if matches!(e, DbErr::Conn(_)) {
        return true;
    }

    if matches!(e, DbErr::ConnectionAcquire(_)) {
        return true;
    }

    if matches!(e, DbErr::Query(_)) {
        let s = e.to_string().to_ascii_lowercase();
        let hints = [
            "broken pipe",
            "connection reset",
            "io error",
            "network",
            "eof",
            "transport",
        ];
        return hints.iter().any(|h| s.contains(h));
    }

    false
}

// ============================================================================
// 连接助手
// ============================================================================

/// 连接数据库（写库）
async fn connect_db(url: &str, config: &DatabaseConfig) -> Result<DatabaseConnection, DbErr> {
    let mut opt = ConnectOptions::new(url);
    opt.max_connections(config.max_connections)
        .min_connections(config.min_connections)
        .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
        .acquire_timeout(Duration::from_secs(config.connect_timeout_secs))
        .idle_timeout(Duration::from_secs(config.idle_timeout_secs))
        .max_lifetime(Duration::from_secs(config.max_lifetime_secs))
        .test_before_acquire(true);
    Database::connect(opt).await
}

/// 连接读库
async fn connect_db_read(
    url: &str,
    config: &DatabaseReadConfig,
) -> Result<DatabaseConnection, DbErr> {
    let mut opt = ConnectOptions::new(url);
    opt.max_connections(config.max_connections)
        .min_connections(config.min_connections)
        .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
        .acquire_timeout(Duration::from_secs(config.acquire_timeout_secs))
        .idle_timeout(Duration::from_secs(config.idle_timeout_secs))
        .max_lifetime(Duration::from_secs(config.max_lifetime_secs))
        .test_before_acquire(true);
    Database::connect(opt).await
}

// ============================================================================
// 内部配置类型（避免循环依赖 keycompute-config）
// ============================================================================

/// 写库连接池配置（匹配 keycompute-config 的 DatabaseConfig 字段）
#[doc(hidden)]
#[derive(Clone)]
pub struct DatabaseConfig {
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout_secs: u64,
    pub idle_timeout_secs: u64,
    pub max_lifetime_secs: u64,
}

impl From<&keycompute_config::DatabaseConfig> for DatabaseConfig {
    fn from(c: &keycompute_config::DatabaseConfig) -> Self {
        Self {
            max_connections: c.max_connections,
            min_connections: c.min_connections,
            connect_timeout_secs: c.connect_timeout_secs,
            idle_timeout_secs: c.idle_timeout_secs,
            max_lifetime_secs: c.max_lifetime_secs,
        }
    }
}

/// 读库连接池配置（匹配 keycompute-config 的 DatabaseReadConfig 字段）
#[doc(hidden)]
#[derive(Clone)]
pub struct DatabaseReadConfig {
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout_secs: u64,
    pub acquire_timeout_secs: u64,
    pub idle_timeout_secs: u64,
    pub max_lifetime_secs: u64,
}

impl From<&keycompute_config::DatabaseReadConfig> for DatabaseReadConfig {
    fn from(c: &keycompute_config::DatabaseReadConfig) -> Self {
        Self {
            max_connections: c.max_connections,
            min_connections: c.min_connections,
            connect_timeout_secs: c.connect_timeout_secs,
            acquire_timeout_secs: c.acquire_timeout_secs,
            idle_timeout_secs: c.idle_timeout_secs,
            max_lifetime_secs: c.max_lifetime_secs,
        }
    }
}

/// 路由配置（匹配 keycompute-config 的 DatabaseRoutingConfig 字段）
#[doc(hidden)]
#[derive(Clone)]
pub struct DatabaseRoutingConfig {
    pub strategy: String,
    pub read_weights: Vec<u32>,
    pub retry_attempts: usize,
    pub circuit_break_ms: u64,
    pub fallback_to_write: bool,
    pub health_check_interval_secs: u64,
}

impl From<&keycompute_config::DatabaseRoutingConfig> for DatabaseRoutingConfig {
    fn from(c: &keycompute_config::DatabaseRoutingConfig) -> Self {
        Self {
            strategy: c.strategy.clone(),
            read_weights: c.read_weights.clone(),
            retry_attempts: c.retry_attempts,
            circuit_break_ms: c.circuit_break_ms,
            fallback_to_write: c.fallback_to_write,
            health_check_interval_secs: c.health_check_interval_secs,
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- is_locking_select ----

    #[test]
    fn test_is_locking_select_for_update() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT * FROM users WHERE id = $1 FOR UPDATE".to_string(),
        );
        assert!(is_locking_select(&stmt));
    }

    #[test]
    fn test_is_locking_select_for_share() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT * FROM users WHERE id = $1 FOR SHARE".to_string(),
        );
        assert!(is_locking_select(&stmt));
    }

    #[test]
    fn test_is_locking_select_plain_select() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT * FROM users WHERE id = $1".to_string(),
        );
        assert!(!is_locking_select(&stmt));
    }

    // ---- is_write_statement ----

    #[test]
    fn test_is_write_statement_insert() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "INSERT INTO users (name, email) VALUES ($1, $2) RETURNING id".to_string(),
        );
        assert!(is_write_statement(&stmt));
    }

    #[test]
    fn test_is_write_statement_update() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "UPDATE users SET name = $1 WHERE id = $2 RETURNING id".to_string(),
        );
        assert!(is_write_statement(&stmt));
    }

    #[test]
    fn test_is_write_statement_select_is_not_write() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT * FROM users WHERE id = $1".to_string(),
        );
        assert!(!is_write_statement(&stmt));
    }

    #[test]
    fn test_is_write_statement_with_cte_insert() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "WITH deleted AS (DELETE FROM logs WHERE created_at < $1 RETURNING *) INSERT INTO audit SELECT * FROM deleted".to_string(),
        );
        assert!(is_write_statement(&stmt));
    }

    #[test]
    fn test_is_write_statement_with_cte_select_not_write() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "WITH recent AS (SELECT * FROM users ORDER BY id DESC LIMIT 10) SELECT * FROM recent"
                .to_string(),
        );
        assert!(!is_write_statement(&stmt));
    }

    // ---- is_connection_error ----

    #[test]
    fn test_is_connection_error_conn_variant() {
        let err = DbErr::Conn(sea_orm::RuntimeErr::Internal("broken pipe".to_string()));
        assert!(is_connection_error(&err));
    }

    #[test]
    fn test_is_connection_error_acquire_timeout() {
        let err = DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::Timeout);
        assert!(is_connection_error(&err));
    }

    #[test]
    fn test_is_connection_error_query_not_connection() {
        let err = DbErr::Query(sea_orm::RuntimeErr::Internal(
            "syntax error at or near \"SELECT\"".to_string(),
        ));
        assert!(!is_connection_error(&err));
    }

    // ---- select_weighted_index ----

    #[test]
    fn test_select_weighted_index_single() {
        let counter = AtomicUsize::new(0);
        assert_eq!(select_weighted_index(&[0], &[5], &counter), 0);
    }

    #[test]
    fn test_select_weighted_index_zero_weights_fallback_round_robin() {
        let counter = AtomicUsize::new(0);
        let healthy = [0usize, 1, 2];
        let weights = [0u32, 0, 0];
        assert_eq!(select_weighted_index(&healthy, &weights, &counter), 0);
        assert_eq!(select_weighted_index(&healthy, &weights, &counter), 1);
    }

    // ---- is_locking_select edge cases ----

    #[test]
    fn test_is_locking_select_skip_locked() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT * FROM users WHERE id = $1 FOR UPDATE SKIP LOCKED".to_string(),
        );
        assert!(is_locking_select(&stmt));
    }

    #[test]
    fn test_is_locking_select_nowait() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT * FROM users WHERE id = $1 FOR UPDATE NOWAIT".to_string(),
        );
        assert!(is_locking_select(&stmt));
    }

    #[test]
    fn test_is_locking_select_no_key_update() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT * FROM users WHERE id = $1 FOR NO KEY UPDATE".to_string(),
        );
        assert!(is_locking_select(&stmt));
    }

    #[test]
    fn test_is_locking_select_key_share() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT * FROM users WHERE id = $1 FOR KEY SHARE".to_string(),
        );
        assert!(is_locking_select(&stmt));
    }

    #[test]
    fn test_is_locking_select_lock_in_share_mode() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT * FROM users WHERE id = $1 LOCK IN SHARE MODE".to_string(),
        );
        assert!(is_locking_select(&stmt));
    }

    #[test]
    fn test_is_locking_select_lowercase() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "select * from users where id = $1 for update".to_string(),
        );
        assert!(is_locking_select(&stmt));
    }

    // ---- is_write_statement edge cases ----

    #[test]
    fn test_is_write_statement_delete_returning() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "DELETE FROM logs WHERE created_at < $1 RETURNING id".to_string(),
        );
        assert!(is_write_statement(&stmt));
    }

    #[test]
    fn test_is_write_statement_insert_select() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "INSERT INTO audit (user_id, action) SELECT id, $2 FROM users WHERE email = $1"
                .to_string(),
        );
        assert!(is_write_statement(&stmt));
    }

    #[test]
    fn test_is_write_statement_with_recursive_insert() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "WITH RECURSIVE org_tree AS (SELECT id, parent_id FROM orgs WHERE id = $1 UNION ALL SELECT o.id, o.parent_id FROM orgs o INNER JOIN org_tree ot ON o.parent_id = ot.id) INSERT INTO audit_orgs SELECT * FROM org_tree"
                .to_string(),
        );
        assert!(is_write_statement(&stmt));
    }

    #[test]
    fn test_is_write_statement_with_cte_delete_then_insert() {
        // 真实场景：CTE 做 DELETE RETURNING，主语句 INSERT
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "WITH moved AS (DELETE FROM table_a WHERE id = $1 RETURNING *) INSERT INTO table_b SELECT * FROM moved RETURNING id"
                .to_string(),
        );
        assert!(is_write_statement(&stmt));
    }

    #[test]
    fn test_is_write_statement_cte_in_subquery_not_main() {
        // CTE 内有 UPDATE 但主语句是 SELECT — 当前实现不检测 CTE 内部写操作
        // 这是已知限制：此类模式在 SeaORM 中不会通过 query_one/query_all 路由写操作
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "WITH updated AS (UPDATE accounts SET balance = balance + $1 WHERE id = $2 RETURNING *) SELECT * FROM updated"
                .to_string(),
        );
        assert!(!is_write_statement(&stmt));
    }

    #[test]
    fn test_is_write_statement_replace() {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "REPLACE INTO users (id, name) VALUES ($1, $2)".to_string(),
        );
        assert!(is_write_statement(&stmt));
    }

    // ---- is_connection_error edge cases ----

    #[test]
    fn test_is_connection_error_query_broken_pipe() {
        let err = DbErr::Query(sea_orm::RuntimeErr::Internal("broken pipe".to_string()));
        assert!(is_connection_error(&err));
    }

    #[test]
    fn test_is_connection_error_query_connection_reset() {
        let err = DbErr::Query(sea_orm::RuntimeErr::Internal(
            "connection reset by peer".to_string(),
        ));
        assert!(is_connection_error(&err));
    }

    #[test]
    fn test_is_connection_error_query_eof() {
        let err = DbErr::Query(sea_orm::RuntimeErr::Internal("unexpected EOF".to_string()));
        assert!(is_connection_error(&err));
    }

    #[test]
    fn test_is_connection_error_query_io_error() {
        let err = DbErr::Query(sea_orm::RuntimeErr::Internal(
            "IO error: connection refused".to_string(),
        ));
        assert!(is_connection_error(&err));
    }
}
