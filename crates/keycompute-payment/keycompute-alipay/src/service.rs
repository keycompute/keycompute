//! 支付服务核心逻辑
//
//! 整合支付宝客户端和数据库操作，提供完整的支付流程

use crate::client::{AlipayClient, QueryResponse};
use crate::config::AlipayConfig;
use chrono::{DateTime, Utc};
use keycompute_db::DbRouter;
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

mod urlencoding {
    use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

    pub fn encode(s: &str) -> String {
        utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
    }
}

/// 支付服务
pub struct PaymentService {
    client: AlipayClient,
    pool: Arc<DbRouter>,
}

impl PaymentService {
    /// 创建新的支付服务
    pub fn new(config: AlipayConfig, pool: Arc<DbRouter>) -> Result<Self, PaymentError> {
        let client = AlipayClient::new(config)?;
        Ok(Self { client, pool })
    }

    /// 从环境变量创建支付服务
    pub async fn from_env(pool: Arc<DbRouter>) -> Result<Self, PaymentError> {
        let config = AlipayConfig::from_env()?;
        Self::new(config, pool)
    }

    /// 创建支付订单
    ///
    /// 返回支付URL，用于前端跳转到支付宝支付页面
    pub async fn create_order(
        &self,
        req: CreateOrderRequest,
    ) -> Result<CreateOrderResult, PaymentError> {
        // 生成商户订单号
        let out_trade_no = generate_out_trade_no();

        // 格式化金额
        let amount_str = format!("{:.2}", req.amount);

        // 生成支付URL
        let pay_url = self.client.page_pay_url(
            &out_trade_no,
            &amount_str,
            &req.subject,
            req.body.as_deref(),
        )?;

        // 创建数据库订单记录
        let db_req = keycompute_db::CreatePaymentOrderRequest {
            tenant_id: req.tenant_id,
            user_id: req.user_id,
            amount: req.amount,
            subject: req.subject.clone(),
            body: req.body.clone(),
            expire_minutes: self.client.config().timeout_minutes,
        };

        let order = keycompute_db::PaymentOrder::create(
            self.pool.as_ref(),
            &db_req,
            &out_trade_no,
            &pay_url,
        )
        .await
        .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

        Ok(CreateOrderResult {
            order_id: order.id,
            out_trade_no: order.out_trade_no,
            pay_url: order.pay_url.unwrap_or_default(),
            expired_at: order.expired_at,
        })
    }

    /// 创建手机网站支付订单
    pub async fn create_wap_order(
        &self,
        req: CreateOrderRequest,
    ) -> Result<CreateOrderResult, PaymentError> {
        let out_trade_no = generate_out_trade_no();
        let amount_str = format!("{:.2}", req.amount);

        let pay_url = self.client.wap_pay_url(
            &out_trade_no,
            &amount_str,
            &req.subject,
            req.body.as_deref(),
        )?;

        let db_req = keycompute_db::CreatePaymentOrderRequest {
            tenant_id: req.tenant_id,
            user_id: req.user_id,
            amount: req.amount,
            subject: req.subject.clone(),
            body: req.body.clone(),
            expire_minutes: self.client.config().timeout_minutes,
        };

        let order = keycompute_db::PaymentOrder::create(
            self.pool.as_ref(),
            &db_req,
            &out_trade_no,
            &pay_url,
        )
        .await
        .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

        Ok(CreateOrderResult {
            order_id: order.id,
            out_trade_no: order.out_trade_no,
            pay_url: order.pay_url.unwrap_or_default(),
            expired_at: order.expired_at,
        })
    }

    /// 创建扫码支付订单（当面付）
    ///
    /// 生成支付二维码，用户使用支付宝扫码完成支付
    /// 返回二维码内容和订单信息
    ///
    /// # 执行流程
    /// 1. 先创建数据库订单记录（状态为 pending）
    /// 2. 再调用支付宝 precreate API 获取二维码
    /// 3. 更新数据库订单的 pay_url 字段
    ///
    /// 这样可以避免：支付宝 precreate 成功但数据库失败导致的不一致
    pub async fn create_qr_order(
        &self,
        req: CreateOrderRequest,
    ) -> Result<CreateQrOrderResult, PaymentError> {
        // 生成商户订单号
        let out_trade_no = generate_out_trade_no();

        // 格式化金额
        let amount_str = format!("{:.2}", req.amount);

        // 先创建数据库订单记录（状态为 pending）
        let db_req = keycompute_db::CreatePaymentOrderRequest {
            tenant_id: req.tenant_id,
            user_id: req.user_id,
            amount: req.amount,
            subject: req.subject.clone(),
            body: req.body.clone(),
            expire_minutes: self.client.config().timeout_minutes,
        };

        // 临时使用空字符串作为 pay_url，后续更新
        let order =
            keycompute_db::PaymentOrder::create(self.pool.as_ref(), &db_req, &out_trade_no, "")
                .await
                .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

        // 调用支付宝 precreate 接口
        let precreate_result = self
            .client
            .precreate(
                &out_trade_no,
                &amount_str,
                &req.subject,
                req.body.as_deref(),
            )
            .await;

        let precreate_response = match precreate_result {
            Ok(r) => r,
            Err(e) => {
                // precreate 调用失败，标记订单为失败状态
                if let Err(mark_err) =
                    keycompute_db::PaymentOrder::mark_as_failed(self.pool.as_ref(), order.id).await
                {
                    tracing::error!("Failed to mark order {} as failed: {}", order.id, mark_err);
                }
                return Err(PaymentError::ApiError(e.to_string()));
            }
        };

        if !precreate_response.is_success() {
            // precreate 返回失败，标记订单为失败状态
            if let Err(mark_err) =
                keycompute_db::PaymentOrder::mark_as_failed(self.pool.as_ref(), order.id).await
            {
                tracing::error!("Failed to mark order {} as failed: {}", order.id, mark_err);
            }
            return Err(PaymentError::ApiError(
                precreate_response
                    .sub_msg
                    .unwrap_or_else(|| precreate_response.msg.clone()),
            ));
        }

        let qr_code = precreate_response.qr_code.clone().unwrap_or_default();

        // 更新数据库订单的 pay_url 字段
        let stmt = sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            r#"UPDATE payment_orders SET pay_url = $1 WHERE id = $2"#,
            [qr_code.as_str().into(), order.id.into()],
        );
        self.pool
            .execute(stmt)
            .await
            .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

        Ok(CreateQrOrderResult {
            order_id: order.id,
            out_trade_no: order.out_trade_no,
            qr_code,
            expired_at: order.expired_at,
        })
    }

    /// 查询订单状态
    pub async fn query_order(&self, out_trade_no: &str) -> Result<QueryResponse, PaymentError> {
        self.client
            .query_order(out_trade_no)
            .await
            .map_err(|e| PaymentError::ApiError(e.to_string()))
    }

    /// 处理支付成功回调
    ///
    /// 验签成功后更新订单状态并充值用户余额
    pub async fn handle_notify(
        &self,
        params: HashMap<String, String>,
    ) -> Result<NotifyResult, PaymentError> {
        // 转换为参数列表
        let params_vec: Vec<(String, String)> = params.clone().into_iter().collect();

        // 验签
        if !self.client.verify_notify(&params_vec)? {
            return Err(PaymentError::InvalidSignature);
        }

        // 解析通知参数
        let out_trade_no = params
            .get("out_trade_no")
            .ok_or(PaymentError::MissingParam("out_trade_no"))?
            .clone();
        let trade_no = params
            .get("trade_no")
            .ok_or(PaymentError::MissingParam("trade_no"))?
            .clone();
        let trade_status = params
            .get("trade_status")
            .ok_or(PaymentError::MissingParam("trade_status"))?
            .clone();
        let total_amount: Decimal = params
            .get("total_amount")
            .ok_or(PaymentError::MissingParam("total_amount"))?
            .parse()
            .map_err(|_| PaymentError::InvalidAmount)?;

        // 查询订单
        let order =
            keycompute_db::PaymentOrder::find_by_out_trade_no(self.pool.as_ref(), &out_trade_no)
                .await
                .map_err(|e| PaymentError::DatabaseError(e.to_string()))?
                .ok_or(PaymentError::OrderNotFound)?;

        // 检查订单状态
        if order.status != "pending" {
            // 订单已处理，返回成功避免重复通知
            return Ok(NotifyResult {
                order_id: order.id,
                status: order.status.clone(),
                amount: order.amount,
                trade_no: order.trade_no.unwrap_or_default(),
            });
        }

        // 验证金额一致性（安全检查）
        if (total_amount - order.amount).abs() > Decimal::new(1, 2) {
            // 允许 0.01 元误差
            tracing::error!(
                "Amount mismatch: order={}, notify={}",
                order.amount,
                total_amount
            );
            return Err(PaymentError::AmountMismatch {
                expected: order.amount,
                actual: total_amount,
            });
        }

        // 检查交易状态
        if trade_status == "TRADE_SUCCESS" || trade_status == "TRADE_FINISHED" {
            // 交易成功，继续处理
        } else if trade_status == "TRADE_CLOSED" {
            // 交易关闭，使用 close 方法设置 closed_at
            keycompute_db::PaymentOrder::close(self.pool.as_ref(), order.id)
                .await
                .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

            return Ok(NotifyResult {
                order_id: order.id,
                status: "closed".to_string(),
                amount: order.amount,
                trade_no,
            });
        } else {
            // 其他状态（如 WAIT_BUYER_PAY），不应该出现在回调中
            // 记录警告日志，但不修改订单状态，返回错误让支付宝重试
            tracing::warn!(
                "Unexpected trade_status '{}' in notify for order {}, ignoring",
                trade_status,
                order.id
            );
            return Err(PaymentError::InvalidTradeStatus(trade_status));
        }

        // 开始事务处理支付成功
        let tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

        // 更新订单状态（幂等：只有 pending 状态才能更新）
        let notify_data = serde_json::to_value(&params)
            .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

        let updated_order_stmt = sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            r#"
            UPDATE payment_orders
            SET status = 'paid',
                trade_no = $1,
                notify_data = $2,
                paid_at = NOW(),
                updated_at = NOW()
            WHERE id = $3 AND status = 'pending'
            RETURNING *
            "#,
            [
                trade_no.as_str().into(),
                notify_data.clone().into(),
                order.id.into(),
            ],
        );
        let updated_order = keycompute_db::PaymentOrder::find_by_statement(updated_order_stmt)
            .one(&tx)
            .await
            .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

        // 如果订单已被其他事务处理，直接返回成功（幂等）
        let updated_order = match updated_order {
            Some(o) => o,
            None => {
                // 订单已被处理，回滚事务并返回成功
                tx.rollback()
                    .await
                    .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;
                return Ok(NotifyResult {
                    order_id: order.id,
                    status: "paid".to_string(), // 已被处理
                    amount: order.amount,
                    trade_no,
                });
            }
        };

        // 充值用户余额（在同一事务中，保证原子性）
        let description = format!("支付宝充值 - 订单号: {}", out_trade_no);
        keycompute_db::UserBalance::recharge_in_tx(
            &tx,
            order.user_id,
            order.tenant_id,
            order.amount, // 使用订单金额，而非回调金额
            Some(order.id),
            Some(&description),
        )
        .await
        .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

        // 提交事务（包含订单更新和余额充值，保证原子性）
        tx.commit()
            .await
            .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

        Ok(NotifyResult {
            order_id: updated_order.id,
            status: "paid".to_string(),
            amount: updated_order.amount,
            trade_no,
        })
    }

    /// 主动同步订单状态
    ///
    /// 从支付宝查询订单状态并更新本地订单
    pub async fn sync_order_status(&self, out_trade_no: &str) -> Result<SyncResult, PaymentError> {
        // 查询本地订单
        let order =
            keycompute_db::PaymentOrder::find_by_out_trade_no(self.pool.as_ref(), out_trade_no)
                .await
                .map_err(|e| PaymentError::DatabaseError(e.to_string()))?
                .ok_or(PaymentError::OrderNotFound)?;

        // 如果订单已处理，直接返回
        if order.status != "pending" {
            return Ok(SyncResult {
                order_id: order.id,
                status: order.status.clone(),
                changed: false,
            });
        }

        // 从支付宝查询订单状态
        let query_result = self
            .client
            .query_order(out_trade_no)
            .await
            .map_err(|e| PaymentError::ApiError(e.to_string()))?;

        if !query_result.is_success() {
            return Err(PaymentError::ApiError(
                query_result
                    .sub_msg
                    .unwrap_or_else(|| query_result.msg.clone()),
            ));
        }

        // 检查交易状态
        let trade_status = query_result.trade_status.as_deref().unwrap_or("");

        if trade_status == "TRADE_SUCCESS" || trade_status == "TRADE_FINISHED" {
            // 交易成功，更新订单并充值
            let trade_no = query_result.trade_no.clone().unwrap_or_default();
            let notify_data = serde_json::to_value(&query_result)
                .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

            let tx = self
                .pool
                .begin()
                .await
                .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

            // 更新订单状态（幂等：只有 pending 状态才能更新）
            let updated_order_stmt = sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                r#"
                UPDATE payment_orders
                SET status = 'paid',
                    trade_no = $1,
                    notify_data = $2,
                    paid_at = NOW(),
                    updated_at = NOW()
                WHERE id = $3 AND status = 'pending'
                RETURNING *
                "#,
                [
                    trade_no.as_str().into(),
                    notify_data.clone().into(),
                    order.id.into(),
                ],
            );
            let updated_order = keycompute_db::PaymentOrder::find_by_statement(updated_order_stmt)
                .one(&tx)
                .await
                .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

            // 如果订单已被其他事务处理，直接返回成功（幂等）
            let updated_order = match updated_order {
                Some(o) => o,
                None => {
                    // 订单已被处理，回滚事务并返回
                    tx.rollback()
                        .await
                        .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;
                    return Ok(SyncResult {
                        order_id: order.id,
                        status: "paid".to_string(),
                        changed: false, // 未发生变化
                    });
                }
            };

            // 充值余额（在同一事务中，保证原子性）
            let description = format!("支付宝充值(同步) - 订单号: {}", out_trade_no);
            keycompute_db::UserBalance::recharge_in_tx(
                &tx,
                order.user_id,
                order.tenant_id,
                order.amount,
                Some(order.id),
                Some(&description),
            )
            .await
            .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

            // 提交事务（包含订单更新和余额充值，保证原子性）
            tx.commit()
                .await
                .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

            Ok(SyncResult {
                order_id: updated_order.id,
                status: "paid".to_string(),
                changed: true,
            })
        } else if trade_status == "TRADE_CLOSED" {
            // 交易关闭，使用 close 方法设置 closed_at
            keycompute_db::PaymentOrder::close(self.pool.as_ref(), order.id)
                .await
                .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

            Ok(SyncResult {
                order_id: order.id,
                status: "closed".to_string(),
                changed: true,
            })
        } else {
            // 等待付款或其他状态
            Ok(SyncResult {
                order_id: order.id,
                status: "pending".to_string(),
                changed: false,
            })
        }
    }

    /// 关闭订单
    ///
    /// # 注意
    /// 此方法会先调用支付宝关闭订单，然后更新本地状态。
    /// 如果支付宝关闭成功但本地更新失败，本地状态可能不一致。
    pub async fn close_order(&self, out_trade_no: &str) -> Result<(), PaymentError> {
        // 先查询本地订单
        let order =
            keycompute_db::PaymentOrder::find_by_out_trade_no(self.pool.as_ref(), out_trade_no)
                .await
                .map_err(|e| PaymentError::DatabaseError(e.to_string()))?
                .ok_or(PaymentError::OrderNotFound)?;

        // 检查订单状态，已处理的订单不需要关闭
        if order.status != "pending" {
            return Err(PaymentError::InvalidOrderStatus);
        }

        // 调用支付宝关闭订单接口
        let result = self
            .client
            .close_order(out_trade_no)
            .await
            .map_err(|e| PaymentError::ApiError(e.to_string()))?;

        if !result.is_success() {
            return Err(PaymentError::ApiError(
                result.sub_msg.unwrap_or_else(|| result.msg.clone()),
            ));
        }

        // 更新本地订单状态（使用条件更新，避免并发问题）
        keycompute_db::PaymentOrder::close(self.pool.as_ref(), order.id)
            .await
            .map_err(|e| PaymentError::DatabaseError(e.to_string()))?;

        Ok(())
    }

    /// 获取用户余额
    pub async fn get_user_balance(&self, user_id: Uuid) -> Result<UserBalanceInfo, PaymentError> {
        let balance = keycompute_db::UserBalance::find_by_user(self.pool.as_ref(), user_id)
            .await
            .map_err(|e| PaymentError::DatabaseError(e.to_string()))?
            .unwrap_or_else(|| keycompute_db::UserBalance {
                id: Uuid::nil(),
                tenant_id: Uuid::nil(),
                user_id,
                available_balance: Decimal::ZERO,
                frozen_balance: Decimal::ZERO,
                total_recharged: Decimal::ZERO,
                total_consumed: Decimal::ZERO,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            });

        Ok(UserBalanceInfo {
            user_id: balance.user_id,
            available_balance: balance.available_balance,
            frozen_balance: balance.frozen_balance,
            total_balance: balance.total_balance(),
            total_recharged: balance.total_recharged,
            total_consumed: balance.total_consumed,
        })
    }

    /// 获取支付客户端（用于需要直接调用支付宝API的场景）
    pub fn client(&self) -> &AlipayClient {
        &self.client
    }
}

/// 生成商户订单号
///
/// 格式: KC + 14位时间戳 + 8位UUID随机后缀
/// 冲突概率极低，支持高并发场景
fn generate_out_trade_no() -> String {
    let timestamp = Utc::now().format("%Y%m%d%H%M%S");
    // 使用 UUID 前8位作为随机后缀，提供约 4.3 billion 种组合
    let uuid_suffix = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    format!("KC{}{}", timestamp, uuid_suffix)
}

/// 创建订单请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateOrderRequest {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub amount: Decimal,
    pub subject: String,
    pub body: Option<String>,
}

/// 创建订单结果
#[derive(Debug, Clone, Serialize)]
pub struct CreateOrderResult {
    /// 订单ID
    pub order_id: Uuid,
    /// 商户订单号
    pub out_trade_no: String,
    /// 支付URL
    pub pay_url: String,
    /// 过期时间
    pub expired_at: DateTime<Utc>,
}

/// 回调处理结果
#[derive(Debug, Clone, Serialize)]
pub struct NotifyResult {
    pub order_id: Uuid,
    pub status: String,
    pub amount: Decimal,
    pub trade_no: String,
}

/// 同步订单结果
#[derive(Debug, Clone, Serialize)]
pub struct SyncResult {
    pub order_id: Uuid,
    pub status: String,
    pub changed: bool,
}

/// 用户余额信息
#[derive(Debug, Clone, Serialize)]
pub struct UserBalanceInfo {
    pub user_id: Uuid,
    pub available_balance: Decimal,
    pub frozen_balance: Decimal,
    pub total_balance: Decimal,
    pub total_recharged: Decimal,
    pub total_consumed: Decimal,
}

/// 创建扫码支付订单结果
#[derive(Debug, Clone, Serialize)]
pub struct CreateQrOrderResult {
    /// 订单ID
    pub order_id: Uuid,
    /// 商户订单号
    pub out_trade_no: String,
    /// 支付二维码内容（可用于生成二维码图片）
    pub qr_code: String,
    /// 过期时间
    pub expired_at: DateTime<Utc>,
}

impl CreateQrOrderResult {
    /// 获取二维码图片URL（使用第三方二维码生成服务）
    ///
    /// 示例：返回一个可以展示二维码图片的URL
    pub fn qr_code_image_url(&self) -> String {
        // 使用公共二维码生成API
        format!(
            "https://api.qrserver.com/v1/create-qr-code/?size=300x300&data={}",
            urlencoding::encode(&self.qr_code)
        )
    }
}

/// 支付错误
#[derive(Debug, thiserror::Error)]
pub enum PaymentError {
    #[error("配置错误: {0}")]
    ConfigError(String),
    #[error("API错误: {0}")]
    ApiError(String),
    #[error("数据库错误: {0}")]
    DatabaseError(String),
    #[error("签名验证失败")]
    InvalidSignature,
    #[error("缺少参数: {0}")]
    MissingParam(&'static str),
    #[error("订单不存在")]
    OrderNotFound,
    #[error("金额无效")]
    InvalidAmount,
    #[error("订单状态无效")]
    InvalidOrderStatus,
    #[error("金额不匹配: 订单 {expected}, 回调 {actual}")]
    AmountMismatch { expected: Decimal, actual: Decimal },
    #[error("无效的交易状态: {0}")]
    InvalidTradeStatus(String),
}

impl PaymentError {
    /// 判断错误是否可重试
    ///
    /// 可重试错误：数据库临时故障、网络超时等，支付宝应该重试通知
    /// 不可重试错误：签名错误、订单不存在、金额不匹配等，支付宝不应重试
    pub fn is_retryable(&self) -> bool {
        match self {
            // 数据库错误通常是可重试的（连接池耗尽、临时网络问题等）
            PaymentError::DatabaseError(_) => true,
            // 以下错误不可重试，重试也无法解决
            PaymentError::ConfigError(_) => false,
            PaymentError::ApiError(_) => false,
            PaymentError::InvalidSignature => false,
            PaymentError::MissingParam(_) => false,
            PaymentError::OrderNotFound => false,
            PaymentError::InvalidAmount => false,
            PaymentError::InvalidOrderStatus => false,
            PaymentError::AmountMismatch { .. } => false,
            PaymentError::InvalidTradeStatus(_) => false,
        }
    }
}

impl From<crate::config::ConfigError> for PaymentError {
    fn from(e: crate::config::ConfigError) -> Self {
        PaymentError::ConfigError(e.to_string())
    }
}

impl From<crate::client::ClientError> for PaymentError {
    fn from(e: crate::client::ClientError) -> Self {
        PaymentError::ApiError(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_out_trade_no() {
        let order_no = generate_out_trade_no();
        assert!(order_no.starts_with("KC"));
        assert_eq!(order_no.len(), 24); // KC + 14位时间戳 + 8位UUID后缀

        // 验证生成的订单号唯一性
        let order_no2 = generate_out_trade_no();
        assert_ne!(order_no, order_no2);

        // 验证UUID后缀只包含十六进制字符
        let uuid_suffix = &order_no[16..]; // KC(2) + 时间戳(14) = 16
        assert!(
            uuid_suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "UUID后缀应只包含十六进制字符: {}",
            uuid_suffix
        );
    }
}
