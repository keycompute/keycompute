use client_api::{
    AdminApi,
    api::admin::{
        ApproveTokenRequest, DeleteNodeResponse, NodeGatewayListQueryParams, NodeGatewayNodePage,
        NodeGatewayOverviewResponse, NodeGatewayTaskPage, PendingTokenPage,
        PendingTokenQueryParams, RecoverNodeResponse,
    },
    error::Result,
};

use super::api_client::get_client;

pub async fn overview(token: &str) -> Result<NodeGatewayOverviewResponse> {
    let client = get_client();
    AdminApi::new(&client).node_gateway_overview(token).await
}

pub async fn list_pending_tokens(
    params: &PendingTokenQueryParams,
    token: &str,
) -> Result<PendingTokenPage> {
    let client = get_client();
    AdminApi::new(&client)
        .list_pending_tokens_page(params, token)
        .await
}

pub async fn list_nodes(
    params: &NodeGatewayListQueryParams,
    token: &str,
) -> Result<NodeGatewayNodePage> {
    let client = get_client();
    AdminApi::new(&client)
        .list_node_gateway_nodes(params, token)
        .await
}

pub async fn list_tasks(
    params: &NodeGatewayListQueryParams,
    token: &str,
) -> Result<NodeGatewayTaskPage> {
    let client = get_client();
    AdminApi::new(&client)
        .list_node_gateway_tasks(params, token)
        .await
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
