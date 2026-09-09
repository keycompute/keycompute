//! 管理功能模块
//!
//! 拆分为子模块：user / account / pricing / payment

mod account;
mod monitoring;
mod node_gateway;
mod payment;
mod pricing;
mod user;

pub use super::common::MessageResponse;
use crate::client::ApiClient;
use crate::error::Result;

const COMPAT_LIST_PAGE_SIZE: u64 = 100;

// Re-export 各子模块的公共类型
pub use account::{
    AccountInfo, AccountPage, AccountQueryParams, AccountRefreshResponse, AccountTestResponse,
    CreateAccountRequest, UpdateAccountRequest,
};
pub use monitoring::{
    MonitoringAttemptDetail, MonitoringNodeHealth, MonitoringOverviewResponse,
    MonitoringProbeRequest, MonitoringQuery, MonitoringRequestDetail, MonitoringRequestItem,
    MonitoringRequestPage, MonitoringSummary, MonitoringSummaryResponse,
    MonitoringTargetHealthResponse, MonitoringTraceEntry, MonitoringTraceSummary,
};
pub use node_gateway::{
    ApproveTokenRequest, DeleteNodeResponse, ExcludeNodeResponse, NodeGatewayListQueryParams,
    NodeGatewayNodeInfo, NodeGatewayNodePage, NodeGatewayNodeStats, NodeGatewayOverviewResponse,
    NodeGatewayTaskInfo, NodeGatewayTaskPage, NodeGatewayTaskStats, PendingTokenPage,
    PendingTokenQueryParams, PendingTokenWithUser, RecoverNodeResponse, RevokeNodeRequest,
    RevokeNodeTokenResponse,
};
pub use payment::{PaymentOrderInfo, PaymentOrderPage, PaymentProviderStatus};
pub use pricing::{
    CalculateCostRequest, CostCalculationResponse, CreatePricingRequest, CreatePricingResponse,
    MakeDefaultPricingResponse, PricingInfo, PricingPage, PricingQueryParams,
    SetDefaultPricingRequest, UpdatePricingRequest, UpdatePricingResponse,
};
pub use user::{
    ApiKeyInfo, BalanceReservationInfo, ReleaseBalanceReservationRequest,
    ReleaseBalanceReservationResponse, UpdateBalanceRequest, UpdateBalanceResponse,
    UpdateUserRequest, UpdateUserResponse, UserBalanceReservationsResponse, UserDetail,
    UserListResponse, UserQueryParams,
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

    /// 更新用户余额（充值/扣除）。`idempotency_key` 必填；同一逻辑操作的
    /// 超时或网络重试必须复用同一个 key。
    pub async fn update_user_balance(
        &self,
        id: &str,
        req: &UpdateBalanceRequest,
        idempotency_key: &str,
        token: &str,
    ) -> Result<UpdateBalanceResponse> {
        self.client
            .post_json_with_idempotency_key(
                &format!("/api/v1/users/{}/balance", id),
                req,
                idempotency_key,
                Some(token),
            )
            .await
    }

    /// 冻结用户余额。`idempotency_key` 必填；同一逻辑操作的超时或网络
    /// 重试必须复用同一个 key。
    pub async fn freeze_user_balance(
        &self,
        id: &str,
        req: &UpdateBalanceRequest,
        idempotency_key: &str,
        token: &str,
    ) -> Result<UpdateBalanceResponse> {
        self.client
            .post_json_with_idempotency_key(
                &format!("/api/v1/users/{}/balance/freeze", id),
                req,
                idempotency_key,
                Some(token),
            )
            .await
    }

    /// 解冻用户余额。`idempotency_key` 必填；同一逻辑操作的超时或网络
    /// 重试必须复用同一个 key。
    pub async fn unfreeze_user_balance(
        &self,
        id: &str,
        req: &UpdateBalanceRequest,
        idempotency_key: &str,
        token: &str,
    ) -> Result<UpdateBalanceResponse> {
        self.client
            .post_json_with_idempotency_key(
                &format!("/api/v1/users/{}/balance/unfreeze", id),
                req,
                idempotency_key,
                Some(token),
            )
            .await
    }

    /// 获取用户余额拆分及活跃请求预留的第一页。需要遍历后续页面时，
    /// 使用响应的 `next_cursor` 调用 `list_user_balance_reservations_page`。
    pub async fn list_user_balance_reservations(
        &self,
        id: &str,
        token: &str,
    ) -> Result<UserBalanceReservationsResponse> {
        self.list_user_balance_reservations_page(id, None, None, token)
            .await
    }

    /// 获取一页用户余额预留。`cursor` 必须原样使用前一页返回的
    /// `next_cursor`；`limit` 在服务端约束到 1..=100。
    pub async fn list_user_balance_reservations_page(
        &self,
        id: &str,
        cursor: Option<&str>,
        limit: Option<u64>,
        token: &str,
    ) -> Result<UserBalanceReservationsResponse> {
        let mut path = format!("/api/v1/users/{}/balance/reservations", id);
        let mut query = Vec::new();
        if let Some(cursor) = cursor {
            query.push(format!("cursor={}", urlencoding::encode(cursor)));
        }
        if let Some(limit) = limit {
            query.push(format!("limit={limit}"));
        }
        if !query.is_empty() {
            path.push('?');
            path.push_str(&query.join("&"));
        }
        self.client.get_json(&path, Some(token)).await
    }

    /// 按 request_id 和列表返回的预留版本安全释放一笔卡死的请求余额预留。
    /// 自动或手工重试必须复用相同版本和 reason；完全相同的重试会安全重放。
    pub async fn release_user_balance_reservation(
        &self,
        id: &str,
        request_id: &str,
        req: &ReleaseBalanceReservationRequest,
        token: &str,
    ) -> Result<ReleaseBalanceReservationResponse> {
        self.client
            .post_json(
                &format!(
                    "/api/v1/users/{}/balance/reservations/{}/release",
                    id, request_id
                ),
                req,
                Some(token),
            )
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
        if params.is_some_and(AccountQueryParams::has_explicit_pagination) {
            return Ok(self.list_accounts_page(params, token).await?.accounts);
        }

        let mut params = params.cloned().unwrap_or_default();
        params.page_size = Some(COMPAT_LIST_PAGE_SIZE as u32);
        params.limit = None;
        params.offset = None;

        let mut accounts = Vec::new();
        let mut page = 1u32;
        loop {
            params.page = Some(page);
            let response = self.list_accounts_page(Some(&params), token).await?;
            accounts.extend(response.accounts);
            if response.total_pages == 0 || page >= response.total_pages {
                break;
            }
            page += 1;
        }
        Ok(accounts)
    }

    pub async fn list_accounts_page(
        &self,
        params: Option<&AccountQueryParams>,
        token: &str,
    ) -> Result<AccountPage> {
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
    pub async fn refresh_account(&self, id: &str, token: &str) -> Result<AccountRefreshResponse> {
        self.client
            .post_json(
                &format!("/api/v1/accounts/{}/refresh", id),
                &serde_json::json!({}),
                Some(token),
            )
            .await
    }

    // ==================== Node Gateway 管理 ====================

    pub async fn node_gateway_overview(&self, token: &str) -> Result<NodeGatewayOverviewResponse> {
        self.client
            .get_json("/api/v1/admin/node-gateway/overview", Some(token))
            .await
    }

    pub async fn list_node_gateway_nodes(
        &self,
        params: &NodeGatewayListQueryParams,
        token: &str,
    ) -> Result<NodeGatewayNodePage> {
        let query = params.to_query_string();
        self.client
            .get_json(
                &format!("/api/v1/admin/node-gateway/nodes?{query}"),
                Some(token),
            )
            .await
    }

    pub async fn list_node_gateway_tasks(
        &self,
        params: &NodeGatewayListQueryParams,
        token: &str,
    ) -> Result<NodeGatewayTaskPage> {
        let query = params.to_query_string();
        self.client
            .get_json(
                &format!("/api/v1/admin/node-gateway/tasks?{query}"),
                Some(token),
            )
            .await
    }

    /// 获取待审批的注册令牌列表
    pub async fn list_pending_tokens(&self, token: &str) -> Result<Vec<PendingTokenWithUser>> {
        let mut tokens = Vec::new();
        let mut page = 1u64;
        loop {
            let params = PendingTokenQueryParams::default()
                .with_page(page)
                .with_page_size(COMPAT_LIST_PAGE_SIZE);
            let response = self.list_pending_tokens_page(&params, token).await?;
            tokens.extend(response.tokens);
            if response.total_pages == 0 || page >= response.total_pages {
                break;
            }
            page += 1;
        }
        Ok(tokens)
    }

    pub async fn list_pending_tokens_page(
        &self,
        params: &PendingTokenQueryParams,
        token: &str,
    ) -> Result<PendingTokenPage> {
        let query = params.to_query_string();
        self.client
            .get_json(
                &format!("/api/v1/admin/node-gateway/tokens/pending?{query}"),
                Some(token),
            )
            .await
    }

    /// 审批/拒绝注册令牌
    pub async fn approve_token(
        &self,
        token_id: &str,
        req: &ApproveTokenRequest,
        auth_token: &str,
    ) -> Result<serde_json::Value> {
        self.client
            .post_json(
                &format!("/api/v1/admin/node-gateway/tokens/{}/approve", token_id),
                req,
                Some(auth_token),
            )
            .await
    }

    /// 排除节点（从节点池中移除）
    pub async fn exclude_node(&self, node_id: &str, token: &str) -> Result<ExcludeNodeResponse> {
        self.client
            .post_json(
                &format!("/api/v1/admin/nodes/{}/exclude", node_id),
                &(),
                Some(token),
            )
            .await
    }

    /// 恢复被排除的节点
    pub async fn recover_node(&self, node_id: &str, token: &str) -> Result<RecoverNodeResponse> {
        self.client
            .post_json(
                &format!("/api/v1/admin/nodes/{}/recover", node_id),
                &(),
                Some(token),
            )
            .await
    }

    /// 吊销节点注册令牌（排除节点 + 将对应 token 标记为 rejected 并记录原因）
    pub async fn revoke_node_token(
        &self,
        node_id: &str,
        reason: &str,
        token: &str,
    ) -> Result<RevokeNodeTokenResponse> {
        let req = RevokeNodeRequest {
            reason: reason.to_string(),
        };
        self.client
            .post_json(
                &format!("/api/v1/admin/nodes/{}/revoke-token", node_id),
                &req,
                Some(token),
            )
            .await
    }

    /// 删除节点（彻底删除节点数据，清除关联 token）
    pub async fn delete_node(&self, node_id: &str, token: &str) -> Result<DeleteNodeResponse> {
        self.client
            .delete_json(&format!("/api/v1/admin/nodes/{}", node_id), Some(token))
            .await
    }

    pub async fn monitoring_overview(&self, token: &str) -> Result<MonitoringOverviewResponse> {
        self.client
            .get_json("/api/v1/admin/monitoring/overview", Some(token))
            .await
    }

    pub async fn monitoring_requests(
        &self,
        params: &MonitoringQuery,
        token: &str,
    ) -> Result<MonitoringRequestPage> {
        let query = params.to_query_string();
        let path = if query.is_empty() {
            "/api/v1/admin/monitoring/requests".to_string()
        } else {
            format!("/api/v1/admin/monitoring/requests?{query}")
        };
        self.client.get_json(&path, Some(token)).await
    }

    pub async fn monitoring_request(
        &self,
        request_id: &str,
        token: &str,
    ) -> Result<MonitoringRequestDetail> {
        self.client
            .get_json(
                &format!("/api/v1/admin/monitoring/requests/{request_id}"),
                Some(token),
            )
            .await
    }

    pub async fn monitoring_summary(
        &self,
        params: &MonitoringQuery,
        token: &str,
    ) -> Result<MonitoringSummaryResponse> {
        let query = params.to_query_string();
        let path = if query.is_empty() {
            "/api/v1/admin/monitoring/summary".to_string()
        } else {
            format!("/api/v1/admin/monitoring/summary?{query}")
        };
        self.client.get_json(&path, Some(token)).await
    }

    pub async fn monitoring_target_health(
        &self,
        params: &MonitoringQuery,
        token: &str,
    ) -> Result<MonitoringTargetHealthResponse> {
        let query = params.to_query_string();
        let path = if query.is_empty() {
            "/api/v1/admin/monitoring/targets/health".to_string()
        } else {
            format!("/api/v1/admin/monitoring/targets/health?{query}")
        };
        self.client.get_json(&path, Some(token)).await
    }

    pub async fn probe_monitoring_targets(
        &self,
        account_ids: Option<Vec<String>>,
        token: &str,
    ) -> Result<serde_json::Value> {
        self.client
            .post_json(
                "/api/v1/admin/monitoring/targets/probe",
                &MonitoringProbeRequest { account_ids },
                Some(token),
            )
            .await
    }

    // ==================== 定价管理 ====================

    /// 获取定价列表
    pub async fn list_pricing(&self, token: &str) -> Result<Vec<PricingInfo>> {
        let mut pricing = Vec::new();
        let mut page = 1u64;
        loop {
            let params = PricingQueryParams::default()
                .with_page(page)
                .with_page_size(COMPAT_LIST_PAGE_SIZE);
            let response = self.list_pricing_page(&params, token).await?;
            pricing.extend(response.pricing);
            if response.total_pages == 0 || page >= response.total_pages {
                break;
            }
            page += 1;
        }
        Ok(pricing)
    }

    pub async fn list_pricing_page(
        &self,
        params: &PricingQueryParams,
        token: &str,
    ) -> Result<PricingPage> {
        let query = params.to_query_string();
        let path = if query.is_empty() {
            "/api/v1/pricing".to_string()
        } else {
            format!("/api/v1/pricing?{query}")
        };
        self.client.get_json(&path, Some(token)).await
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

    /// 将定价设为默认
    pub async fn make_pricing_default(
        &self,
        id: &str,
        token: &str,
    ) -> Result<MakeDefaultPricingResponse> {
        self.client
            .post_json(
                &format!("/api/v1/pricing/{}/make-default", id),
                &(),
                Some(token),
            )
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
        let resp: PaymentOrderPage = self.client.get_json(&path, Some(token)).await?;
        Ok(resp.orders)
    }

    pub async fn list_payment_orders_page(
        &self,
        params: Option<&PaymentQueryParams>,
        token: &str,
    ) -> Result<PaymentOrderPage> {
        let path = if let Some(params) = params {
            format!("/api/v1/admin/payments/orders?{}", params.to_query_string())
        } else {
            "/api/v1/admin/payments/orders".to_string()
        };
        self.client.get_json(&path, Some(token)).await
    }

    /// 获取支付渠道的开关、配置和实际可用状态。
    pub async fn get_payment_providers(&self, token: &str) -> Result<Vec<PaymentProviderStatus>> {
        self.client
            .get_json("/api/v1/admin/payments/providers", Some(token))
            .await
    }

    /// 创建并关闭一笔真实小额订单，验证当前支付渠道配置。
    pub async fn verify_payment_provider(
        &self,
        method: &str,
        token: &str,
    ) -> Result<MessageResponse> {
        self.client
            .post_json(
                &format!("/api/v1/admin/payments/providers/{method}/verify"),
                &serde_json::json!({}),
                Some(token),
            )
            .await
    }
}
