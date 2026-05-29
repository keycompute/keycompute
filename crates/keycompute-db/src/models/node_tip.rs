//! 节点租赁小费模型
//!
//! 当用户通过 node gateway 发起会话并完成计费后，节点提供者（owner）获得小费。
//! tips = usage_log.user_amount * node_tip_ratio
//!
//! 提现流程：
//!   1. alipay  - 用户提供支付宝账户+姓名，管理员线下打款后标记已提现
//!   2. balance - 直接转入用户 available_balance

use crate::DbError;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::str::FromStr;
use uuid::Uuid;

/// 节点租赁小费记录
///
/// 小费提现以账户为单元：所有小费归入同一账户，提现时一次性提取全部待提现金额。
/// 提现状态由 node_tip_withdrawals 表管理，本表不追踪单笔小费的提现状态。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
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
    /// 待提现总额
    pub pending_amount: Decimal,
    /// 已提现总额
    pub withdrawn_amount: Decimal,
    /// 累计小费总额
    pub total_amount: Decimal,
    /// 待提现笔数
    pub pending_count: i64,
}

/// 小费汇总查询结果（内部用 FromRow）
#[derive(Debug, Clone, FromRow)]
struct TipSummaryRow {
    pending_amount: Option<Decimal>,
    withdrawn_amount: Option<Decimal>,
    total_amount: Option<Decimal>,
    pending_count: Option<i64>,
}

impl NodeTip {
    /// 获取用户的小费汇总
    ///
    /// 提现状态由 node_tip_withdrawals 表推导：
    /// - pending_amount = 总小费 - 非拒绝提现单总额
    /// - withdrawn_amount = 非拒绝提现单总额
    /// - total_amount = 历史累计小费总额
    pub async fn get_summary(
        pool: &sqlx::PgPool,
        user_id: Uuid,
    ) -> Result<NodeTipSummary, DbError> {
        let row = sqlx::query_as::<_, TipSummaryRow>(
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
        )
        .bind(user_id)
        .fetch_one(pool)
        .await?;

        Ok(NodeTipSummary {
            pending_amount: row.pending_amount.unwrap_or_default(),
            withdrawn_amount: row.withdrawn_amount.unwrap_or_default(),
            total_amount: row.total_amount.unwrap_or_default(),
            pending_count: row.pending_count.unwrap_or_default(),
        })
    }

    /// 获取用户的小费历史记录（分页）
    pub async fn list_by_user(
        pool: &sqlx::PgPool,
        user_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<NodeTip>, DbError> {
        let tips = sqlx::query_as::<_, NodeTip>(
            r#"
            SELECT * FROM node_tips
            WHERE owner_user_id = $1
            ORDER BY created_at DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(user_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;

        Ok(tips)
    }

    /// 获取用户小费记录总数
    pub async fn count_by_user(pool: &sqlx::PgPool, user_id: Uuid) -> Result<i64, DbError> {
        let count =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM node_tips WHERE owner_user_id = $1")
                .bind(user_id)
                .fetch_one(pool)
                .await?;

        Ok(count)
    }

    /// 根据 usage_log 自动创建小费记录（计费完成后调用）
    ///
    /// 流程：
    ///   1. 查询 usage_log 获取 user_amount 和 request_id
    ///   2. 查询 node_task（通过 request_id）获取 assigned_node_id
    ///   3. 查询 node 获取 owner_user_id
    ///   4. 读取 system_settings 中的 node_tip_ratio
    ///   5. 计算 tips = user_amount * ratio，创建 node_tips 记录
    ///
    /// 如果 usage_log 不是来自 node gateway（无对应 node_task），则静默返回 Ok(None)。
    ///
    /// 所有读取和写入在同一事务中执行，防止 tip_ratio 在读取后、写入前被管理员修改。
    pub async fn create_from_usage_log(
        pool: &sqlx::PgPool,
        usage_log_id: Uuid,
    ) -> Result<Option<NodeTip>, DbError> {
        let mut tx = pool.begin().await?;

        // 1. 查询 usage_log
        let usage_log = sqlx::query_as::<_, super::usage_log::UsageLog>(
            "SELECT * FROM usage_logs WHERE id = $1",
        )
        .bind(usage_log_id)
        .fetch_optional(&mut *tx)
        .await?;

        let usage_log = match usage_log {
            Some(log) => log,
            None => {
                tracing::warn!(%usage_log_id, "UsageLog not found, skipping tip creation");
                tx.rollback().await?;
                return Ok(None);
            }
        };

        // 2. 查询对应的 node_task（仅成功的任务)
        let task_row: Option<(Uuid, Uuid)> = sqlx::query_as(
            r#"
            SELECT assigned_node_id, user_id
            FROM node_tasks
            WHERE request_id = $1
              AND status = 'succeeded'
              AND assigned_node_id IS NOT NULL
            "#,
        )
        .bind(usage_log.request_id)
        .fetch_optional(&mut *tx)
        .await?;

        let (node_id, consumer_user_id) = match task_row {
            Some(row) => row,
            None => {
                // 不是 node gateway 请求，或任务未成功
                tx.rollback().await?;
                return Ok(None);
            }
        };

        // 3. 查询节点所有者
        let node = sqlx::query_as::<_, super::node::Node>("SELECT * FROM nodes WHERE id = $1")
            .bind(node_id)
            .fetch_optional(&mut *tx)
            .await?;

        let node = match node {
            Some(n) => n,
            None => {
                tracing::warn!(%node_id, "Node not found, skipping tip creation");
                tx.rollback().await?;
                return Ok(None);
            }
        };

        // 节点所有者不能是自己（消费者也是自己的节点时不产生小费）
        if node.owner_user_id == consumer_user_id {
            tx.rollback().await?;
            return Ok(None);
        }

        // 4. 读取小费比例（事务内读取，防止在计算与写入之间被管理员修改）
        let ratio_str: String =
            sqlx::query_scalar("SELECT value FROM system_settings WHERE key = $1")
                .bind(super::system_setting::setting_keys::NODE_TIP_RATIO)
                .fetch_optional(&mut *tx)
                .await?
                .unwrap_or_else(|| "0.90".to_string());

        let tip_ratio: Decimal = ratio_str.parse().unwrap_or_else(|_| {
            tracing::warn!(
                ratio_str = %ratio_str,
                "Invalid node_tip_ratio in system_settings, falling back to 0.9"
            );
            Decimal::new(9, 1) // fallback 0.9
        });

        if tip_ratio <= Decimal::ZERO {
            tx.rollback().await?;
            return Ok(None);
        }

        // 5. 转换 BigDecimal → Decimal（通过字符串桥接，避免 f64 精度损失）
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
            tx.rollback().await?;
            return Ok(None);
        }

        // 6. 计算小费 = 计费金额 * 比例，保留 10 位小数（与 usage_logs DECIMAL(20,10) 对齐）
        let bill_amount = bill_amount.round_dp(10);
        let tip_amount = (bill_amount * tip_ratio).round_dp(10);

        // 7. 创建小费记录（在事务内使用 ON CONFLICT DO NOTHING 防止重复创建）
        let tip = sqlx::query_as::<_, NodeTip>(
            r#"
            INSERT INTO node_tips (
                usage_log_id, node_id, owner_user_id, consumer_user_id,
                tip_amount, tip_ratio, bill_amount
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (usage_log_id) DO NOTHING
            RETURNING *
            "#,
        )
        .bind(usage_log_id)
        .bind(node_id)
        .bind(node.owner_user_id)
        .bind(consumer_user_id)
        .bind(tip_amount)
        .bind(tip_ratio)
        .bind(bill_amount)
        .fetch_optional(&mut *tx)
        .await?;

        let tip = match tip {
            Some(t) => {
                tx.commit().await?;
                t
            }
            None => {
                tracing::debug!(%usage_log_id, "Tip already exists (ON CONFLICT), skipping duplicate creation");
                tx.rollback().await?;
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
