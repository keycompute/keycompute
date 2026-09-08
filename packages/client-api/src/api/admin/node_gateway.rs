use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct NodeGatewayOverviewResponse {
    pub enabled: bool,
    pub node_stats: NodeGatewayNodeStats,
    pub task_stats: NodeGatewayTaskStats,
}

#[derive(Debug, Clone, Default)]
pub struct NodeGatewayListQueryParams {
    pub status: Option<String>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

impl NodeGatewayListQueryParams {
    pub fn with_page(mut self, page: u64) -> Self {
        self.page = Some(page);
        self
    }

    pub fn with_page_size(mut self, page_size: u64) -> Self {
        self.page_size = Some(page_size);
        self
    }

    pub fn to_query_string(&self) -> String {
        let mut params = Vec::new();
        if let Some(status) = self.status.as_deref().filter(|value| !value.is_empty()) {
            params.push(format!(
                "status={}",
                crate::api::common::encode_query_value(status)
            ));
        }
        if let Some(page) = self.page {
            params.push(format!("page={page}"));
        }
        if let Some(page_size) = self.page_size {
            params.push(format!("page_size={page_size}"));
        }
        params.join("&")
    }
}

/// 待审批注册令牌列表查询参数。
///
/// 该接口支持按用户邮箱、token ID 或 token 预览搜索，不接受节点/任务的状态筛选。
#[derive(Debug, Clone, Default)]
pub struct PendingTokenQueryParams {
    pub search: Option<String>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

impl PendingTokenQueryParams {
    pub fn with_search(mut self, search: impl Into<String>) -> Self {
        self.search = Some(search.into());
        self
    }

    pub fn with_page(mut self, page: u64) -> Self {
        self.page = Some(page);
        self
    }

    pub fn with_page_size(mut self, page_size: u64) -> Self {
        self.page_size = Some(page_size);
        self
    }

    pub fn to_query_string(&self) -> String {
        let mut params = Vec::new();
        if let Some(search) = self.search.as_deref().filter(|value| !value.is_empty()) {
            params.push(format!(
                "search={}",
                crate::api::common::encode_query_value(search)
            ));
        }
        if let Some(page) = self.page {
            params.push(format!("page={page}"));
        }
        if let Some(page_size) = self.page_size {
            params.push(format!("page_size={page_size}"));
        }
        params.join("&")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeGatewayNodePage {
    pub nodes: Vec<NodeGatewayNodeInfo>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeGatewayTaskPage {
    pub tasks: Vec<NodeGatewayTaskInfo>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
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

#[derive(Debug, Clone, Deserialize)]
pub struct PendingTokenPage {
    pub tokens: Vec<PendingTokenWithUser>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
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

#[cfg(test)]
mod tests {
    use super::{NodeGatewayListQueryParams, PendingTokenQueryParams};

    #[test]
    fn node_gateway_query_serializes_status_and_pagination() {
        let query = NodeGatewayListQueryParams {
            status: Some("online".to_string()),
            ..Default::default()
        }
        .with_page(4)
        .with_page_size(20)
        .to_query_string();
        assert_eq!(query, "status=online&page=4&page_size=20");
    }

    #[test]
    fn pending_token_query_serializes_search_and_pagination() {
        let query = PendingTokenQueryParams::default()
            .with_search("alice+ops@example.com")
            .with_page(3)
            .with_page_size(25)
            .to_query_string();
        assert_eq!(
            query,
            "search=alice%2Bops%40example.com&page=3&page_size=25"
        );
    }
}
