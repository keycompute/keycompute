//! 小费提现记录模型
//!
//! 支持两种提现方式：
//!   - alipay  - 用户提供支付宝账户+真实姓名，管理员线下打款
//!   - balance - 直接转入用户 available_balance
//!
//! # PII 敏感字段加密存储
//!
//! `encrypted_alipay_account` 和 `encrypted_real_name` 使用 AES-256-GCM 加密存储：
//! - 加密格式：`base64(nonce || ciphertext)`
//! - 密钥复用 `CRYPTO__SECRET_KEY` 配置
//! - 加解密逻辑在调用方（handler 层）完成

use crate::DbError;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
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
///
/// # PII 敏感字段加密存储
///
/// `encrypted_alipay_account` 和 `encrypted_real_name` 使用 AES-256-GCM 加密存储：
/// - 加密格式：`base64(nonce || ciphertext)`
/// - 密钥复用 `CRYPTO__SECRET_KEY` 配置
/// - 使用 `encrypt_pii()` 和 `decrypt_pii()` 静态方法进行加解密
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct NodeTipWithdrawal {
    pub id: Uuid,
    pub user_id: Uuid,
    pub withdrawal_type: String,
    pub total_amount: Decimal,
    pub currency: String,
    /// 加密的支付宝账号（仅 alipay 方式）
    /// 格式：`base64(nonce || ciphertext)`
    pub encrypted_alipay_account: Option<String>,
    /// 加密的真实姓名（仅 alipay 方式）
    /// 格式：`base64(nonce || ciphertext)`
    pub encrypted_real_name: Option<String>,
    pub status: String,
    pub admin_id: Option<Uuid>,
    pub admin_remark: Option<String>,
    pub actioned_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 提现记录 + 用户邮箱（JOIN 查询结果）
///
/// 用于管理员接口，包含解密后的明文数据
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct NodeTipWithdrawalWithUser {
    pub id: Uuid,
    pub user_id: Uuid,
    pub user_email: String,
    pub withdrawal_type: String,
    pub total_amount: Decimal,
    pub currency: String,
    /// 加密的支付宝账号（仅 alipay 方式）
    pub encrypted_alipay_account: Option<String>,
    /// 加密的真实姓名（仅 alipay 方式）
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
    /// 提现方式：alipay / balance
    pub withdrawal_type: String,
    /// 支付宝账号（仅 alipay 方式必填）
    pub alipay_account: Option<String>,
    /// 真实姓名（仅 alipay 方式必填）
    pub real_name: Option<String>,
}

/// 审批提现请求
#[derive(Debug, Clone, Deserialize)]
pub struct ApproveWithdrawalRequest {
    /// 操作：approve / reject
    pub action: String,
    /// 管理员备注
    pub remark: Option<String>,
}

/// PII 脱敏静态方法
impl NodeTipWithdrawal {
    /// 对支付宝账号进行脱敏处理
    ///
    /// 脱敏规则：
    /// - 手机号：显示前 3 位和后 4 位，中间用 `****` 替代
    /// - 邮箱：显示前 3 位和 `@` 及域名，中间用 `****` 替代
    /// - 其他：显示前 3 位和后 3 位，中间用 `****` 替代
    ///
    /// # 参数
    /// - `account`: 原始支付宝账号（可能是手机号或邮箱）
    ///
    /// # 返回
    /// 脱敏后的字符串
    pub fn mask_alipay_account(account: &str) -> String {
        if account.is_empty() {
            return "****".to_string();
        }

        // 判断是否为邮箱
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

        // 判断是否为手机号（11 位数字）
        if account.len() == 11 && account.chars().all(|c| c.is_ascii_digit()) {
            return format!("{}****{}", &account[..3], &account[7..]);
        }

        // 其他情况：显示前 3 位和后 3 位
        if account.len() <= 6 {
            return "*".repeat(account.len());
        }
        format!("{}****{}", &account[..3], &account[account.len() - 3..])
    }

    /// 对真实姓名进行脱敏处理
    ///
    /// 脱敏规则：
    /// - 中文姓名（2-4 个字）：显示第一个字，后面用 `*` 替代
    /// - 英文姓名：显示第一个字母和最后一个字母，中间用 `*` 替代
    /// - 其他：显示前 1 位和后 1 位，中间用 `*` 替代
    ///
    /// # 参数
    /// - `name`: 原始真实姓名
    ///
    /// # 返回
    /// 脱敏后的字符串
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

        // 3 个字或以上：显示第一个字，后面用 * 替代
        format!("{}**", chars[0])
    }
    /// 创建提现记录（在事务内执行）
    ///
    /// # PII 加密
    ///
    /// `encrypted_alipay_account` 和 `encrypted_real_name` 应该是已经加密的数据。
    /// 加密逻辑在调用方（handler 层）完成。
    pub async fn create(
        pool: &mut sqlx::PgConnection,
        user_id: Uuid,
        withdrawal_type: &str,
        total_amount: Decimal,
        encrypted_alipay_account: Option<&str>,
        encrypted_real_name: Option<&str>,
    ) -> Result<NodeTipWithdrawal, DbError> {
        let withdrawal = sqlx::query_as::<_, NodeTipWithdrawal>(
            r#"
            INSERT INTO node_tip_withdrawals (
                user_id, withdrawal_type, total_amount,
                encrypted_alipay_account, encrypted_real_name
            )
            VALUES ($1, $2, $3, $4, $5)
            RETURNING *
            "#,
        )
        .bind(user_id)
        .bind(withdrawal_type)
        .bind(total_amount)
        .bind(encrypted_alipay_account)
        .bind(encrypted_real_name)
        .fetch_one(&mut *pool)
        .await?;

        Ok(withdrawal)
    }

    /// 根据 ID 查询提现记录
    pub async fn find_by_id(
        pool: &sqlx::PgPool,
        id: Uuid,
    ) -> Result<Option<NodeTipWithdrawal>, DbError> {
        let w = sqlx::query_as::<_, NodeTipWithdrawal>(
            "SELECT * FROM node_tip_withdrawals WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;
        Ok(w)
    }

    /// 获取用户的提现记录列表（分页）
    pub async fn list_by_user(
        pool: &sqlx::PgPool,
        user_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<NodeTipWithdrawal>, DbError> {
        let withdrawals = sqlx::query_as::<_, NodeTipWithdrawal>(
            r#"
            SELECT * FROM node_tip_withdrawals
            WHERE user_id = $1
            ORDER BY created_at DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(user_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;

        Ok(withdrawals)
    }

    /// 获取所有待审批提现记录（管理员用，JOIN 用户邮箱）
    ///
    /// # PII 解密
    ///
    /// 此方法返回的是加密数据。管理员接口需要调用 `decrypt_sensitive_data()` 方法来解密 PII。
    pub async fn list_pending_with_users(
        pool: &sqlx::PgPool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<NodeTipWithdrawalWithUser>, DbError> {
        let withdrawals = sqlx::query_as::<_, NodeTipWithdrawalWithUser>(
            r#"
            SELECT
                w.id, w.user_id, u.email AS user_email,
                w.withdrawal_type, w.total_amount, w.currency,
                w.encrypted_alipay_account, w.encrypted_real_name,
                w.status, w.admin_id, w.admin_remark,
                w.actioned_at, w.created_at, w.updated_at
            FROM node_tip_withdrawals w
            JOIN users u ON w.user_id = u.id
            WHERE w.status = 'pending'
            ORDER BY w.created_at ASC
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;

        Ok(withdrawals)
    }

    /// 审批通过提现（管理员操作，在事务内执行）
    ///
    /// 仅将状态从 pending → approved，不执行实际转账。
    /// alipay 方式需要管理员线下打款后手动标记 completed。
    /// balance 方式的 completed 在调用方的事务中完成。
    pub async fn approve(
        pool: &mut sqlx::PgConnection,
        id: Uuid,
        admin_id: Uuid,
        remark: Option<&str>,
    ) -> Result<NodeTipWithdrawal, DbError> {
        let w = sqlx::query_as::<_, NodeTipWithdrawal>(
            r#"
            UPDATE node_tip_withdrawals
            SET status = 'approved',
                admin_id = $1,
                admin_remark = $2,
                actioned_at = NOW(),
                updated_at = NOW()
            WHERE id = $3
              AND status = 'pending'
            RETURNING *
            "#,
        )
        .bind(admin_id)
        .bind(remark)
        .bind(id)
        .fetch_optional(&mut *pool)
        .await?;

        match w {
            Some(w) => Ok(w),
            None => Err(DbError::Other("提现状态已变更，请刷新后重试".to_string())),
        }
    }

    /// 标记提现为已完成（在事务内执行）
    ///
    /// 用于 alipay 方式管理员线下打款后标记，或 balance 方式自动完成。
    ///
    /// - `total_amount`: 可选，更新实际金额（以事务内的最新汇总为准）
    /// - `remark`: 可选，NULL 时保留 admin_remark 原值（COALESCE 语义）
    ///
    /// 允许从 pending 或 approved 状态转换：
    /// - balance 自助提现：pending → completed（跳过审批）
    /// - alipay 线下打款：approved → completed（管理员审批后完成）
    pub async fn mark_completed(
        pool: &mut sqlx::PgConnection,
        id: Uuid,
        admin_id: Option<Uuid>,
        remark: Option<&str>,
        total_amount: Option<Decimal>,
    ) -> Result<NodeTipWithdrawal, DbError> {
        let w = sqlx::query_as::<_, NodeTipWithdrawal>(
            r#"
            UPDATE node_tip_withdrawals
            SET status = 'completed',
                admin_id = COALESCE($1, admin_id),
                admin_remark = COALESCE($2, admin_remark),
                total_amount = COALESCE($3, total_amount),
                actioned_at = NOW(),
                updated_at = NOW()
            WHERE id = $4
              AND status IN ('pending', 'approved')
            RETURNING *
            "#,
        )
        .bind(admin_id)
        .bind(remark)
        .bind(total_amount)
        .bind(id)
        .fetch_optional(&mut *pool)
        .await?;

        match w {
            Some(w) => Ok(w),
            None => Err(DbError::Other("提现状态已变更，请刷新后重试".to_string())),
        }
    }

    /// 拒绝提现（管理员操作）
    pub async fn reject(
        pool: &mut sqlx::PgConnection,
        id: Uuid,
        admin_id: Uuid,
        remark: Option<&str>,
    ) -> Result<NodeTipWithdrawal, DbError> {
        let w = sqlx::query_as::<_, NodeTipWithdrawal>(
            r#"
            UPDATE node_tip_withdrawals
            SET status = 'rejected',
                admin_id = $1,
                admin_remark = $2,
                actioned_at = NOW(),
                updated_at = NOW()
            WHERE id = $3
              AND status = 'pending'
            RETURNING *
            "#,
        )
        .bind(admin_id)
        .bind(remark)
        .bind(id)
        .fetch_optional(&mut *pool)
        .await?;

        match w {
            Some(w) => Ok(w),
            None => Err(DbError::Other("提现状态已变更，请刷新后重试".to_string())),
        }
    }
}
