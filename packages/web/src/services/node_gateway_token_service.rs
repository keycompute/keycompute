use client_api::api::node_gateway_token::{NodeGatewayTokenApi, NodeGatewayTokenDetail};
use client_api::error::Result;

use super::api_client::get_client;

/// 获取当前用户的节点注册令牌详情（最近一条）
#[allow(dead_code)]
pub async fn get_my_token(token: &str) -> Result<NodeGatewayTokenDetail> {
    NodeGatewayTokenApi::new(&get_client())
        .get_my_token(token)
        .await
}

/// 获取当前用户的所有节点注册令牌（历史列表）
pub async fn list_my_tokens(token: &str) -> Result<Vec<NodeGatewayTokenDetail>> {
    NodeGatewayTokenApi::new(&get_client())
        .list_my_tokens(token)
        .await
}

/// 删除被拒绝的令牌记录（仅 rejected 且无 revoke_reason）
pub async fn delete_my_token(token_id: &str, token: &str) -> Result<()> {
    NodeGatewayTokenApi::new(&get_client())
        .delete_my_token(token_id, token)
        .await
        .map(|_| ())
}

/// 申请节点注册令牌
pub async fn create_my_token(token: &str) -> Result<NodeGatewayTokenDetail> {
    NodeGatewayTokenApi::new(&get_client())
        .create_my_token(token)
        .await
}
