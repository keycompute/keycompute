//! Writer-fresh node profile lookup shared with model discovery.
use async_trait::async_trait;
use keycompute_db::DbRouter;
use keycompute_routing::NodeCapabilityIndex;
use keycompute_types::node_capability::NativeRequirements;
use keycompute_types::{KeyComputeError, Result};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use std::sync::Arc;
pub const READY_NODE_CONDITION: &str = "n.status = 'online' AND t.status = 'active' AND ns.expires_at > NOW() AND ns.revoked_at IS NULL AND ns.accepting_tasks=TRUE AND n.capabilities_json->>'runtime' = 'ollama'";
pub fn ready_profile_condition(model: &str, operation: &str) -> String {
    format!(
        "ns.native_profiles_json @> jsonb_build_array(jsonb_build_object('version',1,'model',{model},'operation',{operation}))"
    )
}
pub struct PostgresNodeIndex {
    pool: Arc<DbRouter>,
}
impl PostgresNodeIndex {
    pub fn new(pool: Arc<DbRouter>) -> Self {
        Self { pool }
    }
    async fn query(&self, required: serde_json::Value) -> Result<bool> {
        let sql = format!(
            r#"WITH required AS (SELECT $1::jsonb r)
            SELECT EXISTS(SELECT 1 FROM nodes n JOIN node_sessions ns ON ns.node_id=n.id
            JOIN users u ON u.id=n.owner_user_id JOIN tenants t ON t.id=u.tenant_id CROSS JOIN required
            WHERE {READY_NODE_CONDITION} AND ns.accepted_models_json @> jsonb_build_array(r->>'model')
              AND ns.native_operations_json @> jsonb_build_array(r->>'operation')
              AND EXISTS(SELECT 1 FROM jsonb_array_elements(ns.native_profiles_json) p WHERE {profile}))"#,
            profile = keycompute_db::models::native_capability::profile_matches("p", "r")
        );
        let row = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            self.pool
                .write_conn()
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    sql,
                    [required.into()],
                )),
        )
        .await
        .map_err(|_| {
            KeyComputeError::ServiceUnavailable("node capability lookup timed out".into())
        })?
        .map_err(|_| {
            KeyComputeError::ServiceUnavailable("node capability lookup unavailable".into())
        })?;
        row.ok_or_else(|| {
            KeyComputeError::ServiceUnavailable("node capability result unavailable".into())
        })?
        .try_get_by_index::<bool>(0)
        .map_err(|_| KeyComputeError::ServiceUnavailable("node capability result invalid".into()))
    }
}
#[async_trait]
impl NodeCapabilityIndex for PostgresNodeIndex {
    async fn has_ready_node(&self, model: &str) -> Result<bool> {
        self.query(serde_json::json!({"version":1,"model":model,"operation":"chat","features":[],"enforce_limits":false})).await
    }
    async fn has_ready_native(&self, needed: &NativeRequirements) -> Result<bool> {
        let value = serde_json::to_value(needed)
            .map_err(|_| KeyComputeError::InvalidRequest("invalid native requirements".into()))?;
        self.query(value).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_uses_session_and_owner_state() {
        for field in [
            "t.status = 'active'",
            "ns.expires_at > NOW()",
            "ns.revoked_at IS NULL",
            "ns.accepting_tasks=TRUE",
        ] {
            assert!(READY_NODE_CONDITION.contains(field));
        }
        let sql = ready_profile_condition("m.model", "'chat'");
        assert!(sql.contains("native_profiles_json"));
        assert!(sql.contains("'version',1"));
    }
}
