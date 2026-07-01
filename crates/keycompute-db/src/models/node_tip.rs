//! 节点租赁小费模型

use crate::DbError;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sea_orm::{
    TransactionTrait, {ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement},
};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use uuid::Uuid;

/// 节点租赁小费记录
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct NodeTip {
    pub id: Uuid,
    pub usage_log_id: Uuid,
    pub node_id: Uuid,
    pub owner_user_id: Uuid,
    pub consumer_user_id: Uuid,
    pub tip_amount: Decimal,
    pub currency: String,
    pub tip_ratio: Decimal,
    pub bill_amount: Decimal,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 小费汇总信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeTipSummary {
    pub pending_amount: Decimal,
    pub withdrawn_amount: Decimal,
    pub total_amount: Decimal,
    pub pending_count: i64,
}

/// 小费汇总查询结果（内部用）
#[derive(Debug, Clone, FromQueryResult)]
struct TipSummaryRow {
    pending_amount: Option<Decimal>,
    withdrawn_amount: Option<Decimal>,
    total_amount: Option<Decimal>,
    pending_count: Option<i64>,
}

impl NodeTip {
    /// 获取用户的小费汇总
    pub async fn get_summary(
        db: &DatabaseConnection,
        user_id: Uuid,
    ) -> Result<NodeTipSummary, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT
                COALESCE((
                    SELECT SUM(nt.tip_amount)
                    FROM node_tips nt
                    WHERE nt.owner_user_id = $1
                ), 0)
                - COALESCE((
                    SELECT SUM(ntw.total_amount)
                    FROM node_tip_withdrawals ntw
                    WHERE ntw.user_id = $1 AND ntw.status != 'rejected'
                ), 0) AS pending_amount,
                COALESCE((
                    SELECT SUM(ntw.total_amount)
                    FROM node_tip_withdrawals ntw
                    WHERE ntw.user_id = $1 AND ntw.status != 'rejected'
                ), 0) AS withdrawn_amount,
                COALESCE((
                    SELECT SUM(nt.tip_amount)
                    FROM node_tips nt
                    WHERE nt.owner_user_id = $1
                ), 0) AS total_amount,
                COALESCE((
                    SELECT COUNT(*)
                    FROM node_tips nt
                    WHERE nt.owner_user_id = $1
                ), 0) AS pending_count
            "#,
            [user_id.into()],
        );
        let row = TipSummaryRow::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("summary query failed".to_string()))?;

        Ok(NodeTipSummary {
            pending_amount: row.pending_amount.unwrap_or_default(),
            withdrawn_amount: row.withdrawn_amount.unwrap_or_default(),
            total_amount: row.total_amount.unwrap_or_default(),
            pending_count: row.pending_count.unwrap_or_default(),
        })
    }

    /// 获取用户的小费历史记录（分页）
    pub async fn list_by_user(
        db: &DatabaseConnection,
        user_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<NodeTip>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_tips WHERE owner_user_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
            [user_id.into(), limit.into(), offset.into()],
        );
        let tips = NodeTip::find_by_statement(stmt).all(db).await?;

        Ok(tips)
    }

    /// 获取用户小费记录总数
    pub async fn count_by_user(db: &DatabaseConnection, user_id: Uuid) -> Result<i64, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*) FROM node_tips WHERE owner_user_id = $1",
            [user_id.into()],
        );
        let result = db
            .query_one(stmt)
            .await?
            .ok_or_else(|| DbError::Other("count query failed".to_string()))?;
        let count: i64 = result.try_get_by_index(0).map_err(DbError::DatabaseError)?;

        Ok(count)
    }

    /// 根据 usage_log 自动创建小费记录（计费完成后调用）
    pub async fn create_from_usage_log(
        db: &DatabaseConnection,
        usage_log_id: Uuid,
    ) -> Result<Option<NodeTip>, DbError> {
        let txn = db.begin().await?;

        // 1. 查询 usage_log
        let usage_log_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM usage_logs WHERE id = $1",
            [usage_log_id.into()],
        );
        let usage_log = super::usage_log::UsageLog::find_by_statement(usage_log_stmt)
            .one(&txn)
            .await?;

        let usage_log = match usage_log {
            Some(log) => log,
            None => {
                tracing::warn!(%usage_log_id, "UsageLog not found, skipping tip creation");
                txn.rollback().await?;
                return Ok(None);
            }
        };

        // 2. 查询对应的 node_task（仅成功的任务)
        let task_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT assigned_node_id, user_id
            FROM node_tasks
            WHERE request_id = $1
              AND status = 'succeeded'
              AND assigned_node_id IS NOT NULL
            "#,
            [usage_log.request_id.into()],
        );
        let task_row: Option<(Uuid, Uuid)> = {
            let result = txn.query_one(task_stmt).await?;
            match result {
                Some(row) => {
                    let node_id: Uuid = row.try_get_by_index(0).map_err(DbError::DatabaseError)?;
                    let user_id: Uuid = row.try_get_by_index(1).map_err(DbError::DatabaseError)?;
                    Some((node_id, user_id))
                }
                None => None,
            }
        };

        let (node_id, consumer_user_id) = match task_row {
            Some(row) => row,
            None => {
                txn.rollback().await?;
                return Ok(None);
            }
        };

        // 3. 查询节点所有者
        let node_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM nodes WHERE id = $1",
            [node_id.into()],
        );
        let node = super::node::Node::find_by_statement(node_stmt)
            .one(&txn)
            .await?;

        let node = match node {
            Some(n) => n,
            None => {
                tracing::warn!(%node_id, "Node not found, skipping tip creation");
                txn.rollback().await?;
                return Ok(None);
            }
        };

        if node.owner_user_id == consumer_user_id {
            txn.rollback().await?;
            return Ok(None);
        }

        // 4. 读取小费比例（事务内读取）
        let ratio_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT value FROM system_settings WHERE key = $1",
            [super::system_setting::setting_keys::NODE_TIP_RATIO.into()],
        );
        let ratio_str: String = txn
            .query_one(ratio_stmt)
            .await?
            .and_then(|r| r.try_get_by_index::<String>(0).ok())
            .unwrap_or_else(|| "0.90".to_string());

        let tip_ratio: Decimal = ratio_str.parse().unwrap_or_else(|_| {
            tracing::warn!(
                ratio_str = %ratio_str,
                "Invalid node_tip_ratio in system_settings, falling back to 0.9"
            );
            Decimal::new(9, 1)
        });

        if tip_ratio <= Decimal::ZERO {
            txn.rollback().await?;
            return Ok(None);
        }

        // 5. 转换 BigDecimal → Decimal
        let bill_amount: Decimal =
            Decimal::from_str(&usage_log.user_amount.to_string()).map_err(|e| {
                tracing::error!(
                    %usage_log_id,
                    amount = %usage_log.user_amount,
                    error = %e,
                    "Failed to convert BigDecimal to Decimal for tip calculation"
                );
                DbError::Other(format!(
                    "Failed to convert BigDecimal to Decimal for usage_log {}: {}",
                    usage_log_id, e
                ))
            })?;

        if bill_amount <= Decimal::ZERO {
            txn.rollback().await?;
            return Ok(None);
        }

        let bill_amount = bill_amount.round_dp(10);
        let tip_amount = (bill_amount * tip_ratio).round_dp(10);

        // 7. 创建小费记录
        let tip_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO node_tips (
                usage_log_id, node_id, owner_user_id, consumer_user_id,
                tip_amount, tip_ratio, bill_amount
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (usage_log_id) DO NOTHING
            RETURNING *
            "#,
            [
                usage_log_id.into(),
                node_id.into(),
                node.owner_user_id.into(),
                consumer_user_id.into(),
                tip_amount.into(),
                tip_ratio.into(),
                bill_amount.into(),
            ],
        );
        let tip = NodeTip::find_by_statement(tip_stmt).one(&txn).await?;

        let tip = match tip {
            Some(t) => {
                txn.commit().await?;
                t
            }
            None => {
                tracing::debug!(%usage_log_id, "Tip already exists (ON CONFLICT), skipping duplicate creation");
                txn.rollback().await?;
                return Ok(None);
            }
        };

        tracing::info!(
            %usage_log_id,
            %node_id,
            owner_user_id = %node.owner_user_id,
            %tip_amount,
            %tip_ratio,
            "Tip created for node lease"
        );

        Ok(Some(tip))
    }
}
