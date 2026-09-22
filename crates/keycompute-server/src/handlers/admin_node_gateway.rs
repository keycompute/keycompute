//! Legacy URL adapters; all operations use the canonical scoped control plane.
use super::tenant_nodes::{self as control, Command, ListQuery};
use crate::{
    error::Result,
    extractors::{GlobalConsoleAuth, RequestId},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::{DateTime, Utc};
use keycompute_auth::AuthorizationAction;
use keycompute_db::models::node_control::{
    self as dao, NodeAction, NodeChange, NodeInfo, TaskInfo,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetQuery {
    pub tenant_id: Uuid,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub status: Option<String>,
    pub search: Option<String>,
    pub owner_user_id: Option<Uuid>,
}
impl TargetQuery {
    fn list(self) -> ListQuery {
        ListQuery {
            page: self.page,
            page_size: self.page_size,
            status: self.status,
            search: self.search,
            owner_user_id: self.owner_user_id,
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub tenant_id: Uuid,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteQuery {
    pub tenant_id: Uuid,
    pub expected_updated_at: DateTime<Utc>,
    pub reason: String,
}
#[derive(Debug, Serialize)]
pub struct NodeGatewayOverviewResponse {
    pub enabled: bool,
    pub node_stats: dao::NodeStats,
    pub task_stats: dao::TaskStats,
}
pub async fn get_node_gateway_overview(
    a: GlobalConsoleAuth,
    State(s): State<AppState>,
    Query(q): Query<Target>,
) -> Result<Json<NodeGatewayOverviewResponse>> {
    let scope = control::platform_scope(&a, q.tenant_id, AuthorizationAction::Diagnostics)?;
    let db = control::pool(&s)?.write_conn();
    Ok(Json(NodeGatewayOverviewResponse {
        enabled: s.node_gateway.is_some(),
        node_stats: control::bounded(dao::node_stats(db, scope)).await?,
        task_stats: control::bounded(dao::task_stats(db, scope)).await?,
    }))
}
pub async fn list_node_gateway_nodes(
    a: GlobalConsoleAuth,
    State(s): State<AppState>,
    Query(q): Query<TargetQuery>,
) -> Result<Json<control::Page<NodeInfo>>> {
    let scope = control::platform_scope(&a, q.tenant_id, AuthorizationAction::Diagnostics)?;
    control::node_page(&s, scope, q.list()).await
}
pub async fn list_node_gateway_tasks(
    a: GlobalConsoleAuth,
    State(s): State<AppState>,
    Query(q): Query<TargetQuery>,
) -> Result<Json<control::Page<TaskInfo>>> {
    let scope = control::platform_scope(&a, q.tenant_id, AuthorizationAction::Diagnostics)?;
    control::task_page(&s, scope, q.list()).await
}
macro_rules! commands {
    ($name:ident,$action:ident,$permission:ident) => {
        pub async fn $name(
            a: GlobalConsoleAuth,
            r: RequestId,
            Path(id): Path<Uuid>,
            State(s): State<AppState>,
            Query(q): Query<Target>,
            Json(cmd): Json<Command>,
        ) -> Result<Json<NodeChange>> {
            let scope = control::platform_scope(&a, q.tenant_id, AuthorizationAction::$permission)?;
            control::node_command(
                &s,
                scope,
                id,
                NodeAction::$action,
                cmd,
                control::platform_audit(&a, r),
            )
            .await
        }
    };
}
commands!(recover_node, Recover, NodeOperations);
commands!(exclude_node, Exclude, NodeOperations);
commands!(revoke_node_token, Revoke, ManagePlatform);
pub async fn delete_node(
    a: GlobalConsoleAuth,
    r: RequestId,
    Path(id): Path<Uuid>,
    State(s): State<AppState>,
    Query(q): Query<DeleteQuery>,
) -> Result<Json<NodeChange>> {
    let scope = control::platform_scope(&a, q.tenant_id, AuthorizationAction::ManagePlatform)?;
    control::node_command(
        &s,
        scope,
        id,
        NodeAction::Delete,
        Command {
            expected_updated_at: q.expected_updated_at,
            reason: q.reason,
        },
        control::platform_audit(&a, r),
    )
    .await
}
