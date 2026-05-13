use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct MonitoringOverviewResponse {
    pub summary: MonitoringSummary,
    pub traces: Vec<MonitoringTraceEntry>,
    pub nodes: Vec<MonitoringNodeHealth>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct MonitoringSummary {
    pub total_usage_logs: i64,
    pub total_node_tasks: i64,
    pub active_node_tasks: i64,
    pub succeeded_node_tasks: i64,
    pub failed_node_tasks: i64,
    pub online_nodes: i64,
    pub avg_node_latency_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct MonitoringTraceEntry {
    pub request_id: String,
    pub task_id: String,
    pub model: String,
    pub status: String,
    pub node_id: Option<String>,
    pub node_name: Option<String>,
    pub lease_id: Option<String>,
    pub queued_at: String,
    pub claimed_at: Option<String>,
    pub finished_at: Option<String>,
    pub deadline_at: String,
    pub duration_ms: Option<i64>,
    pub usage_status: Option<String>,
    pub total_tokens: Option<i32>,
    pub amount: Option<String>,
    pub submissions_count: i64,
    pub last_submission_action: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct MonitoringNodeHealth {
    pub id: String,
    pub display_name: String,
    pub status: String,
    pub accepted_models_json: serde_json::Value,
    pub last_heartbeat_at: Option<String>,
    pub active_tasks: i64,
    pub succeeded_tasks: i64,
    pub failed_tasks: i64,
}
