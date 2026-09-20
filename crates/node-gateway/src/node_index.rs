//! Node Capability Index 实现
//!
//! 基于 PostgreSQL 实现 NodeCapabilityIndex trait，用于路由决策时检查是否存在 ready 节点。

use async_trait::async_trait;
use keycompute_db::DbRouter;
use keycompute_routing::NodeCapabilityIndex;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use std::sync::Arc;

/// Shared readiness predicate for dispatch and mode-aware model discovery.
/// Query aliases must be nodes `n`, node_sessions `ns`, and owner tenants `t`.
pub const READY_NODE_CONDITION: &str = "n.status = 'online' AND t.status = 'active' AND ns.expires_at > NOW() AND ns.revoked_at IS NULL AND n.capabilities_json->>'runtime' = 'ollama' AND ns.native_operations_json @> '[\"chat\"]'::jsonb";

fn ready_node_query() -> String {
    format!(
        "SELECT EXISTS (SELECT 1 FROM nodes n INNER JOIN node_sessions ns ON n.id = ns.node_id INNER JOIN users u ON u.id = n.owner_user_id INNER JOIN tenants t ON t.id = u.tenant_id WHERE {READY_NODE_CONDITION} AND ns.accepted_models_json @> $1::jsonb LIMIT 1)"
    )
}

/// 基于 PostgreSQL 的 Node 能力索引
pub struct PostgresNodeIndex {
    pool: Arc<DbRouter>,
}

impl PostgresNodeIndex {
    /// 创建新的 PostgresNodeIndex 实例
    pub fn new(pool: Arc<DbRouter>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl NodeCapabilityIndex for PostgresNodeIndex {
    /// 检查是否存在 ready 节点可以处理指定模型
    async fn has_ready_node(&self, model: &str) -> keycompute_types::Result<bool> {
        let model_json: serde_json::Value = serde_json::json!([model]);

        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            ready_node_query(),
            [model_json.into()],
        );

        // Routing is authorization-sensitive: a replica may still advertise a
        // node after its tenant was closed. Read the readiness predicate from
        // the writer so closure takes effect immediately.
        let row = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            self.pool.write_conn().query_one(stmt),
        )
        .await
        .map_err(|_| {
            keycompute_types::KeyComputeError::ServiceUnavailable(
                "Node metadata lookup timed out".into(),
            )
        })?
        .map_err(|_| {
            keycompute_types::KeyComputeError::ServiceUnavailable(
                "Node metadata unavailable".into(),
            )
        })?
        .ok_or_else(|| {
            keycompute_types::KeyComputeError::ServiceUnavailable("Missing node metadata".into())
        })?;
        row.try_get_by_index::<bool>(0).map_err(|_| {
            keycompute_types::KeyComputeError::ServiceUnavailable("Invalid node metadata".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ready_node_query;

    #[test]
    fn ready_node_query_requires_an_active_owner_tenant() {
        let ready_node_query = ready_node_query();
        assert!(ready_node_query.contains("INNER JOIN tenants t"));
        assert!(ready_node_query.contains("t.status = 'active'"));
        assert!(ready_node_query.contains("ns.expires_at > NOW()"));
        assert!(ready_node_query.contains("ns.revoked_at IS NULL"));
    }
}
