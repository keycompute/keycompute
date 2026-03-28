//! 管理功能模块
//!
//! 处理用户管理、账号管理、定价管理、支付管理等 Admin 功能

use crate::client::ApiClient;
use crate::error::Result;
use serde::{Deserialize, Serialize};

/// 管理 API 客户端
#[derive(Debug, Clone)]
pub struct AdminApi {
    client: ApiClient,
}

impl AdminApi {
    /// 创建新的管理 API 客户端
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    // ==================== 用户管理 ====================

    /// 获取所有用户列表
    pub async fn list_all_users(
        &self,
        params: Option<&UserQueryParams>,
        token: &str,
    ) -> Result<Vec<UserDetail>> {
        let path = if let Some(p) = params {
            format!("/api/v1/users?{}", p.to_query_string())
        } else {
            "/api/v1/users".to_string()
        };
        self.client.get_json(&path, Some(token)).await
    }

    /// 获取指定用户详情
    pub async fn get_user_by_id(&self, id: &str, token: &str) -> Result<UserDetail> {
        self.client
            .get_json(&format!("/api/v1/users/{}", id), Some(token))
            .await
    }

    /// 更新用户信息
    pub async fn update_user(
        &self,
        id: &str,
        req: &UpdateUserRequest,
        token: &str,
    ) -> Result<UserDetail> {
        self.client
            .put_json(&format!("/api/v1/users/{}", id), req, Some(token))
            .await
    }

    /// 删除用户
    pub async fn delete_user(&self, id: &str, token: &str) -> Result<MessageResponse> {
        self.client
            .delete_json(&format!("/api/v1/users/{}", id), Some(token))
            .await
    }

    /// 更新用户余额
    pub async fn update_user_balance(
        &self,
        id: &str,
        req: &UpdateBalanceRequest,
        token: &str,
    ) -> Result<BalanceResponse> {
        self.client
            .post_json(&format!("/api/v1/users/{}/balance", id), req, Some(token))
            .await
    }

    /// 获取用户的 API Keys
    pub async fn list_user_api_keys(&self, id: &str, token: &str) -> Result<Vec<ApiKeyInfo>> {
        self.client
            .get_json(&format!("/api/v1/users/{}/api-keys", id), Some(token))
            .await
    }

    // ==================== 账号/渠道管理 ====================

    /// 获取账号列表
    pub async fn list_accounts(
        &self,
        params: Option<&AccountQueryParams>,
        token: &str,
    ) -> Result<Vec<AccountInfo>> {
        let path = if let Some(p) = params {
            format!("/api/v1/accounts?{}", p.to_query_string())
        } else {
            "/api/v1/accounts".to_string()
        };
        self.client.get_json(&path, Some(token)).await
    }

    /// 创建账号
    pub async fn create_account(
        &self,
        req: &CreateAccountRequest,
        token: &str,
    ) -> Result<AccountInfo> {
        self.client
            .post_json("/api/v1/accounts", req, Some(token))
            .await
    }

    /// 更新账号
    pub async fn update_account(
        &self,
        id: &str,
        req: &UpdateAccountRequest,
        token: &str,
    ) -> Result<AccountInfo> {
        self.client
            .put_json(&format!("/api/v1/accounts/{}", id), req, Some(token))
            .await
    }

    /// 删除账号
    pub async fn delete_account(&self, id: &str, token: &str) -> Result<MessageResponse> {
        self.client
            .delete_json(&format!("/api/v1/accounts/{}", id), Some(token))
            .await
    }

    /// 测试账号
    pub async fn test_account(&self, id: &str, token: &str) -> Result<AccountTestResponse> {
        self.client
            .post_json(
                &format!("/api/v1/accounts/{}/test", id),
                &serde_json::json!({}),
                Some(token),
            )
            .await
    }

    /// 刷新账号
    pub async fn refresh_account(&self, id: &str, token: &str) -> Result<AccountInfo> {
        self.client
            .post_json(
                &format!("/api/v1/accounts/{}/refresh", id),
                &serde_json::json!({}),
                Some(token),
            )
            .await
    }

    // ==================== 定价管理 ====================

    /// 获取定价列表
    pub async fn list_pricing(&self, token: &str) -> Result<Vec<PricingInfo>> {
        self.client.get_json("/api/v1/pricing", Some(token)).await
    }

    /// 创建定价
    pub async fn create_pricing(
        &self,
        req: &CreatePricingRequest,
        token: &str,
    ) -> Result<PricingInfo> {
        self.client
            .post_json("/api/v1/pricing", req, Some(token))
            .await
    }

    /// 更新定价
    pub async fn update_pricing(
        &self,
        id: &str,
        req: &UpdatePricingRequest,
        token: &str,
    ) -> Result<PricingInfo> {
        self.client
            .put_json(&format!("/api/v1/pricing/{}", id), req, Some(token))
            .await
    }

    /// 删除定价
    pub async fn delete_pricing(&self, id: &str, token: &str) -> Result<MessageResponse> {
        self.client
            .delete_json(&format!("/api/v1/pricing/{}", id), Some(token))
            .await
    }

    /// 设置默认定价
    pub async fn set_default_pricing(
        &self,
        req: &SetDefaultPricingRequest,
        token: &str,
    ) -> Result<MessageResponse> {
        self.client
            .post_json("/api/v1/pricing/batch-defaults", req, Some(token))
            .await
    }

    /// 计算费用
    pub async fn calculate_cost(
        &self,
        req: &CalculateCostRequest,
        token: &str,
    ) -> Result<CostCalculationResponse> {
        self.client
            .post_json("/api/v1/pricing/calculate", req, Some(token))
            .await
    }

    // ==================== 支付管理 ====================

    /// 获取所有支付订单（Admin）
    pub async fn list_all_payment_orders(
        &self,
        params: Option<&PaymentQueryParams>,
        token: &str,
    ) -> Result<Vec<PaymentOrderInfo>> {
        let path = if let Some(p) = params {
            format!("/api/v1/admin/payments/orders?{}", p.to_query_string())
        } else {
            "/api/v1/admin/payments/orders".to_string()
        };
        self.client.get_json(&path, Some(token)).await
    }
}

// ==================== 请求/响应类型 ====================

/// 用户查询参数
#[derive(Debug, Clone, Serialize, Default)]
pub struct UserQueryParams {
    pub limit: Option<i32>,
    pub offset: Option<i32>,
    pub role: Option<String>,
}

impl UserQueryParams {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_limit(mut self, limit: i32) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn with_offset(mut self, offset: i32) -> Self {
        self.offset = Some(offset);
        self
    }

    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }

    pub fn to_query_string(&self) -> String {
        let mut params = Vec::new();
        if let Some(limit) = self.limit {
            params.push(format!("limit={}", limit));
        }
        if let Some(offset) = self.offset {
            params.push(format!("offset={}", offset));
        }
        if let Some(ref role) = self.role {
            params.push(format!("role={}", role));
        }
        params.join("&")
    }
}

/// 用户详情
#[derive(Debug, Clone, Deserialize)]
pub struct UserDetail {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
    pub tenant_id: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 更新用户请求
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateUserRequest {
    pub name: Option<String>,
    pub role: Option<String>,
}

impl UpdateUserRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }
}

/// 更新余额请求
#[derive(Debug, Clone, Serialize)]
pub struct UpdateBalanceRequest {
    pub amount: f64,
    pub operation: String,
    pub reason: Option<String>,
}

impl UpdateBalanceRequest {
    pub fn add(amount: f64) -> Self {
        Self {
            amount,
            operation: "add".to_string(),
            reason: None,
        }
    }

    pub fn subtract(amount: f64) -> Self {
        Self {
            amount,
            operation: "subtract".to_string(),
            reason: None,
        }
    }

    pub fn set(amount: f64) -> Self {
        Self {
            amount,
            operation: "set".to_string(),
            reason: None,
        }
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// 余额响应
#[derive(Debug, Clone, Deserialize)]
pub struct BalanceResponse {
    pub user_id: String,
    pub balance: f64,
    pub currency: String,
}

/// API Key 信息
#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeyInfo {
    pub id: String,
    pub name: String,
    pub key_preview: String,
    pub revoked: bool,
    pub created_at: String,
}

/// 账号查询参数
#[derive(Debug, Clone, Serialize, Default)]
pub struct AccountQueryParams {
    pub provider: Option<String>,
    pub status: Option<String>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
}

impl AccountQueryParams {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
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
        if let Some(ref provider) = self.provider {
            params.push(format!("provider={}", provider));
        }
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

/// 账号信息
#[derive(Debug, Clone, Deserialize)]
pub struct AccountInfo {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub status: String,
    pub is_active: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 创建账号请求
#[derive(Debug, Clone, Serialize)]
pub struct CreateAccountRequest {
    pub name: String,
    pub provider: String,
    pub api_key: String,
    pub api_base: Option<String>,
}

impl CreateAccountRequest {
    pub fn new(
        name: impl Into<String>,
        provider: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            provider: provider.into(),
            api_key: api_key.into(),
            api_base: None,
        }
    }

    pub fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = Some(api_base.into());
        self
    }
}

/// 更新账号请求
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateAccountRequest {
    pub name: Option<String>,
    pub api_key: Option<String>,
    pub api_base: Option<String>,
    pub is_active: Option<bool>,
}

impl UpdateAccountRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn with_is_active(mut self, is_active: bool) -> Self {
        self.is_active = Some(is_active);
        self
    }
}

/// 账号测试响应
#[derive(Debug, Clone, Deserialize)]
pub struct AccountTestResponse {
    pub success: bool,
    pub message: String,
    pub latency_ms: Option<i64>,
}

/// 定价信息
#[derive(Debug, Clone, Deserialize)]
pub struct PricingInfo {
    pub id: String,
    pub model: String,
    pub input_price: f64,
    pub output_price: f64,
    pub currency: String,
    pub is_default: bool,
    pub created_at: String,
}

/// 创建定价请求
#[derive(Debug, Clone, Serialize)]
pub struct CreatePricingRequest {
    pub model: String,
    pub input_price: f64,
    pub output_price: f64,
    pub currency: String,
}

impl CreatePricingRequest {
    pub fn new(
        model: impl Into<String>,
        input_price: f64,
        output_price: f64,
        currency: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            input_price,
            output_price,
            currency: currency.into(),
        }
    }
}

/// 更新定价请求
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdatePricingRequest {
    pub input_price: Option<f64>,
    pub output_price: Option<f64>,
    pub currency: Option<String>,
}

impl UpdatePricingRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_input_price(mut self, price: f64) -> Self {
        self.input_price = Some(price);
        self
    }

    pub fn with_output_price(mut self, price: f64) -> Self {
        self.output_price = Some(price);
        self
    }
}

/// 设置默认定价请求
#[derive(Debug, Clone, Serialize)]
pub struct SetDefaultPricingRequest {
    pub model_ids: Vec<String>,
}

/// 计算费用请求
#[derive(Debug, Clone, Serialize)]
pub struct CalculateCostRequest {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// 费用计算响应
#[derive(Debug, Clone, Deserialize)]
pub struct CostCalculationResponse {
    pub model: String,
    pub input_cost: f64,
    pub output_cost: f64,
    pub total_cost: f64,
    pub currency: String,
}

/// 支付订单查询参数
#[derive(Debug, Clone, Serialize, Default)]
pub struct PaymentQueryParams {
    pub status: Option<String>,
    pub user_id: Option<String>,
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

    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
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
        if let Some(ref user_id) = self.user_id {
            params.push(format!("user_id={}", user_id));
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

/// 支付订单信息
#[derive(Debug, Clone, Deserialize)]
pub struct PaymentOrderInfo {
    pub id: String,
    pub user_id: String,
    pub out_trade_no: String,
    pub amount: f64,
    pub currency: String,
    pub status: String,
    pub created_at: String,
}

/// 通用消息响应
#[derive(Debug, Clone, Deserialize)]
pub struct MessageResponse {
    pub message: String,
}
