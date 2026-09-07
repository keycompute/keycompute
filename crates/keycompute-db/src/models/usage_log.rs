use crate::{DbError, DbRouter};
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 用量日志模型（计费主账本）
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct UsageLog {
    pub id: Uuid,
    pub request_id: Uuid,
    #[serde(default, skip_serializing)]
    pub idempotency_id: Option<Uuid>,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub produce_ai_key_id: Uuid,
    pub model_name: String,
    pub provider_name: String,
    pub account_id: Uuid,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub total_tokens: i32,
    pub input_unit_price_snapshot: BigDecimal,
    pub output_unit_price_snapshot: BigDecimal,
    pub user_amount: BigDecimal,
    pub currency: String,
    pub usage_source: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// 创建用量日志请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateUsageLogRequest {
    pub request_id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub produce_ai_key_id: Uuid,
    pub model_name: String,
    pub provider_name: String,
    pub account_id: Uuid,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub input_unit_price_snapshot: BigDecimal,
    pub output_unit_price_snapshot: BigDecimal,
    pub user_amount: BigDecimal,
    pub currency: String,
    pub usage_source: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

/// 用量统计
#[derive(Debug, Clone, Serialize, FromQueryResult)]
pub struct UsageStats {
    pub total_requests: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_tokens: i64,
    pub total_amount: BigDecimal,
}

/// 用户用量统计（用于用户自服务 API）
#[derive(Debug, Clone, Serialize, FromQueryResult)]
pub struct UserUsageStats {
    pub total_requests: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_tokens: i64,
    pub total_cost: BigDecimal,
}

impl UsageLog {
    /// 创建用量日志
    pub async fn create(
        db: &impl ConnectionTrait,
        req: &CreateUsageLogRequest,
    ) -> Result<UsageLog, DbError> {
        Self::create_with_idempotency(db, req, None).await
    }

    /// Insert one immutable ledger row, or return the authoritative row for a
    /// stable idempotency identity when a retry races or replays it.
    pub async fn create_with_idempotency(
        db: &impl ConnectionTrait,
        req: &CreateUsageLogRequest,
        idempotency_id: Option<Uuid>,
    ) -> Result<UsageLog, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO usage_logs (
                request_id, idempotency_id, tenant_id, user_id, produce_ai_key_id,
                model_name, provider_name, account_id,
                input_tokens, output_tokens, total_tokens,
                input_unit_price_snapshot, output_unit_price_snapshot,
                user_amount, currency, usage_source, status,
                started_at, finished_at
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $9 + $10,
                $11, $12, $13, $14, $15, $16, $17, $18
            )
            ON CONFLICT DO NOTHING
            RETURNING *
            "#,
            [
                req.request_id.into(),
                idempotency_id.into(),
                req.tenant_id.into(),
                req.user_id.into(),
                req.produce_ai_key_id.into(),
                req.model_name.as_str().into(),
                req.provider_name.as_str().into(),
                req.account_id.into(),
                req.input_tokens.into(),
                req.output_tokens.into(),
                req.input_unit_price_snapshot.clone().into(),
                req.output_unit_price_snapshot.clone().into(),
                req.user_amount.clone().into(),
                req.currency.as_str().into(),
                req.usage_source.as_str().into(),
                req.status.as_str().into(),
                req.started_at.into(),
                req.finished_at.into(),
            ],
        );
        let log = match UsageLog::find_by_statement(stmt).one(db).await? {
            Some(log) => log,
            None => match idempotency_id {
                Some(idempotency_id) => {
                    UsageLog::find_by_idempotency_id(db, idempotency_id).await?
                }
                None => UsageLog::find_by_request_id(db, req.request_id).await?,
            }
            .ok_or_else(|| {
                DbError::Other(
                    "usage log conflict did not expose the authoritative row".to_string(),
                )
            })?,
        };

        Ok(log)
    }

    /// 根据 ID 查找用量日志
    pub async fn find_by_id(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<UsageLog>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM usage_logs WHERE id = $1",
            [id.into()],
        );
        let log = UsageLog::find_by_statement(stmt).one(db).await?;

        Ok(log)
    }

    /// 根据请求 ID 查找用量日志
    pub async fn find_by_request_id(
        db: &impl ConnectionTrait,
        request_id: Uuid,
    ) -> Result<Option<UsageLog>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM usage_logs WHERE request_id = $1",
            [request_id.into()],
        );
        let log = UsageLog::find_by_statement(stmt).one(db).await?;

        Ok(log)
    }

    /// Find the ledger row for either an ordinary request ID or the stable
    /// identity assigned to an idempotent Responses request.
    pub async fn find_by_billing_request_id(
        db: &impl ConnectionTrait,
        billing_request_id: Uuid,
    ) -> Result<Option<UsageLog>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM usage_logs WHERE request_id = $1 OR idempotency_id = $1 LIMIT 1",
            [billing_request_id.into()],
        );
        Ok(UsageLog::find_by_statement(stmt).one(db).await?)
    }

    /// Read the authoritative ledger row for settlement recovery.
    ///
    /// A successful but stale replica read must not hide a committed usage log:
    /// callers use the returned row to replay post-ledger effects and TPM usage.
    pub async fn find_by_billing_request_id_on_writer(
        db: &DbRouter,
        billing_request_id: Uuid,
    ) -> Result<Option<UsageLog>, DbError> {
        Self::find_by_billing_request_id(db.write_conn(), billing_request_id).await
    }

    /// Check the writer for a completed logical billing identity.
    ///
    /// This is a correctness boundary for idempotent upstream dispatch: a
    /// lagging read replica must never hide an already committed ledger row
    /// and authorize the same logical request to execute again.
    pub async fn exists_by_billing_request_id_on_writer(
        db: &DbRouter,
        billing_request_id: Uuid,
    ) -> Result<bool, DbError> {
        Self::exists_by_billing_request_id(db.write_conn(), billing_request_id).await
    }

    /// Check one concrete connection or transaction for a completed logical
    /// billing identity. Callers that are also creating an idempotency claim
    /// use this form so the claim and the pre-dispatch ledger check commit (or
    /// roll back) together.
    pub async fn exists_by_billing_request_id(
        db: &impl ConnectionTrait,
        billing_request_id: Uuid,
    ) -> Result<bool, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT EXISTS (
                SELECT 1 FROM usage_logs
                WHERE request_id = $1 OR idempotency_id = $1
            ) AS exists"#,
            [billing_request_id.into()],
        );
        let result = db
            .query_one(stmt)
            .await?
            .ok_or_else(|| DbError::Other("usage ledger existence query returned no row".into()))?;
        result.try_get_by_index(0).map_err(DbError::DatabaseError)
    }

    pub async fn find_by_idempotency_id(
        db: &impl ConnectionTrait,
        idempotency_id: Uuid,
    ) -> Result<Option<UsageLog>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM usage_logs WHERE idempotency_id = $1",
            [idempotency_id.into()],
        );
        Ok(UsageLog::find_by_statement(stmt).one(db).await?)
    }

    /// 查找租户的用量日志
    pub async fn find_by_tenant(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<UsageLog>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM usage_logs WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
            [tenant_id.into(), limit.into(), offset.into()],
        );
        let logs = UsageLog::find_by_statement(stmt).all(db).await?;

        Ok(logs)
    }

    /// 获取租户的用量日志总数
    pub async fn count_by_tenant(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
    ) -> Result<i64, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*) FROM usage_logs WHERE tenant_id = $1",
            [tenant_id.into()],
        );
        let result = db
            .query_one(stmt)
            .await?
            .ok_or_else(|| DbError::Other("count query failed".to_string()))?;
        let count: i64 = result.try_get_by_index(0).map_err(DbError::DatabaseError)?;
        Ok(count)
    }

    /// 查找用户的用量日志
    pub async fn find_by_user(
        db: &impl ConnectionTrait,
        user_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<UsageLog>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM usage_logs WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
            [user_id.into(), limit.into(), offset.into()],
        );
        let logs = UsageLog::find_by_statement(stmt).all(db).await?;

        Ok(logs)
    }

    /// 获取租户用量统计
    pub async fn get_stats_by_tenant(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<UsageStats, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT
                COUNT(*) as total_requests,
                COALESCE(SUM(input_tokens), 0) as total_input_tokens,
                COALESCE(SUM(output_tokens), 0) as total_output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COALESCE(SUM(user_amount), 0) as total_amount
            FROM usage_logs
            WHERE tenant_id = $1
              AND created_at >= $2
              AND created_at < $3
            "#,
            [tenant_id.into(), from.into(), to.into()],
        );
        let stats = UsageStats::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("stats query failed".to_string()))?;

        Ok(stats)
    }

    /// 获取用户用量统计
    pub async fn get_stats_by_user(
        db: &impl ConnectionTrait,
        user_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<UsageStats, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT
                COUNT(*) as total_requests,
                COALESCE(SUM(input_tokens), 0) as total_input_tokens,
                COALESCE(SUM(output_tokens), 0) as total_output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COALESCE(SUM(user_amount), 0) as total_amount
            FROM usage_logs
            WHERE user_id = $1
              AND created_at >= $2
              AND created_at < $3
            "#,
            [user_id.into(), from.into(), to.into()],
        );
        let stats = UsageStats::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("stats query failed".to_string()))?;

        Ok(stats)
    }

    /// 获取用户全部用量统计（不限时间范围）
    pub async fn get_user_stats(
        db: &impl ConnectionTrait,
        user_id: Uuid,
    ) -> Result<UserUsageStats, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT
                COUNT(*) as total_requests,
                COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens,
                COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens,
                COALESCE(SUM(total_tokens), 0)::bigint as total_tokens,
                COALESCE(SUM(user_amount), 0) as total_cost
            FROM usage_logs
            WHERE user_id = $1
            "#,
            [user_id.into()],
        );
        let stats = UserUsageStats::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("stats query failed".to_string()))?;

        Ok(stats)
    }

    /// 获取租户按模型分组的统计
    pub async fn get_stats_by_tenant_grouped_by_model(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<ModelStatsRow>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT
                model_name,
                COUNT(*) as request_count,
                COALESCE(SUM(input_tokens), 0)::bigint as input_tokens,
                COALESCE(SUM(output_tokens), 0)::bigint as output_tokens,
                COALESCE(SUM(user_amount), 0) as amount
            FROM usage_logs
            WHERE tenant_id = $1
              AND created_at >= $2
              AND created_at < $3
            GROUP BY model_name
            ORDER BY request_count DESC
            "#,
            [tenant_id.into(), from.into(), to.into()],
        );
        let stats = ModelStatsRow::find_by_statement(stmt).all(db).await?;

        Ok(stats)
    }
}

/// 按模型分组的统计
#[derive(Debug, Clone, Serialize, FromQueryResult)]
pub struct ModelStatsRow {
    pub model_name: String,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub amount: BigDecimal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sea_orm::{
        Database, DatabaseConnection, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Value,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct ExistsProxy {
        exists: bool,
        query_count: Arc<AtomicUsize>,
    }

    #[derive(Debug)]
    struct EmptyProxy {
        query_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ProxyDatabaseTrait for ExistsProxy {
        async fn query(&self, _statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
            self.query_count.fetch_add(1, Ordering::Relaxed);
            Ok(vec![ProxyRow::new(BTreeMap::from([(
                "exists".to_string(),
                Value::Bool(Some(self.exists)),
            )]))])
        }

        async fn execute(&self, _statement: Statement) -> Result<ProxyExecResult, DbErr> {
            Ok(ProxyExecResult::default())
        }
    }

    #[async_trait]
    impl ProxyDatabaseTrait for EmptyProxy {
        async fn query(&self, _statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
            self.query_count.fetch_add(1, Ordering::Relaxed);
            Ok(Vec::new())
        }

        async fn execute(&self, _statement: Statement) -> Result<ProxyExecResult, DbErr> {
            Ok(ProxyExecResult::default())
        }
    }

    #[tokio::test]
    async fn completed_ledger_existence_check_bypasses_stale_read_replica() {
        async fn exists_connection(exists: bool) -> (DatabaseConnection, Arc<AtomicUsize>) {
            let query_count = Arc::new(AtomicUsize::new(0));
            let connection = Database::connect_proxy(
                DbBackend::Postgres,
                Arc::new(Box::new(ExistsProxy {
                    exists,
                    query_count: Arc::clone(&query_count),
                })),
            )
            .await
            .unwrap();
            (connection, query_count)
        }

        let (writer, writer_queries) = exists_connection(true).await;
        let (stale_reader, reader_queries) = exists_connection(false).await;
        let router = DbRouter::with_read_connections_for_test(writer, vec![stale_reader]);

        assert!(
            UsageLog::exists_by_billing_request_id_on_writer(&router, Uuid::new_v4())
                .await
                .unwrap()
        );
        assert_eq!(writer_queries.load(Ordering::Relaxed), 1);
        assert_eq!(reader_queries.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn completed_ledger_lookup_bypasses_read_replica() {
        async fn empty_connection() -> (DatabaseConnection, Arc<AtomicUsize>) {
            let query_count = Arc::new(AtomicUsize::new(0));
            let connection = Database::connect_proxy(
                DbBackend::Postgres,
                Arc::new(Box::new(EmptyProxy {
                    query_count: Arc::clone(&query_count),
                })),
            )
            .await
            .unwrap();
            (connection, query_count)
        }

        let (writer, writer_queries) = empty_connection().await;
        let (stale_reader, reader_queries) = empty_connection().await;
        let router = DbRouter::with_read_connections_for_test(writer, vec![stale_reader]);

        assert!(
            UsageLog::find_by_billing_request_id_on_writer(&router, Uuid::new_v4())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(writer_queries.load(Ordering::Relaxed), 1);
        assert_eq!(reader_queries.load(Ordering::Relaxed), 0);
    }
}
