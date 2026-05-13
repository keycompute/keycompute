use serde::Deserialize;

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
