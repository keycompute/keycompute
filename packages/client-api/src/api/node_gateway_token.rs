//! Node Gateway Token 管理模块
//!
//! 处理用户 Node Gateway 注册令牌的查询、申请和删除。
//! 审批制：用户申请 → Admin 审批 → token 下发 → 用户随时可查看明文（is_revealed 仅标记已查看）。

use crate::api::common::MessageResponse;
use crate::client::ApiClient;
use crate::error::Result;
use serde::Deserialize;

/// Node Gateway Token API 客户端
#[derive(Debug, Clone)]
pub struct NodeGatewayTokenApi {
    client: ApiClient,
}

impl NodeGatewayTokenApi {
    /// 创建新的 API 客户端
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    /// 获取当前用户的 node gateway registration token 信息
    ///
    /// GET /api/v1/me/node-gateway/token
    ///
    /// 如果 token 已审批 → 始终返回 registration_token 明文（is_revealed=true 时附带提醒）
    /// 否则 → registration_token 为 null
    pub async fn get_my_token(&self, auth_token: &str) -> Result<NodeGatewayTokenDetail> {
        self.client
            .get_json("/api/v1/me/node-gateway/token", Some(auth_token))
            .await
    }

    /// 获取当前用户的所有 node gateway registration token（历史列表）
    ///
    /// GET /api/v1/me/node-gateway/tokens
    pub async fn list_my_tokens(&self, auth_token: &str) -> Result<Vec<NodeGatewayTokenDetail>> {
        self.client
            .get_json("/api/v1/me/node-gateway/tokens", Some(auth_token))
            .await
    }

    /// 申请 node gateway registration token
    ///
    /// POST /api/v1/me/node-gateway/token
    ///
    /// 创建申请后需等待 Admin 审批。
    pub async fn create_my_token(&self, auth_token: &str) -> Result<NodeGatewayTokenDetail> {
        self.client
            .post_json("/api/v1/me/node-gateway/token", &(), Some(auth_token))
            .await
    }

    /// 删除被拒绝的令牌记录
    ///
    /// DELETE /api/v1/me/node-gateway/token/{id}
    ///
    /// 仅允许删除 status='rejected' 且 revoke_reason IS NULL 的令牌
    pub async fn delete_my_token(
        &self,
        token_id: &str,
        auth_token: &str,
    ) -> Result<MessageResponse> {
        self.client
            .delete_json(
                &format!("/api/v1/me/node-gateway/token/{}", token_id),
                Some(auth_token),
            )
            .await
    }
}

/// 已注册节点信息（用户端查看令牌时展示）
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RegisteredNodeInfo {
    pub id: String,
    pub display_name: String,
    pub status: String,
    pub last_heartbeat_at: Option<String>,
}

/// Node Gateway Token 详情响应
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NodeGatewayTokenDetail {
    pub token: NodeGatewayTokenInfo,
    /// token 明文（审批通过后始终返回；is_revealed=true 时仍返回但附带安全提醒）
    pub registration_token: Option<String>,
    /// 提示信息
    #[serde(default)]
    pub message: Option<String>,
    /// 已注册的节点信息（token 已消费时返回）
    #[serde(default)]
    pub registered_node: Option<RegisteredNodeInfo>,
}

/// Node Gateway Token 信息（不含明文）
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NodeGatewayTokenInfo {
    pub id: String,
    pub user_id: String,
    pub token_preview: String,
    pub status: String,
    pub is_revealed: bool,
    pub actioned_at: Option<String>,
    pub consumed_at: Option<String>,
    pub revoke_reason: Option<String>,
    pub issued_at: String,
}
