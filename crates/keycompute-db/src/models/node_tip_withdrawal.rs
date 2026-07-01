//! 小费提现记录模型

use crate::DbError;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 提现方式常量
pub const WITHDRAWAL_TYPE_ALIPAY: &str = "alipay";
pub const WITHDRAWAL_TYPE_BALANCE: &str = "balance";

/// 提现状态常量
pub const WITHDRAWAL_STATUS_PENDING: &str = "pending";
pub const WITHDRAWAL_STATUS_APPROVED: &str = "approved";
pub const WITHDRAWAL_STATUS_COMPLETED: &str = "completed";
pub const WITHDRAWAL_STATUS_REJECTED: &str = "rejected";

/// 小费提现记录
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct NodeTipWithdrawal {
    pub id: Uuid,
    pub user_id: Uuid,
    pub withdrawal_type: String,
    pub total_amount: Decimal,
    pub currency: String,
    pub encrypted_alipay_account: Option<String>,
    pub encrypted_real_name: Option<String>,
    pub status: String,
    pub admin_id: Option<Uuid>,
    pub admin_remark: Option<String>,
    pub actioned_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 提现记录 + 用户邮箱（JOIN 查询结果）
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct NodeTipWithdrawalWithUser {
    pub id: Uuid,
    pub user_id: Uuid,
    pub user_email: String,
    pub withdrawal_type: String,
    pub total_amount: Decimal,
    pub currency: String,
    pub encrypted_alipay_account: Option<String>,
    pub encrypted_real_name: Option<String>,
    pub status: String,
    pub admin_id: Option<Uuid>,
    pub admin_remark: Option<String>,
    pub actioned_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 创建提现请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateTipWithdrawalRequest {
    pub withdrawal_type: String,
    pub alipay_account: Option<String>,
    pub real_name: Option<String>,
}

/// 审批提现请求
#[derive(Debug, Clone, Deserialize)]
pub struct ApproveWithdrawalRequest {
    pub action: String,
    pub remark: Option<String>,
}

/// PII 脱敏静态方法
impl NodeTipWithdrawal {
    pub fn mask_alipay_account(account: &str) -> String {
        if account.is_empty() {
            return "****".to_string();
        }
        if account.contains('@') {
            let parts: Vec<&str> = account.split('@').collect();
            if parts.len() == 2 {
                let user_part = parts[0];
                let domain_part = parts[1];
                if user_part.len() <= 3 {
                    return format!("{}****@{}", &user_part[..1], domain_part);
                }
                return format!("{}****@{}", &user_part[..3], domain_part);
            }
        }
        if account.len() == 11 && account.chars().all(|c| c.is_ascii_digit()) {
            return format!("{}****{}", &account[..3], &account[7..]);
        }
        if account.len() <= 6 {
            return "*".repeat(account.len());
        }
        format!("{}****{}", &account[..3], &account[account.len() - 3..])
    }

    pub fn mask_real_name(name: &str) -> String {
        if name.is_empty() {
            return "****".to_string();
        }
        let chars: Vec<char> = name.chars().collect();
        let len = chars.len();
        if len == 1 {
            return format!("{}*", chars[0]);
        }
        if len == 2 {
            return format!("{}*", chars[0]);
        }
        format!("{}**", chars[0])
    }

    /// 创建提现记录
    pub async fn create(
        db: &impl ConnectionTrait,
        user_id: Uuid,
        withdrawal_type: &str,
        total_amount: Decimal,
        encrypted_alipay_account: Option<&str>,
        encrypted_real_name: Option<&str>,
    ) -> Result<NodeTipWithdrawal, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO node_tip_withdrawals (user_id, withdrawal_type, total_amount, encrypted_alipay_account, encrypted_real_name) VALUES ($1, $2, $3, $4, $5) RETURNING *"#,
            [
                user_id.into(),
                withdrawal_type.into(),
                total_amount.into(),
                encrypted_alipay_account.into(),
                encrypted_real_name.into(),
            ],
        );
        let withdrawal = NodeTipWithdrawal::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(withdrawal)
    }

    /// 根据 ID 查询提现记录
    pub async fn find_by_id(
        db: &DatabaseConnection,
        id: Uuid,
    ) -> Result<Option<NodeTipWithdrawal>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_tip_withdrawals WHERE id = $1",
            [id.into()],
        );
        let w = NodeTipWithdrawal::find_by_statement(stmt).one(db).await?;
        Ok(w)
    }

    /// 获取用户的提现记录列表（分页）
    pub async fn list_by_user(
        db: &DatabaseConnection,
        user_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<NodeTipWithdrawal>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_tip_withdrawals WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
            [user_id.into(), limit.into(), offset.into()],
        );
        let withdrawals = NodeTipWithdrawal::find_by_statement(stmt).all(db).await?;

        Ok(withdrawals)
    }

    /// 获取所有待审批提现记录（管理员用，JOIN 用户邮箱）
    pub async fn list_pending_with_users(
        db: &DatabaseConnection,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<NodeTipWithdrawalWithUser>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT w.id, w.user_id, u.email AS user_email, w.withdrawal_type, w.total_amount, w.currency, w.encrypted_alipay_account, w.encrypted_real_name, w.status, w.admin_id, w.admin_remark, w.actioned_at, w.created_at, w.updated_at FROM node_tip_withdrawals w JOIN users u ON w.user_id = u.id WHERE w.status = 'pending' ORDER BY w.created_at ASC LIMIT $1 OFFSET $2"#,
            [limit.into(), offset.into()],
        );
        let withdrawals = NodeTipWithdrawalWithUser::find_by_statement(stmt)
            .all(db)
            .await?;

        Ok(withdrawals)
    }

    /// 审批通过提现
    pub async fn approve(
        db: &impl ConnectionTrait,
        id: Uuid,
        admin_id: Uuid,
        remark: Option<&str>,
    ) -> Result<NodeTipWithdrawal, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE node_tip_withdrawals SET status = 'approved', admin_id = $1, admin_remark = $2, actioned_at = NOW(), updated_at = NOW() WHERE id = $3 AND status = 'pending' RETURNING *"#,
            [admin_id.into(), remark.into(), id.into()],
        );

        match NodeTipWithdrawal::find_by_statement(stmt).one(db).await? {
            Some(w) => Ok(w),
            None => Err(DbError::Other("提现状态已变更，请刷新后重试".to_string())),
        }
    }

    /// 标记提现为已完成
    pub async fn mark_completed(
        db: &impl ConnectionTrait,
        id: Uuid,
        admin_id: Option<Uuid>,
        remark: Option<&str>,
        total_amount: Option<Decimal>,
    ) -> Result<NodeTipWithdrawal, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE node_tip_withdrawals SET status = 'completed', admin_id = COALESCE($1, admin_id), admin_remark = COALESCE($2, admin_remark), total_amount = COALESCE($3, total_amount), actioned_at = NOW(), updated_at = NOW() WHERE id = $4 AND status IN ('pending', 'approved') RETURNING *"#,
            [
                admin_id.into(),
                remark.into(),
                total_amount.into(),
                id.into(),
            ],
        );

        match NodeTipWithdrawal::find_by_statement(stmt).one(db).await? {
            Some(w) => Ok(w),
            None => Err(DbError::Other("提现状态已变更，请刷新后重试".to_string())),
        }
    }

    /// 拒绝提现
    pub async fn reject(
        db: &impl ConnectionTrait,
        id: Uuid,
        admin_id: Uuid,
        remark: Option<&str>,
    ) -> Result<NodeTipWithdrawal, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE node_tip_withdrawals SET status = 'rejected', admin_id = $1, admin_remark = $2, actioned_at = NOW(), updated_at = NOW() WHERE id = $3 AND status = 'pending' RETURNING *"#,
            [admin_id.into(), remark.into(), id.into()],
        );

        match NodeTipWithdrawal::find_by_statement(stmt).one(db).await? {
            Some(w) => Ok(w),
            None => Err(DbError::Other("提现状态已变更，请刷新后重试".to_string())),
        }
    }
}
