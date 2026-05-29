use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct NodeGatewayOverviewResponse {
    pub enabled: bool,
    pub node_stats: NodeGatewayNodeStats,
    pub task_stats: NodeGatewayTaskStats,
    pub nodes: Vec<NodeGatewayNodeInfo>,
    pub recent_tasks: Vec<NodeGatewayTaskInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeGatewayNodeStats {
    pub total: i64,
    pub online: i64,
    pub offline: i64,
    pub excluded: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeGatewayTaskStats {
    pub total: i64,
    pub queued: i64,
    pub leased: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub expired: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeGatewayNodeInfo {
    pub id: String,
    pub display_name: String,
    pub client_instance_id: String,
    pub status: String,
    pub accepted_models_json: serde_json::Value,
    pub consecutive_failure_count: i32,
    pub failure_threshold: i32,
    pub last_heartbeat_at: Option<String>,
    pub updated_at: String,
    /// 注册该节点时使用的 token 预览
    pub token_preview: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeGatewayTaskInfo {
    pub id: String,
    pub model: String,
    pub status: String,
    pub assigned_node_id: Option<String>,
    pub failure_count: i32,
    pub failure_threshold: i32,
    pub queued_at: String,
    pub deadline_at: String,
    pub updated_at: String,
}

/// 待审批 token 附带用户邮箱（Admin 使用）
#[derive(Debug, Clone, Deserialize)]
pub struct PendingTokenWithUser {
    pub id: String,
    pub user_id: String,
    pub token_preview: String,
    pub status: String,
    pub issued_at: String,
    pub user_email: String,
}

/// Admin 审批 token 请求
#[derive(Debug, Clone, Serialize)]
pub struct ApproveTokenRequest {
    pub action: String, // "approve" or "reject"
}

/// Admin 吊销节点请求
#[derive(Debug, Clone, Serialize)]
pub struct RevokeNodeRequest {
    pub reason: String,
}

/// 排除节点响应
#[derive(Debug, Clone, Deserialize)]
pub struct ExcludeNodeResponse {
    pub id: String,
    pub status: String,
}

/// 吊销节点注册令牌响应
#[derive(Debug, Clone, Deserialize)]
pub struct RevokeNodeTokenResponse {
    pub id: String,
    pub node_status: String,
    pub token_status: String,
    pub revoke_reason: String,
}

/// 恢复节点响应
#[derive(Debug, Clone, Deserialize)]
pub struct RecoverNodeResponse {
    pub id: String,
    pub status: String,
    pub consecutive_failure_count: i32,
}

/// 删除节点响应
#[derive(Debug, Clone, Deserialize)]
pub struct DeleteNodeResponse {
    pub id: String,
    pub deleted: bool,
}
