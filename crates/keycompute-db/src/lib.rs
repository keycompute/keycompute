//! KeyCompute 数据库访问层
//!
//! 提供 PostgreSQL 数据库连接池、ORM 模型和迁移支持

pub mod db_router;
pub mod models;
pub mod schema;

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database as SeaDatabase, DatabaseConnection,
    DatabaseTransaction, DbBackend, Statement, TransactionTrait,
};
use std::sync::Arc;
use std::time::Duration;

pub use db_router::DbRouter;
pub use models::*;
pub use schema::*;

// ============================================================================
// 错误类型定义
// ============================================================================

/// 数据库错误类型
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// 连接错误
    #[error("database connection failed: {0}")]
    ConnectionError(String),

    /// 迁移错误
    #[error("migration failed: {0}")]
    MigrationError(String),

    /// 实体未找到
    #[error("{entity} not found: {id}")]
    NotFound { entity: String, id: String },

    /// 余额不足
    #[error("insufficient balance: required {required}, available {available}")]
    InsufficientBalance { required: String, available: String },

    /// 唯一约束冲突
    #[error("duplicate key: {entity} with {field}={value} already exists")]
    DuplicateKey {
        entity: String,
        field: String,
        value: String,
    },

    /// 订单状态无效
    #[error("invalid order status: expected {expected}, actual {actual}")]
    InvalidOrderStatus { expected: String, actual: String },

    /// 数据库原生错误
    #[error("database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),

    /// 其他错误
    #[error("{0}")]
    Other(String),
}

impl DbError {
    /// 创建 NotFound 错误
    pub fn not_found(entity: impl Into<String>, id: impl Into<String>) -> Self {
        Self::NotFound {
            entity: entity.into(),
            id: id.into(),
        }
    }

    /// 创建 InsufficientBalance 错误
    pub fn insufficient_balance(required: impl Into<String>, available: impl Into<String>) -> Self {
        Self::InsufficientBalance {
            required: required.into(),
            available: available.into(),
        }
    }

    /// 创建 DuplicateKey 错误
    pub fn duplicate_key(
        entity: impl Into<String>,
        field: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Self::DuplicateKey {
            entity: entity.into(),
            field: field.into(),
            value: value.into(),
        }
    }

    /// 检查是否为未找到错误
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }

    /// 检查是否为余额不足错误
    pub fn is_insufficient_balance(&self) -> bool {
        matches!(self, Self::InsufficientBalance { .. })
    }

    /// 检查是否为唯一约束冲突
    pub fn is_duplicate(&self) -> bool {
        matches!(self, Self::DuplicateKey { .. })
            || matches!(self, Self::DatabaseError(sea_orm::DbErr::Query(e))
                if e.to_string().contains("duplicate key") || e.to_string().contains("unique constraint"))
    }

    /// 从 sea_orm::DbErr 转换，保留语义
    pub fn from_db_err(err: sea_orm::DbErr, entity: &str, id: &str) -> Self {
        match &err {
            sea_orm::DbErr::RecordNotFound(_) => Self::NotFound {
                entity: entity.to_string(),
                id: id.to_string(),
            },
            sea_orm::DbErr::Query(e)
                if e.to_string().contains("duplicate key")
                    || e.to_string().contains("unique constraint") =>
            {
                Self::DuplicateKey {
                    entity: entity.to_string(),
                    field: "constraint".to_string(),
                    value: id.to_string(),
                }
            }
            _ => Self::DatabaseError(err),
        }
    }
}

// ============================================================================
// 数据库配置
// ============================================================================

/// 数据库配置
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// 数据库连接 URL
    pub url: String,
    /// 最大连接数
    pub max_connections: u32,
    /// 最小连接数
    pub min_connections: u32,
    /// 连接超时时间（秒）
    pub connect_timeout: u64,
    /// 连接空闲超时时间（秒）
    pub idle_timeout: u64,
    /// 连接最大生命周期（秒）
    pub max_lifetime: u64,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://localhost/keycompute".to_string()),
            max_connections: 10,
            min_connections: 2,
            connect_timeout: 30,
            idle_timeout: 600,
            max_lifetime: 1800,
        }
    }
}

impl From<&DatabaseConfig> for db_router::DatabaseConfig {
    fn from(c: &DatabaseConfig) -> Self {
        Self {
            max_connections: c.max_connections,
            min_connections: c.min_connections,
            connect_timeout_secs: c.connect_timeout,
            idle_timeout_secs: c.idle_timeout,
            max_lifetime_secs: c.max_lifetime,
        }
    }
}

// ============================================================================
// 连接池管理
// ============================================================================

/// 初始化数据库连接
///
/// # Examples
///
/// ```rust,no_run
/// use keycompute_db::{init_pool, DatabaseConfig};
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let config = DatabaseConfig::default();
///     let db = init_pool(&config).await?;
///     Ok(())
/// }
/// ```
pub async fn init_pool(config: &DatabaseConfig) -> Result<DatabaseConnection, DbError> {
    let mut opt = ConnectOptions::new(&config.url);
    opt.max_connections(config.max_connections)
        .min_connections(config.min_connections)
        .acquire_timeout(Duration::from_secs(config.connect_timeout))
        .idle_timeout(Duration::from_secs(config.idle_timeout))
        .max_lifetime(Duration::from_secs(config.max_lifetime));

    let db = SeaDatabase::connect(opt)
        .await
        .map_err(|e| DbError::ConnectionError(e.to_string()))?;

    tracing::info!("Database pool initialized successfully");

    Ok(db)
}

/// 将 SQL 按顶层分号切分为独立语句
///
/// 正确处理以下场景：
/// - `$$ ... $$` 美元引号块（PL/pgSQL 函数体等）
/// - `'...'` 单引号字符串常量
fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_dollar_block = false;
    let mut prev_char: Option<char> = None;

    for ch in sql.chars() {
        current.push(ch);

        match ch {
            '\'' if prev_char != Some('\\') && !in_dollar_block => {
                in_single_quote = !in_single_quote;
            }
            '$' if prev_char == Some('$') && !in_single_quote && !in_dollar_block => {
                // 完整匹配 $$，跳过已推入的 current
                in_dollar_block = true;
            }
            '$' if prev_char == Some('$') && !in_single_quote && in_dollar_block => {
                // 完整匹配关闭 $$
                in_dollar_block = false;
            }
            ';' if !in_single_quote && !in_dollar_block => {
                // 移除已推入的 ';'，它只是分隔符不属于语句内容
                current.pop();
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    statements.push(trimmed.to_string());
                }
                current.clear();
            }
            _ => {}
        }

        prev_char = Some(ch);
    }

    // 处理最后一个语句
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_string());
    }

    statements
}

/// Check if a database error is an expected "already exists" error for
/// idempotent migration statements (CREATE TABLE IF NOT EXISTS, CREATE INDEX IF NOT EXISTS).
///
/// This function ONLY matches DDL-level "already exists" errors:
/// - PostgreSQL 42P07 (duplicate_table): table already exists
/// - PostgreSQL 42710 (duplicate_object): index/function/type already exists
///
/// It does NOT match data-level errors like UniqueConstraintViolation (23505)
/// because those indicate real data conflicts, not idempotent DDL re-execution.
fn is_duplicate_table_or_index_error(err: &sea_orm::DbErr) -> bool {
    // SqlErr only covers data-level constraint violations (23505, 23503, etc.),
    // not DDL "already exists" errors (42P07, 42710). So we skip sql_err() matching
    // entirely and rely on string-based detection for DDL-specific error codes.

    let msg = err.to_string().to_lowercase();

    // Match PostgreSQL error codes for DDL "already exists" scenarios:
    // - 42P07: duplicate_table (CREATE TABLE IF NOT EXISTS on existing table without IF NOT EXISTS)
    // - 42710: duplicate_object (CREATE INDEX IF NOT EXISTS on existing index)
    if msg.contains("42p07") || msg.contains("42710") {
        return true;
    }

    // Fallback: match the generic "already exists" message, but only for DDL objects
    if msg.contains("already exists") {
        return msg.contains("relation")
            || msg.contains("type")
            || msg.contains("index")
            || msg.contains("table");
    }

    false
}

/// 运行数据库迁移
///
/// 使用纯 SQL 执行嵌入式迁移文件。
/// 每个语句在独立的 savepoint 中执行，避免单条失败导致整个迁移中断。
/// 通过 idempotent 检查（`IF NOT EXISTS` + 错误容忍）支持重复执行。
///
/// # 并发安全
///
/// 使用 PostgreSQL session-level advisory lock 防止多个连接并行执行迁移。
/// 这在并行集成测试场景下是必要的——多个测试线程同时创建连接池并运行迁移时，
/// `INSERT INTO system_settings` 等语句会产生 `tuple concurrently updated` 冲突。
pub async fn run_migrations(db: &(impl ConnectionTrait + TransactionTrait)) -> Result<(), DbError> {
    // 获取 session-level advisory lock，防止并发迁移
    // 使用双 key 版本：pg_advisory_lock(key1 int, key2 int)
    // key1 = hashtext('keycompute'), key2 = hashtext('db_migration')
    // 锁在连接关闭时自动释放
    db.execute_unprepared(
        "SELECT pg_advisory_lock(hashtext('keycompute'), hashtext('db_migration'))",
    )
    .await
    .map_err(|e| {
        DbError::MigrationError(format!(
            "Failed to acquire migration advisory lock (concurrent migration prevention): {}",
            e
        ))
    })?;

    let result = run_migrations_internal(db).await;

    // 释放 advisory lock（最佳努力，连接关闭时也会自动释放）
    db.execute_unprepared(
        "SELECT pg_advisory_unlock(hashtext('keycompute'), hashtext('db_migration'))",
    )
    .await
    .ok();

    result
}

/// 迁移内部实现（被 `run_migrations` 的 advisory lock 保护）
async fn run_migrations_internal(
    db: &(impl ConnectionTrait + TransactionTrait),
) -> Result<(), DbError> {
    let sql = include_str!("migrations/001_init.sql");

    for statement in split_sql_statements(sql) {
        let trimmed = statement.trim();
        if trimmed.is_empty() {
            continue;
        }

        // 使用 savepoint 隔离每个语句，失败时不影响其他语句
        let sp = db
            .begin()
            .await
            .map_err(|e| DbError::MigrationError(e.to_string()))?;

        match sp
            .execute(Statement::from_string(
                sp.get_database_backend(),
                trimmed.to_string(),
            ))
            .await
        {
            Ok(_) => {
                sp.commit()
                    .await
                    .map_err(|e| DbError::MigrationError(e.to_string()))?;
            }
            Err(e) => {
                sp.rollback()
                    .await
                    .map_err(|e| DbError::MigrationError(e.to_string()))?;

                if is_duplicate_table_or_index_error(&e) {
                    tracing::warn!("Migration statement skipped (already exists): {}", trimmed);
                } else {
                    return Err(DbError::MigrationError(e.to_string()));
                }
            }
        }
    }

    tracing::info!("Database migrations completed successfully");

    Ok(())
}

// ============================================================================
// 数据库管理器
// ============================================================================

/// 数据库管理器
///
/// 封装数据库路由器，提供统一的数据库访问入口
#[derive(Clone)]
pub struct Database {
    router: Arc<DbRouter>,
}

impl Database {
    /// 创建新的数据库实例
    ///
    /// # 参数
    /// * `write_config` — 写库连接池配置
    /// * `read_urls` — 读库连接 URL 列表（空列表 = 无读写分离）
    /// * `read_config` — 读库连接池配置
    /// * `routing_config` — 读写分离路由配置
    pub async fn new(
        write_config: &DatabaseConfig,
        read_urls: &[String],
        read_config: &keycompute_config::DatabaseReadConfig,
        routing_config: &keycompute_config::DatabaseRoutingConfig,
    ) -> Result<Self, DbError> {
        use db_router::{
            DatabaseConfig as RouterDbConfig, DatabaseReadConfig as RouterReadConfig,
            DatabaseRoutingConfig as RouterRoutingConfig,
        };
        let router = DbRouter::new(
            &write_config.url,
            read_urls,
            &RouterDbConfig::from(write_config),
            &RouterReadConfig::from(read_config),
            &RouterRoutingConfig::from(routing_config),
        )
        .await
        .map_err(|e| DbError::ConnectionError(e.to_string()))?;
        Ok(Self { router })
    }

    /// 从 DbRouter 创建
    pub fn from_router(router: Arc<DbRouter>) -> Self {
        Self { router }
    }

    /// 从现有连接创建（包装为单库模式）
    pub fn from_connection(db: DatabaseConnection) -> Self {
        Self {
            router: DbRouter::single(db),
        }
    }

    /// 获取连接引用（返回写库连接）
    pub fn connection(&self) -> &DatabaseConnection {
        self.router.write_conn()
    }

    /// 获取路由引用
    pub fn router(&self) -> Arc<DbRouter> {
        Arc::clone(&self.router)
    }

    /// 开始一个事务
    pub async fn begin(&self) -> Result<DatabaseTransaction, sea_orm::DbErr> {
        self.router.write_conn().begin().await
    }

    /// 运行迁移
    pub async fn migrate(&self) -> Result<(), DbError> {
        run_migrations(self.router.write_conn()).await
    }

    /// 测试连接
    pub async fn test_connection(&self) -> Result<(), DbError> {
        let stmt = Statement::from_string(DbBackend::Postgres, "SELECT 1".to_string());
        self.router.write_conn().execute(stmt).await?;
        Ok(())
    }

    /// 获取写库连接（消费自身）
    #[deprecated(since = "0.3.0", note = "Use `router()` or `connection()` instead")]
    pub fn into_connection(self) -> DatabaseConnection {
        self.router.write_conn().clone()
    }

    /// 获取路由器（消费自身）
    pub fn into_router(self) -> Arc<DbRouter> {
        self.router
    }
}

/// 数据库连接管理器（已弃用，使用 Database）
#[deprecated(since = "0.2.0", note = "Use `Database` instead")]
pub type DatabaseManager = Database;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_database_config_default() {
        let config = DatabaseConfig::default();
        assert_eq!(config.max_connections, 10);
        assert_eq!(config.min_connections, 2);
    }

    #[test]
    fn test_db_error_helpers() {
        let err = DbError::not_found("User", "123");
        assert!(err.is_not_found());
        assert!(err.to_string().contains("User not found"));

        let err = DbError::insufficient_balance("100", "50");
        assert!(err.is_insufficient_balance());
        assert!(err.to_string().contains("insufficient balance"));

        let err = DbError::duplicate_key("User", "email", "test@example.com");
        assert!(err.is_duplicate());
    }

    #[test]
    fn test_db_error_from_db_err() {
        let err = DbError::from_db_err(
            sea_orm::DbErr::RecordNotFound("not found".to_string()),
            "User",
            "123",
        );
        assert!(err.is_not_found());
    }

    #[test]
    fn test_split_sql_statements_simple() {
        let sql = "SELECT 1; SELECT 2;";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].trim_start().starts_with("SELECT 1"));
        assert!(stmts[1].trim_start().starts_with("SELECT 2"));
    }

    #[test]
    fn test_split_sql_statements_no_semicolon() {
        let sql = "SELECT 1";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].trim_start().starts_with("SELECT 1"));
    }

    #[test]
    fn test_split_sql_statements_preserves_dollar_block() {
        let sql = "CREATE FUNCTION foo() RETURNS TRIGGER AS $$\nBEGIN\n    IF OLD.role = 'system' THEN\n        RAISE EXCEPTION 'cannot delete'\n    END IF;\n    RETURN OLD;\nEND;\n$$ LANGUAGE plpgsql;\n\nSELECT 1;";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 2, "should produce exactly 2 statements");
        assert!(
            stmts[0].contains("$$"),
            "first statement should contain $$ block"
        );
        assert!(
            stmts[0].contains("LANGUAGE plpgsql"),
            "first should be function"
        );
        assert!(
            stmts[1].trim_start().starts_with("SELECT 1"),
            "second should be SELECT"
        );
    }

    #[test]
    fn test_split_sql_statements_single_quotes() {
        let sql = r#"INSERT INTO t VALUES ('foo;bar');SELECT 1;"#;
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert!(
            stmts[0].contains("'foo;bar'"),
            "semicolon inside quotes should be preserved"
        );
        assert!(stmts[1].trim_start().starts_with("SELECT 1"));
    }

    #[test]
    fn test_split_sql_statements_empty_result() {
        let stmts = split_sql_statements("");
        assert!(stmts.is_empty());
    }

    #[test]
    fn test_split_sql_statements_whitespace_only() {
        let stmts = split_sql_statements("   ;   ;   ");
        assert!(stmts.is_empty());
    }

    #[test]
    fn test_split_sql_statements_mixed_newlines() {
        let sql = "-- comment\nSELECT 1;\n\n-- another comment\nSELECT 2;";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 2);
    }
}
