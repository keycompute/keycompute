//! Metadata-only lease checks for cancellation-aware native workers.
use crate::{NodeGatewayStore, PostgresNodeIndex};
use keycompute_db::DbError;
use keycompute_routing::NodeCapabilityIndex;
use keycompute_types::{
    node::{NodeTaskLeaseStatusRequest, NodeTaskLeaseStatusResponse},
    node_capability::{NativeFeature, NativeRequirements},
    node_native::NodeNativeRequest,
};
use sea_orm::{DbBackend, FromQueryResult, Statement};
use std::time::Duration;
#[derive(FromQueryResult)]
struct LeaseState {
    active: bool,
    status: String,
}
impl NodeGatewayStore {
    /// This is only a readiness snapshot; persisted task requirements enforce
    /// the same capability again in the atomic lease claim.
    pub async fn cancellable_native_ready(
        &self,
        native: &NodeNativeRequest,
    ) -> Result<bool, DbError> {
        let mut required =
            NativeRequirements::from_request(native).map_err(|e| DbError::Other(e.into()))?;
        if !required.features.contains(&NativeFeature::Cancellation) {
            required.features.push(NativeFeature::Cancellation);
        }
        PostgresNodeIndex::new(self.pool_arc())
            .has_ready_native(&required)
            .await
            .map_err(|e| DbError::Other(e.to_string()))
    }
    pub async fn native_lease_status(
        &self,
        request: &NodeTaskLeaseStatusRequest,
    ) -> Result<NodeTaskLeaseStatusResponse, DbError> {
        if request.protocol_version != "node.v1" {
            return Err(DbError::Other("native_control_protocol_invalid".into()));
        }
        let statement = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT nt.status,
                (nt.status='leased' AND nt.deadline_at>NOW()
                 AND ns.revoked_at IS NULL AND ns.expires_at>NOW()
                 AND n.status='online' AND owner_t.status='active' AND caller_t.status='active'
                 AND (trace.tenant_id IS NULL OR trace.tenant_id=caller.tenant_id)
                 AND (managed.id IS NULL OR (managed.status IN ('queued','in_progress') AND managed.deleted_at IS NULL))) AS active
            FROM node_tasks nt
            JOIN node_sessions ns ON ns.id=nt.assigned_session_id AND ns.node_id=nt.assigned_node_id
            JOIN nodes n ON n.id=ns.node_id
            JOIN users owner ON owner.id=n.owner_user_id
            JOIN tenants owner_t ON owner_t.id=owner.tenant_id
            JOIN users caller ON caller.id=nt.user_id
            JOIN tenants caller_t ON caller_t.id=caller.tenant_id
            LEFT JOIN gateway_requests trace ON trace.request_id=nt.request_id
            LEFT JOIN scoped_responses managed ON managed.request_id=nt.request_id
            WHERE nt.id=$1 AND nt.assigned_node_id=$2 AND nt.assigned_session_id=$3
              AND nt.lease_id=$4 AND nt.native_requirements_json IS NOT NULL
        "#,
            [
                request.task_id.into(),
                request.node_id.into(),
                request.session_id.into(),
                request.lease_id.into(),
            ],
        );
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            LeaseState::find_by_statement(statement).one(self.pool().write_conn()),
        )
        .await
        .map_err(|_| DbError::Other("native_control_query_timeout".into()))??
        .ok_or_else(|| DbError::not_found("NodeTask", request.task_id.to_string()))?;
        Ok(NodeTaskLeaseStatusResponse {
            active: result.active,
            status: result.status,
        })
    }
}
