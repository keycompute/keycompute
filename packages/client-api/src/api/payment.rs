//! 支付模块
//!
//! 处理支付订单创建、查询和余额获取

use crate::client::ApiClient;
use crate::error::Result;
use serde::{Deserialize, Serialize};

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
    ) -> Result<PaymentOrderResponse> {
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
        self.client.get_json(&path, Some(token)).await
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
    ) -> Result<PaymentOrderResponse> {
        self.client
            .post_json(
                &format!("/api/v1/payments/sync/{}", out_trade_no),
                &serde_json::json!({}),
                Some(token),
            )
            .await
    }

    /// 获取我的余额
    pub async fn get_my_balance(&self, token: &str) -> Result<BalanceResponse> {
        self.client
            .get_json("/api/v1/payments/balance", Some(token))
            .await
    }
}

/// 创建支付订单请求
#[derive(Debug, Clone, Serialize)]
pub struct CreatePaymentOrderRequest {
    pub amount: f64,
    pub currency: String,
    pub description: Option<String>,
    pub payment_method: String,
}

impl CreatePaymentOrderRequest {
    pub fn new(
        amount: f64,
        currency: impl Into<String>,
        payment_method: impl Into<String>,
    ) -> Self {
        Self {
            amount,
            currency: currency.into(),
            description: None,
            payment_method: payment_method.into(),
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
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
            params.push(format!("status={}", status));
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

/// 支付订单响应
#[derive(Debug, Clone, Deserialize)]
pub struct PaymentOrderResponse {
    pub id: String,
    pub out_trade_no: String,
    pub amount: f64,
    pub currency: String,
    pub status: String,
    pub description: Option<String>,
    pub payment_method: String,
    pub pay_url: Option<String>,
    pub paid_at: Option<String>,
    pub created_at: String,
}

/// 支付订单摘要
#[derive(Debug, Clone, Deserialize)]
pub struct PaymentOrderSummary {
    pub id: String,
    pub out_trade_no: String,
    pub amount: f64,
    pub currency: String,
    pub status: String,
    pub created_at: String,
}

/// 余额响应
#[derive(Debug, Clone, Deserialize)]
pub struct BalanceResponse {
    pub balance: f64,
    pub currency: String,
    pub frozen_balance: f64,
}
