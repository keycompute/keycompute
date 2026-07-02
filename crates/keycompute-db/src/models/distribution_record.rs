use crate::DbError;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use sea_orm::{
    TransactionTrait, {ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 分销记录模型
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct DistributionRecord {
    pub id: Uuid,
    pub usage_log_id: Uuid,
    pub tenant_id: Uuid,
    pub beneficiary_id: Uuid,
    pub share_amount: BigDecimal,
    pub share_ratio: BigDecimal,
    /// 分销层级: level1, level2
    pub level: String,
    pub status: String,
    pub settled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// 创建分销记录请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateDistributionRecordRequest {
    pub usage_log_id: Uuid,
    pub tenant_id: Uuid,
    pub beneficiary_id: Uuid,
    pub share_amount: BigDecimal,
    pub share_ratio: BigDecimal,
    /// 分销层级: level1, level2
    pub level: String,
}

/// 分销统计
#[derive(Debug, Clone, Serialize, FromQueryResult)]
pub struct DistributionStats {
    pub total_records: i64,
    pub total_amount: BigDecimal,
    pub settled_amount: BigDecimal,
    pub pending_amount: BigDecimal,
}

/// 分销层级统计
#[derive(Debug, Clone, Serialize, FromQueryResult)]
pub struct DistributionLevelStats {
    /// 一级分销收益
    pub level1_amount: BigDecimal,
    /// 二级分销收益
    pub level2_amount: BigDecimal,
    /// 一级分销记录数
    pub level1_count: i64,
    /// 二级分销记录数
    pub level2_count: i64,
}

impl DistributionRecord {
    /// 创建分销记录
    pub async fn create(
        db: &impl ConnectionTrait,
        req: &CreateDistributionRecordRequest,
    ) -> Result<DistributionRecord, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO distribution_records (
                usage_log_id, tenant_id, beneficiary_id,
                share_amount, share_ratio, level, status
            )
            VALUES ($1, $2, $3, $4, $5, $6, 'pending')
            RETURNING *
            "#,
            [
                req.usage_log_id.into(),
                req.tenant_id.into(),
                req.beneficiary_id.into(),
                req.share_amount.clone().into(),
                req.share_ratio.clone().into(),
                req.level.as_str().into(),
            ],
        );
        let record = DistributionRecord::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(record)
    }

    /// 批量创建分销记录（使用事务）
    ///
    /// 所有记录在同一事务中创建，保证原子性。
    /// 如果记录已存在（基于唯一约束），则跳过插入。
    pub async fn create_many(
        db: &(impl ConnectionTrait + TransactionTrait),
        requests: &[CreateDistributionRecordRequest],
    ) -> Result<Vec<DistributionRecord>, DbError> {
        let txn = db.begin().await?;
        let records = Self::create_many_tx(&txn, requests).await?;
        txn.commit().await?;
        Ok(records)
    }

    /// 批量创建分销记录（在现有事务中执行）
    ///
    /// 用于在调用者已有事务中执行批量插入。
    /// 使用 ON CONFLICT DO NOTHING 实现幂等性，已存在的记录将被跳过。
    pub async fn create_many_tx(
        txn: &DatabaseTransaction,
        requests: &[CreateDistributionRecordRequest],
    ) -> Result<Vec<DistributionRecord>, DbError> {
        let mut records = Vec::with_capacity(requests.len());

        for req in requests {
            // 使用 ON CONFLICT DO NOTHING 实现幂等性
            // 如果记录已存在（基于 uk_distribution_records_unique 约束），则跳过
            let record_result =
                DistributionRecord::find_by_statement(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    r#"
                INSERT INTO distribution_records (
                    usage_log_id, tenant_id, beneficiary_id,
                    share_amount, share_ratio, level, status
                )
                VALUES ($1, $2, $3, $4, $5, $6, 'pending')
                ON CONFLICT (usage_log_id, beneficiary_id, level) DO NOTHING
                RETURNING *
                "#,
                    [
                        req.usage_log_id.into(),
                        req.tenant_id.into(),
                        req.beneficiary_id.into(),
                        req.share_amount.clone().into(),
                        req.share_ratio.clone().into(),
                        req.level.as_str().into(),
                    ],
                ))
                .one(txn)
                .await?;

            // 如果记录已存在，查询现有记录
            if let Some(record) = record_result {
                records.push(record);
            } else {
                tracing::debug!(
                    usage_log_id = %req.usage_log_id,
                    beneficiary_id = %req.beneficiary_id,
                    level = %req.level,
                    "Distribution record already exists, skipping"
                );
                // 查询已存在的记录（使用 fetch_optional 避免并发删除导致错误）
                match DistributionRecord::find_by_statement(
                    Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "SELECT * FROM distribution_records WHERE usage_log_id = $1 AND beneficiary_id = $2 AND level = $3",
                        [req.usage_log_id.into(), req.beneficiary_id.into(), req.level.as_str().into()],
                    ),
                )
                .one(txn)
                .await
                {
                    Ok(Some(existing)) => records.push(existing),
                    Ok(None) => {
                        // 记录可能已被并发删除，记录警告但不中断流程
                        tracing::warn!(
                            usage_log_id = %req.usage_log_id,
                            beneficiary_id = %req.beneficiary_id,
                            level = %req.level,
                            "Distribution record not found after conflict, possibly deleted concurrently"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            usage_log_id = %req.usage_log_id,
                            beneficiary_id = %req.beneficiary_id,
                            level = %req.level,
                            error = %e,
                            "Failed to fetch existing distribution record"
                        );
                        return Err(e.into());
                    }
                }
            }
        }

        Ok(records)
    }

    /// 根据 ID 查找分销记录
    pub async fn find_by_id(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<DistributionRecord>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM distribution_records WHERE id = $1",
            [id.into()],
        );
        let record = DistributionRecord::find_by_statement(stmt).one(db).await?;

        Ok(record)
    }

    /// 查找用量日志的所有分销记录
    pub async fn find_by_usage_log(
        db: &impl ConnectionTrait,
        usage_log_id: Uuid,
    ) -> Result<Vec<DistributionRecord>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM distribution_records WHERE usage_log_id = $1",
            [usage_log_id.into()],
        );
        let records = DistributionRecord::find_by_statement(stmt).all(db).await?;

        Ok(records)
    }

    /// 查找租户的分销记录
    pub async fn find_by_tenant(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<DistributionRecord>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM distribution_records WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
            [tenant_id.into(), limit.into(), offset.into()],
        );
        let records = DistributionRecord::find_by_statement(stmt).all(db).await?;

        Ok(records)
    }

    /// 查找受益人的分销记录
    pub async fn find_by_beneficiary(
        db: &impl ConnectionTrait,
        beneficiary_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<DistributionRecord>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM distribution_records WHERE beneficiary_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
            [beneficiary_id.into(), limit.into(), offset.into()],
        );
        let records = DistributionRecord::find_by_statement(stmt).all(db).await?;

        Ok(records)
    }

    /// 结算分销记录
    pub async fn settle(&self, db: &impl ConnectionTrait) -> Result<DistributionRecord, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE distribution_records SET status = 'settled', settled_at = NOW() WHERE id = $1 RETURNING *",
            [self.id.into()],
        );
        let record = DistributionRecord::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("DistributionRecord", self.id.to_string()))?;

        Ok(record)
    }

    /// 获取受益人统计
    pub async fn get_stats_by_beneficiary(
        db: &impl ConnectionTrait,
        beneficiary_id: Uuid,
    ) -> Result<DistributionStats, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT
                COUNT(*) as total_records,
                COALESCE(SUM(share_amount), 0) as total_amount,
                COALESCE(SUM(CASE WHEN status = 'settled' THEN share_amount ELSE 0 END), 0) as settled_amount,
                COALESCE(SUM(CASE WHEN status = 'pending' THEN share_amount ELSE 0 END), 0) as pending_amount
            FROM distribution_records
            WHERE beneficiary_id = $1
            "#,
            [beneficiary_id.into()],
        );
        let stats = DistributionStats::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("stats query failed".to_string()))?;

        Ok(stats)
    }

    /// 获取受益人按层级的统计
    pub async fn get_level_stats_by_beneficiary(
        db: &impl ConnectionTrait,
        beneficiary_id: Uuid,
    ) -> Result<DistributionLevelStats, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT
                COALESCE(SUM(CASE WHEN level = 'level1' THEN share_amount ELSE 0 END), 0) as level1_amount,
                COALESCE(SUM(CASE WHEN level = 'level2' THEN share_amount ELSE 0 END), 0) as level2_amount,
                COUNT(CASE WHEN level = 'level1' THEN 1 END) as level1_count,
                COUNT(CASE WHEN level = 'level2' THEN 1 END) as level2_count
            FROM distribution_records
            WHERE beneficiary_id = $1
            "#,
            [beneficiary_id.into()],
        );
        let stats = DistributionLevelStats::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("stats query failed".to_string()))?;

        Ok(stats)
    }

    /// 获取受益人在某个 usage_log 下的总收益（用于推荐人收益显示）
    pub async fn get_earnings_for_referral(
        db: &impl ConnectionTrait,
        beneficiary_id: Uuid,
        referred_user_id: Uuid,
    ) -> Result<BigDecimal, DbError> {
        // 查询该推荐用户产生的所有分销收益
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT COALESCE(SUM(dr.share_amount), 0)
            FROM distribution_records dr
            JOIN usage_logs ul ON dr.usage_log_id = ul.id
            WHERE dr.beneficiary_id = $1 AND ul.user_id = $2
            "#,
            [beneficiary_id.into(), referred_user_id.into()],
        );
        let result = db
            .query_one(stmt)
            .await?
            .ok_or_else(|| DbError::Other("earnings query failed".to_string()))?;
        let amount: BigDecimal = result.try_get_by_index(0).map_err(DbError::DatabaseError)?;

        Ok(amount)
    }
}
