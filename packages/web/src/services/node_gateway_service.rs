use client_api::{
    AdminApi,
    api::admin::{
        ApproveTokenRequest, DeleteNodeResponse, NodeGatewayOverviewResponse, PendingTokenWithUser,
        RecoverNodeResponse,
    },
    error::Result,
};

use super::api_client::get_client;

pub async fn overview(token: &str) -> Result<NodeGatewayOverviewResponse> {
    let client = get_client();
    AdminApi::new(&client).node_gateway_overview(token).await
}

pub async fn list_pending_tokens(token: &str) -> Result<Vec<PendingTokenWithUser>> {
    let client = get_client();
    AdminApi::new(&client).list_pending_tokens(token).await
}

pub async fn approve_token(
    token_id: &str,
    req: &ApproveTokenRequest,
    auth_token: &str,
) -> Result<serde_json::Value> {
    let client = get_client();
    AdminApi::new(&client)
        .approve_token(token_id, req, auth_token)
        .await
}

#[allow(dead_code)]
pub async fn exclude_node(
    node_id: &str,
    token: &str,
) -> Result<client_api::api::admin::ExcludeNodeResponse> {
    let client = get_client();
    AdminApi::new(&client).exclude_node(node_id, token).await
}

pub async fn recover_node(node_id: &str, token: &str) -> Result<RecoverNodeResponse> {
    let client = get_client();
    AdminApi::new(&client).recover_node(node_id, token).await
}

pub async fn revoke_node_token(
    node_id: &str,
    reason: &str,
    token: &str,
) -> Result<client_api::api::admin::RevokeNodeTokenResponse> {
    let client = get_client();
    AdminApi::new(&client)
        .revoke_node_token(node_id, reason, token)
        .await
}

pub async fn delete_node(node_id: &str, token: &str) -> Result<DeleteNodeResponse> {
    let client = get_client();
    AdminApi::new(&client).delete_node(node_id, token).await
}
