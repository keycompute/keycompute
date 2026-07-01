//! KeyCompute 数据库访问层
//!
//! 提供 PostgreSQL 数据库连接池、ORM 模型和迁移支持

pub mod models;
pub mod schema;

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database as SeaDatabase, DatabaseConnection,
    DatabaseTransaction, DbBackend, Statement, TransactionTrait,
};
use std::time::Duration;

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

/// 运行数据库迁移
///
/// 使用纯 SQL 执行嵌入式迁移文件
pub async fn run_migrations(db: &DatabaseConnection) -> Result<(), DbError> {
    let sql = include_str!("migrations/001_init.sql");
    let backend = db.get_database_backend();

    for statement in split_sql_statements(sql) {
        let trimmed = statement.trim();
        if trimmed.is_empty() {
            continue;
        }
        let stmt = Statement::from_string(backend, statement);
        db.execute(stmt)
            .await
            .map_err(|e| DbError::MigrationError(e.to_string()))?;
    }

    tracing::info!("Database migrations completed successfully");

    Ok(())
}

// ============================================================================
// 数据库管理器
// ============================================================================

/// 数据库管理器
///
/// 封装数据库连接池，提供统一的数据库访问入口
#[derive(Clone)]
pub struct Database {
    db: DatabaseConnection,
}

impl Database {
    /// 创建新的数据库实例
    pub async fn new(config: &DatabaseConfig) -> Result<Self, DbError> {
        let db = init_pool(config).await?;
        Ok(Self { db })
    }

    /// 从现有连接创建
    pub fn from_connection(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// 从环境变量创建
    pub async fn from_env() -> Result<Self, DbError> {
        let config = DatabaseConfig::default();
        Self::new(&config).await
    }

    /// 获取连接引用
    pub fn connection(&self) -> &DatabaseConnection {
        &self.db
    }

    /// 获取连接（消费）
    pub fn into_connection(self) -> DatabaseConnection {
        self.db
    }

    /// 开始一个事务
    pub async fn begin(&self) -> Result<DatabaseTransaction, sea_orm::DbErr> {
        self.db.begin().await
    }

    /// 运行迁移
    pub async fn migrate(&self) -> Result<(), DbError> {
        run_migrations(&self.db).await
    }

    /// 测试连接
    pub async fn test_connection(&self) -> Result<(), DbError> {
        let stmt = Statement::from_string(DbBackend::Postgres, "SELECT 1".to_string());
        self.db.execute(stmt).await?;
        Ok(())
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
