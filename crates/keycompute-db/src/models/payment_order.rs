//! 支付订单模型

use crate::DbError;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// 订单状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaymentOrderStatus {
    /// 待支付
    Pending,
    /// 已支付
    Paid,
    /// 支付失败
    Failed,
    /// 已关闭
    Closed,
}

impl PaymentOrderStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PaymentOrderStatus::Pending => "pending",
            PaymentOrderStatus::Paid => "paid",
            PaymentOrderStatus::Failed => "failed",
            PaymentOrderStatus::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(PaymentOrderStatus::Pending),
            "paid" => Some(PaymentOrderStatus::Paid),
            "failed" => Some(PaymentOrderStatus::Failed),
            "closed" => Some(PaymentOrderStatus::Closed),
            _ => None,
        }
    }
}

impl std::fmt::Display for PaymentOrderStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 支付方式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaymentMethod {
    /// 支付宝
    Alipay,
    /// 微信支付
    WechatPay,
}

impl PaymentMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            PaymentMethod::Alipay => "alipay",
            PaymentMethod::WechatPay => "wechatpay",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "alipay" => Some(PaymentMethod::Alipay),
            "wechatpay" => Some(PaymentMethod::WechatPay),
            _ => None,
        }
    }
}

/// 支付订单模型
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PaymentOrder {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    /// 商户订单号（外部订单号）
    pub out_trade_no: String,
    /// 支付宝交易号
    pub trade_no: Option<String>,
    /// 订单金额（单位：元）
    pub amount: Decimal,
    /// 币种
    pub currency: String,
    /// 订单状态
    pub status: String,
    /// 支付方式
    pub payment_method: String,
    /// 商品标题
    pub subject: String,
    /// 商品描述
    pub body: Option<String>,
    /// 支付时间
    pub paid_at: Option<DateTime<Utc>>,
    /// 关闭时间
    pub closed_at: Option<DateTime<Utc>>,
    /// 过期时间
    pub expired_at: DateTime<Utc>,
    /// 支付URL
    pub pay_url: Option<String>,
    /// 回调通知原始数据
    pub notify_data: Option<sqlx::types::Json<serde_json::Value>>,
    /// 备注信息
    pub remarks: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PaymentOrder {
    /// 获取订单状态枚举
    pub fn get_status(&self) -> Option<PaymentOrderStatus> {
        PaymentOrderStatus::parse(&self.status)
    }

    /// 检查订单是否可支付
    pub fn is_payable(&self) -> bool {
        self.get_status() == Some(PaymentOrderStatus::Pending) && self.expired_at > Utc::now()
    }

    /// 检查订单是否已过期
    pub fn is_expired(&self) -> bool {
        self.expired_at <= Utc::now()
    }
}

/// 创建支付订单请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreatePaymentOrderRequest {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    /// 订单金额（单位：元）
    pub amount: Decimal,
    /// 商品标题
    pub subject: String,
    /// 商品描述
    pub body: Option<String>,
    /// 过期时间（分钟），默认30分钟
    #[serde(default = "default_expire_minutes")]
    pub expire_minutes: i32,
}

fn default_expire_minutes() -> i32 {
    30
}

impl PaymentOrder {
    /// 创建新订单
    pub async fn create(
        pool: &sqlx::PgPool,
        req: &CreatePaymentOrderRequest,
        out_trade_no: &str,
        pay_url: &str,
    ) -> Result<PaymentOrder, DbError> {
        let expired_at = Utc::now() + chrono::Duration::minutes(req.expire_minutes as i64);

        let order = sqlx::query_as::<_, PaymentOrder>(
            r#"
            INSERT INTO payment_orders (
                tenant_id, user_id, out_trade_no, amount,
                currency, status, payment_method, subject, body,
                expired_at, pay_url
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            RETURNING *
            "#,
        )
        .bind(req.tenant_id)
        .bind(req.user_id)
        .bind(out_trade_no)
        .bind(req.amount)
        .bind("CNY")
        .bind(PaymentOrderStatus::Pending.as_str())
        .bind(PaymentMethod::Alipay.as_str())
        .bind(&req.subject)
        .bind(&req.body)
        .bind(expired_at)
        .bind(pay_url)
        .fetch_one(pool)
        .await?;

        Ok(order)
    }

    /// 根据ID查找订单
    pub async fn find_by_id(
        pool: &sqlx::PgPool,
        id: Uuid,
    ) -> Result<Option<PaymentOrder>, DbError> {
        let order = sqlx::query_as::<_, PaymentOrder>("SELECT * FROM payment_orders WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
        Ok(order)
    }

    /// 根据商户订单号查找订单
    pub async fn find_by_out_trade_no(
        pool: &sqlx::PgPool,
        out_trade_no: &str,
    ) -> Result<Option<PaymentOrder>, DbError> {
        let order = sqlx::query_as::<_, PaymentOrder>(
            "SELECT * FROM payment_orders WHERE out_trade_no = $1",
        )
        .bind(out_trade_no)
        .fetch_optional(pool)
        .await?;
        Ok(order)
    }

    /// 根据支付宝交易号查找订单
    pub async fn find_by_trade_no(
        pool: &sqlx::PgPool,
        trade_no: &str,
    ) -> Result<Option<PaymentOrder>, DbError> {
        let order =
            sqlx::query_as::<_, PaymentOrder>("SELECT * FROM payment_orders WHERE trade_no = $1")
                .bind(trade_no)
                .fetch_optional(pool)
                .await?;
        Ok(order)
    }

    /// 查找用户的订单列表
    pub async fn find_by_user(
        pool: &sqlx::PgPool,
        user_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PaymentOrder>, DbError> {
        let orders = sqlx::query_as::<_, PaymentOrder>(
            r#"
            SELECT * FROM payment_orders
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
        Ok(orders)
    }

    /// 更新订单为已支付
    ///
    /// # 已废弃
    /// 此方法没有并发保护，可能导致重复处理。
    /// 请使用 `handle_notify` 或 `sync_order_status` 方法替代，它们在事务中处理。
    #[deprecated(
        since = "0.2.0",
        note = "此方法没有并发保护，请使用 PaymentService 中的 handle_notify 或 sync_order_status"
    )]
    pub async fn mark_as_paid(
        pool: &sqlx::PgPool,
        id: Uuid,
        trade_no: &str,
        notify_data: &serde_json::Value,
    ) -> Result<PaymentOrder, DbError> {
        let order = sqlx::query_as::<_, PaymentOrder>(
            r#"
            UPDATE payment_orders
            SET status = $1,
                trade_no = $2,
                notify_data = $3,
                paid_at = NOW(),
                updated_at = NOW()
            WHERE id = $4
            RETURNING *
            "#,
        )
        .bind(PaymentOrderStatus::Paid.as_str())
        .bind(trade_no)
        .bind(sqlx::types::Json(notify_data))
        .bind(id)
        .fetch_one(pool)
        .await?;
        Ok(order)
    }

    /// 更新订单为支付失败
    ///
    /// # 注意
    /// 只有 pending 状态的订单才能标记为失败，避免覆盖已支付状态
    pub async fn mark_as_failed(pool: &sqlx::PgPool, id: Uuid) -> Result<PaymentOrder, DbError> {
        let order = sqlx::query_as::<_, PaymentOrder>(
            r#"
            UPDATE payment_orders
            SET status = $1,
                updated_at = NOW()
            WHERE id = $2 AND status = $3
            RETURNING *
            "#,
        )
        .bind(PaymentOrderStatus::Failed.as_str())
        .bind(id)
        .bind(PaymentOrderStatus::Pending.as_str())
        .fetch_optional(pool)
        .await?;

        // 如果订单已被处理，返回现有订单
        match order {
            Some(o) => Ok(o),
            None => {
                // 订单已被其他事务处理，查询当前状态
                let existing =
                    sqlx::query_as::<_, PaymentOrder>("SELECT * FROM payment_orders WHERE id = $1")
                        .bind(id)
                        .fetch_one(pool)
                        .await?;
                Ok(existing)
            }
        }
    }

    /// 关闭订单
    ///
    /// # Errors
    /// - `DbError::NotFound` - 订单不存在
    /// - `DbError::InvalidOrderStatus` - 订单状态不是 pending
    pub async fn close(pool: &sqlx::PgPool, id: Uuid) -> Result<PaymentOrder, DbError> {
        let order = sqlx::query_as::<_, PaymentOrder>(
            r#"
            UPDATE payment_orders
            SET status = $1,
                closed_at = NOW(),
                updated_at = NOW()
            WHERE id = $2 AND status = $3
            RETURNING *
            "#,
        )
        .bind(PaymentOrderStatus::Closed.as_str())
        .bind(id)
        .bind(PaymentOrderStatus::Pending.as_str())
        .fetch_optional(pool)
        .await?;

        match order {
            Some(order) => Ok(order),
            None => {
                // 检查订单是否存在
                if let Some(existing) = Self::find_by_id(pool, id).await? {
                    Err(DbError::InvalidOrderStatus {
                        expected: "pending".to_string(),
                        actual: existing.status,
                    })
                } else {
                    Err(DbError::not_found("PaymentOrder", id.to_string()))
                }
            }
        }
    }

    /// 关闭过期订单
    pub async fn close_expired_orders(pool: &sqlx::PgPool) -> Result<u64, DbError> {
        let result = sqlx::query(
            r#"
            UPDATE payment_orders
            SET status = $1,
                closed_at = NOW(),
                updated_at = NOW()
            WHERE status = $2 AND expired_at < NOW()
            "#,
        )
        .bind(PaymentOrderStatus::Closed.as_str())
        .bind(PaymentOrderStatus::Pending.as_str())
        .execute(pool)
        .await?;
        Ok(result.rows_affected())
    }
}

/// 支付订单统计
#[derive(Debug, Clone, FromRow)]
pub struct PaymentOrderStats {
    pub total_orders: i64,
    pub total_amount: Decimal,
    pub paid_orders: i64,
    pub paid_amount: Decimal,
    pub pending_orders: i64,
    pub pending_amount: Decimal,
}

impl PaymentOrder {
    /// 获取用户订单统计
    pub async fn get_user_stats(
        pool: &sqlx::PgPool,
        user_id: Uuid,
    ) -> Result<PaymentOrderStats, DbError> {
        let stats = sqlx::query_as::<_, PaymentOrderStats>(
            r#"
            SELECT
                COUNT(*) as total_orders,
                COALESCE(SUM(amount), 0) as total_amount,
                COUNT(*) FILTER (WHERE status = 'paid') as paid_orders,
                COALESCE(SUM(amount) FILTER (WHERE status = 'paid'), 0) as paid_amount,
                COUNT(*) FILTER (WHERE status = 'pending') as pending_orders,
                COALESCE(SUM(amount) FILTER (WHERE status = 'pending'), 0) as pending_amount
            FROM payment_orders
            WHERE user_id = $1
            "#,
        )
        .bind(user_id)
        .fetch_one(pool)
        .await?;
        Ok(stats)
    }
}
