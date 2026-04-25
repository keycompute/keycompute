//! 管理功能模块
//!
//! 拆分为子模块：user / account / pricing / payment

mod account;
mod payment;
mod pricing;
mod user;

use crate::client::ApiClient;
use crate::error::Result;
use serde::Deserialize;

pub use super::common::MessageResponse;

// Re-export 各子模块的公共类型
pub use account::{
    AccountInfo, AccountQueryParams, AccountTestResponse, CreateAccountRequest,
    UpdateAccountRequest,
};
pub use payment::PaymentOrderInfo;
pub use pricing::{
    CalculateCostRequest, CostCalculationResponse, CreatePricingRequest, CreatePricingResponse,
    PricingInfo, SetDefaultPricingRequest, UpdatePricingRequest, UpdatePricingResponse,
};
pub use user::{
    ApiKeyInfo, UpdateBalanceRequest, UpdateBalanceResponse, UpdateUserRequest, UpdateUserResponse,
    UserDetail, UserListResponse, UserQueryParams,
};

// Re-export admin payment's PaymentQueryParams distinctly
pub use payment::PaymentQueryParams;

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
    ) -> Result<UserListResponse> {
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
    ) -> Result<UpdateUserResponse> {
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
    ) -> Result<UpdateBalanceResponse> {
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
    ) -> Result<CreatePricingResponse> {
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
    ) -> Result<UpdatePricingResponse> {
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
        // 后端返回 { orders: Vec<PaymentOrderInfo>, page: u32, page_size: u32 }
        #[derive(Deserialize)]
        struct AdminPaymentOrderListResponse {
            orders: Vec<PaymentOrderInfo>,
            #[allow(dead_code)]
            page: u32,
            #[allow(dead_code)]
            page_size: u32,
        }
        let resp: AdminPaymentOrderListResponse = self.client.get_json(&path, Some(token)).await?;
        Ok(resp.orders)
    }
}
