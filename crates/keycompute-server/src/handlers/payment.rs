//! 支付处理器
//!
//! 处理支付相关HTTP请求

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// ==================== 请求/响应结构体 ====================

/// 支付类型
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaymentType {
    /// 跳转支付（PC网页）
    Page,
    /// 跳转支付（手机H5）
    Wap,
    /// 扫码支付（当面付）
    Qr,
}

impl Default for PaymentType {
    fn default() -> Self {
        Self::Qr
    }
}

/// 创建支付订单请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreatePaymentOrderRequest {
    /// 支付金额（元）
    pub amount: Decimal,
    /// 商品标题
    pub subject: String,
    /// 商品描述
    #[serde(default)]
    pub body: Option<String>,
    /// 支付类型
    #[serde(default)]
    pub payment_type: PaymentType,
}

/// 创建支付订单响应
#[derive(Debug, Serialize)]
pub struct CreatePaymentOrderResponse {
    /// 订单ID
    pub order_id: Uuid,
    /// 商户订单号
    pub out_trade_no: String,
    /// 支付类型
    pub payment_type: String,
    /// 支付URL（跳转支付）
    pub pay_url: Option<String>,
    /// 二维码内容（扫码支付）
    pub qr_code: Option<String>,
    /// 二维码图片URL（扫码支付）
    pub qr_code_image_url: Option<String>,
    /// 过期时间
    pub expired_at: String,
}

/// 支付订单列表响应
#[derive(Debug, Serialize)]
pub struct PaymentOrderListResponse {
    pub orders: Vec<PaymentOrderItem>,
    pub total: i64,
}

/// 支付订单项
#[derive(Debug, Serialize)]
pub struct PaymentOrderItem {
    pub id: Uuid,
    pub out_trade_no: String,
    pub amount: String,
    pub status: String,
    pub subject: String,
    pub created_at: String,
    pub expired_at: String,
}

/// 用户余额响应
#[derive(Debug, Serialize)]
pub struct UserBalanceResponse {
    pub user_id: Uuid,
    pub available_balance: String,
    pub frozen_balance: String,
    pub total_balance: String,
    pub total_recharged: String,
    pub total_consumed: String,
}

/// 支付订单查询参数
#[derive(Debug, Deserialize)]
pub struct PaymentOrderQueryParams {
    /// 状态过滤
    pub status: Option<String>,
    /// 页码
    #[serde(default = "default_page")]
    pub page: i64,
    /// 每页数量
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}

fn default_page() -> i64 {
    1
}

fn default_page_size() -> i64 {
    20
}

/// 同步订单状态响应
#[derive(Debug, Serialize)]
pub struct SyncOrderResponse {
    pub order_id: Uuid,
    pub out_trade_no: String,
    pub status: String,
    pub changed: bool,
}

// ==================== Handler函数 ====================

/// 创建支付订单
///
/// POST /api/v1/payments/orders
///
/// 支持三种支付方式：
/// - page: PC网页跳转支付
/// - wap: 手机H5跳转支付
/// - qr: 扫码支付（默认）
pub async fn create_payment_order(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Json(req): Json<CreatePaymentOrderRequest>,
) -> Result<Json<CreatePaymentOrderResponse>> {
    // 验证金额
    if req.amount <= Decimal::ZERO {
        return Err(ApiError::BadRequest("支付金额必须大于0".to_string()));
    }

    // 验证金额上限（单笔最大10万元）
    let max_amount = Decimal::new(100000, 0);
    if req.amount > max_amount {
        return Err(ApiError::BadRequest(
            "单笔支付金额不能超过10万元".to_string(),
        ));
    }

    // 获取数据库连接池
    let _pool = state
        .pool
        .as_ref()
        .ok_or(ApiError::Internal("数据库未配置".to_string()))?;

    // 获取支付服务
    let payment_service = state
        .payment
        .as_ref()
        .ok_or(ApiError::Internal("支付服务未配置".to_string()))?;

    // 构建创建订单请求
    let create_req = keycompute_alipay::CreateOrderRequest {
        tenant_id: auth.tenant_id,
        user_id: auth.user_id,
        amount: req.amount,
        subject: req.subject.clone(),
        body: req.body.clone(),
    };

    // 根据支付类型创建订单
    let result = match req.payment_type {
        PaymentType::Qr => {
            let res = payment_service
                .create_qr_order(create_req)
                .await
                .map_err(|e| ApiError::Internal(format!("创建扫码支付订单失败: {}", e)))?;
            let qr_code_image_url = res.qr_code_image_url();
            CreatePaymentOrderResponse {
                order_id: res.order_id,
                out_trade_no: res.out_trade_no,
                payment_type: "qr".to_string(),
                pay_url: None,
                qr_code: Some(res.qr_code),
                qr_code_image_url: Some(qr_code_image_url),
                expired_at: res.expired_at.to_rfc3339(),
            }
        }
        PaymentType::Page => {
            let res = payment_service
                .create_order(create_req)
                .await
                .map_err(|e| ApiError::Internal(format!("创建网页支付订单失败: {}", e)))?;
            CreatePaymentOrderResponse {
                order_id: res.order_id,
                out_trade_no: res.out_trade_no,
                payment_type: "page".to_string(),
                pay_url: Some(res.pay_url),
                qr_code: None,
                qr_code_image_url: None,
                expired_at: res.expired_at.to_rfc3339(),
            }
        }
        PaymentType::Wap => {
            let res = payment_service
                .create_wap_order(create_req)
                .await
                .map_err(|e| ApiError::Internal(format!("创建手机支付订单失败: {}", e)))?;
            CreatePaymentOrderResponse {
                order_id: res.order_id,
                out_trade_no: res.out_trade_no,
                payment_type: "wap".to_string(),
                pay_url: Some(res.pay_url),
                qr_code: None,
                qr_code_image_url: None,
                expired_at: res.expired_at.to_rfc3339(),
            }
        }
    };

    Ok(Json(result))
}

/// 获取我的支付订单列表
///
/// GET /api/v1/payments/orders
pub async fn list_my_payment_orders(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Query(params): Query<PaymentOrderQueryParams>,
) -> Result<Json<PaymentOrderListResponse>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or(ApiError::Internal("数据库未配置".to_string()))?;

    let offset = (params.page - 1) * params.page_size;

    // 查询订单列表
    let orders =
        keycompute_db::PaymentOrder::find_by_user(pool, auth.user_id, params.page_size, offset)
            .await
            .map_err(|e| ApiError::Internal(format!("查询订单失败: {}", e)))?;

    // 统计总数
    let stats = keycompute_db::PaymentOrder::get_user_stats(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("查询订单统计失败: {}", e)))?;

    let items: Vec<PaymentOrderItem> = orders
        .into_iter()
        .filter(|o| {
            if let Some(ref status) = params.status {
                &o.status == status
            } else {
                true
            }
        })
        .map(|o| PaymentOrderItem {
            id: o.id,
            out_trade_no: o.out_trade_no,
            amount: o.amount.to_string(),
            status: o.status,
            subject: o.subject,
            created_at: o.created_at.to_rfc3339(),
            expired_at: o.expired_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(PaymentOrderListResponse {
        total: stats.total_orders,
        orders: items,
    }))
}

/// 获取支付订单详情
///
/// GET /api/v1/payments/orders/{id}
pub async fn get_payment_order(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Path(order_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or(ApiError::Internal("数据库未配置".to_string()))?;

    let order = keycompute_db::PaymentOrder::find_by_id(pool, order_id)
        .await
        .map_err(|e| ApiError::Internal(format!("查询订单失败: {}", e)))?
        .ok_or(ApiError::NotFound("订单不存在".to_string()))?;

    // 验证权限
    if order.user_id != auth.user_id && auth.role != "admin" {
        return Err(ApiError::Forbidden("无权访问此订单".to_string()));
    }

    Ok(Json(serde_json::json!({
        "id": order.id,
        "out_trade_no": order.out_trade_no,
        "trade_no": order.trade_no,
        "amount": order.amount.to_string(),
        "status": order.status,
        "subject": order.subject,
        "body": order.body,
        "payment_method": order.payment_method,
        "pay_url": order.pay_url,
        "expired_at": order.expired_at.to_rfc3339(),
        "paid_at": order.paid_at.map(|t| t.to_rfc3339()),
        "created_at": order.created_at.to_rfc3339(),
    })))
}

/// 获取我的余额
///
/// GET /api/v1/payments/balance
pub async fn get_my_balance(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<UserBalanceResponse>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or(ApiError::Internal("数据库未配置".to_string()))?;

    let balance = keycompute_db::UserBalance::get_or_create(pool, auth.tenant_id, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("获取余额失败: {}", e)))?;

    Ok(Json(UserBalanceResponse {
        user_id: balance.user_id,
        available_balance: balance.available_balance.to_string(),
        frozen_balance: balance.frozen_balance.to_string(),
        total_balance: balance.total_balance().to_string(),
        total_recharged: balance.total_recharged.to_string(),
        total_consumed: balance.total_consumed.to_string(),
    }))
}

/// 同步订单状态
///
/// POST /api/v1/payments/sync/{out_trade_no}
pub async fn sync_payment_order(
    _auth: AuthExtractor,
    State(state): State<AppState>,
    Path(out_trade_no): Path<String>,
) -> Result<Json<SyncOrderResponse>> {
    let payment_service = state
        .payment
        .as_ref()
        .ok_or(ApiError::Internal("支付服务未配置".to_string()))?;

    // 同步订单状态
    let result = payment_service
        .sync_order_status(&out_trade_no)
        .await
        .map_err(|e| ApiError::Internal(format!("同步订单状态失败: {}", e)))?;

    Ok(Json(SyncOrderResponse {
        order_id: result.order_id,
        out_trade_no: out_trade_no,
        status: result.status,
        changed: result.changed,
    }))
}

/// 支付宝异步通知
///
/// POST /api/v1/payments/notify/alipay
///
/// 注意：此接口不需要认证，由支付宝服务器调用
pub async fn alipay_notify(
    State(state): State<AppState>,
    // 支付宝通知使用 form-data 格式
    form: String,
) -> Result<String> {
    let payment_service = state
        .payment
        .as_ref()
        .ok_or(ApiError::Internal("支付服务未配置".to_string()))?;

    // 解析 form 数据
    let params: HashMap<String, String> = form
        .split('&')
        .filter_map(|s| {
            let mut parts = s.splitn(2, '=');
            let key = parts.next()?;
            let value = parts.next()?;
            Some((urlencoding_decode(key), urlencoding_decode(value)))
        })
        .collect();

    // 处理通知
    match payment_service.handle_notify(params).await {
        Ok(_) => Ok("success".to_string()),
        Err(e) => {
            tracing::error!("处理支付宝通知失败: {}", e);
            Ok("fail".to_string())
        }
    }
}

/// URL解码辅助函数
fn urlencoding_decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .to_string()
}

// ==================== 管理员接口 ====================

/// 管理员获取所有支付订单
///
/// GET /api/v1/admin/payments/orders
pub async fn admin_list_payment_orders(
    _auth: AuthExtractor,
    State(state): State<AppState>,
    Query(params): Query<PaymentOrderQueryParams>,
) -> Result<Json<serde_json::Value>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or(ApiError::Internal("数据库未配置".to_string()))?;

    // 管理员可以查看所有订单
    let offset = (params.page - 1) * params.page_size;

    let orders = sqlx::query_as::<_, keycompute_db::PaymentOrder>(
        r#"
        SELECT * FROM payment_orders
        WHERE ($1::text IS NULL OR status = $1)
        ORDER BY created_at DESC
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(&params.status)
    .bind(params.page_size)
    .bind(offset)
    .fetch_all(pool.as_ref())
    .await
    .map_err(|e| ApiError::Internal(format!("查询订单失败: {}", e)))?;

    Ok(Json(serde_json::json!({
        "orders": orders.iter().map(|o| serde_json::json!({
            "id": o.id,
            "tenant_id": o.tenant_id,
            "user_id": o.user_id,
            "out_trade_no": o.out_trade_no,
            "trade_no": o.trade_no,
            "amount": o.amount.to_string(),
            "status": o.status,
            "subject": o.subject,
            "created_at": o.created_at.to_rfc3339(),
        })).collect::<Vec<_>>(),
        "page": params.page,
        "page_size": params.page_size,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_payment_order_request() {
        let req = CreatePaymentOrderRequest {
            amount: Decimal::new(100, 0),
            subject: "测试充值".to_string(),
            body: Some("测试".to_string()),
            payment_type: PaymentType::Qr,
        };

        assert_eq!(req.amount, Decimal::new(100, 0));
    }
}
