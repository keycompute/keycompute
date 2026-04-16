//! 支付模块
//!
//! 处理支付订单创建、查询和余额获取

use crate::client::ApiClient;
use crate::error::Result;
use serde::{Deserialize, Serialize};

use super::common::encode_query_value;

/// 支付 API 客户端
#[derive(Debug, Clone)]
pub struct PaymentApi {
    client: ApiClient,
}

impl PaymentApi {
    /// 创建新的支付 API 客户端
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    /// 创建支付订单
    pub async fn create_payment_order(
        &self,
        req: &CreatePaymentOrderRequest,
        token: &str,
    ) -> Result<CreatePaymentOrderResponse> {
        self.client
            .post_json("/api/v1/payments/orders", req, Some(token))
            .await
    }

    /// 获取我的支付订单列表
    pub async fn list_my_payment_orders(
        &self,
        params: Option<&PaymentQueryParams>,
        token: &str,
    ) -> Result<Vec<PaymentOrderSummary>> {
        let path = if let Some(p) = params {
            format!("/api/v1/payments/orders?{}", p.to_query_string())
        } else {
            "/api/v1/payments/orders".to_string()
        };
        // 后端返回 { orders: Vec<PaymentOrderItem>, total: i64 }
        #[derive(Deserialize)]
        struct PaymentOrderListResponse {
            orders: Vec<PaymentOrderSummary>,
            #[allow(dead_code)]
            total: i64,
        }
        let resp: PaymentOrderListResponse = self.client.get_json(&path, Some(token)).await?;
        Ok(resp.orders)
    }

    /// 获取订单详情
    pub async fn get_payment_order(&self, id: &str, token: &str) -> Result<PaymentOrderResponse> {
        self.client
            .get_json(&format!("/api/v1/payments/orders/{}", id), Some(token))
            .await
    }

    /// 同步订单状态
    pub async fn sync_payment_order(
        &self,
        out_trade_no: &str,
        token: &str,
    ) -> Result<SyncPaymentOrderResponse> {
        self.client
            .post_json(
                &format!("/api/v1/payments/sync/{}", out_trade_no),
                &serde_json::json!({}),
                Some(token),
            )
            .await
    }

    /// 获取我的余额
    pub async fn get_my_balance(&self, token: &str) -> Result<UserBalanceResponse> {
        self.client
            .get_json("/api/v1/payments/balance", Some(token))
            .await
    }
}

/// 创建支付订单请求
#[derive(Debug, Clone, Serialize)]
pub struct CreatePaymentOrderRequest {
    pub amount: String,
    pub subject: String,
    pub body: Option<String>,
    pub payment_type: String,
}

impl CreatePaymentOrderRequest {
    pub fn new(amount: f64, subject: impl Into<String>, payment_type: impl Into<String>) -> Self {
        Self {
            amount: format_amount(amount),
            subject: subject.into(),
            body: None,
            payment_type: payment_type.into(),
        }
    }

    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }
}

/// 支付订单查询参数
#[derive(Debug, Clone, Serialize, Default)]
pub struct PaymentQueryParams {
    pub status: Option<String>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
}

impl PaymentQueryParams {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    pub fn with_limit(mut self, limit: i32) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn with_offset(mut self, offset: i32) -> Self {
        self.offset = Some(offset);
        self
    }

    pub fn to_query_string(&self) -> String {
        let mut params = Vec::new();
        if let Some(ref status) = self.status {
            params.push(format!("status={}", encode_query_value(status)));
        }
        if let Some(limit) = self.limit {
            params.push(format!("limit={}", limit));
        }
        if let Some(offset) = self.offset {
            params.push(format!("offset={}", offset));
        }
        params.join("&")
    }
}

fn format_amount(amount: f64) -> String {
    format!("{:.2}", amount)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

/// 创建支付订单响应
#[derive(Debug, Clone, Deserialize)]
pub struct CreatePaymentOrderResponse {
    pub order_id: String,
    pub out_trade_no: String,
    pub payment_type: String,
    pub pay_url: Option<String>,
    pub qr_code: Option<String>,
    pub qr_code_image_url: Option<String>,
    pub expired_at: String,
}

/// 支付订单响应
#[derive(Debug, Clone, Deserialize)]
pub struct PaymentOrderResponse {
    pub id: String,
    pub out_trade_no: String,
    pub amount: String,
    pub status: String,
    pub subject: String,
    pub body: Option<String>,
    pub payment_method: String,
    pub pay_url: Option<String>,
    pub expired_at: String,
    pub paid_at: Option<String>,
    pub created_at: String,
}

/// 同步订单状态响应
#[derive(Debug, Clone, Deserialize)]
pub struct SyncPaymentOrderResponse {
    pub order_id: String,
    pub out_trade_no: String,
    pub status: String,
    pub changed: bool,
}

/// 支付订单摘要
#[derive(Debug, Clone, Deserialize)]
pub struct PaymentOrderSummary {
    pub id: String,
    pub out_trade_no: String,
    pub amount: String,
    pub status: String,
    pub subject: String,
    pub created_at: String,
    pub expired_at: String,
}

/// 用户余额响应（用户查询自己余额时返回）
#[derive(Debug, Clone, Deserialize)]
pub struct UserBalanceResponse {
    pub user_id: String,
    pub available_balance: String,
    pub frozen_balance: String,
    pub total_balance: String,
    pub total_recharged: String,
    pub total_consumed: String,
}
