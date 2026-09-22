//! Task commands keep original execution ownership and durable settlement evidence.
use super::tenant_nodes::{self as nodes, Command, OwnedPath, ResourcePath};
use crate::{
    error::Result,
    extractors::{ConsoleAuth, GlobalConsoleAuth, RequestId},
    state::AppState,
    tenant_access::TenantAdmin,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::post,
};
use keycompute_auth::AuthorizationAction;
use keycompute_db::{
    AuditContext,
    models::node_control::{self as dao, NodeControlScope, TaskAction, TaskChange, TaskMutation},
};
use uuid::Uuid;
async fn execute(
    s: &AppState,
    scope: NodeControlScope,
    id: Uuid,
    cmd: Command,
    action: TaskAction,
    actor: AuditContext,
) -> Result<Json<TaskChange>> {
    Ok(Json(
        nodes::bounded(dao::change_task(
            nodes::pool(s)?,
            scope,
            &actor,
            TaskMutation {
                id,
                expected_updated_at: cmd.expected_updated_at,
                action,
                reason: &cmd.reason,
            },
        ))
        .await?,
    ))
}
macro_rules! commands {
    ($tenant:ident,$platform:ident,$personal:ident,$action:ident) => {
        pub async fn $tenant(
            a: TenantAdmin,
            r: RequestId,
            Path(p): Path<ResourcePath>,
            State(s): State<AppState>,
            Json(cmd): Json<Command>,
        ) -> Result<Json<TaskChange>> {
            execute(
                &s,
                nodes::tenant_scope(&a, p.tenant_id)?,
                p.id,
                cmd,
                TaskAction::$action,
                a.audit(r),
            )
            .await
        }
        pub async fn $platform(
            a: GlobalConsoleAuth,
            r: RequestId,
            Path(p): Path<ResourcePath>,
            State(s): State<AppState>,
            Json(cmd): Json<Command>,
        ) -> Result<Json<TaskChange>> {
            execute(
                &s,
                nodes::platform_scope(&a, p.tenant_id, AuthorizationAction::ManagePlatform)?,
                p.id,
                cmd,
                TaskAction::$action,
                nodes::platform_audit(&a, r),
            )
            .await
        }
        pub async fn $personal(
            a: ConsoleAuth,
            r: RequestId,
            Path(p): Path<OwnedPath>,
            State(s): State<AppState>,
            Json(cmd): Json<Command>,
        ) -> Result<Json<TaskChange>> {
            a.require_owner(a.user_id, AuthorizationAction::ManagePersonalResource)?;
            let actor = AuditContext {
                actor_user_id: a.user_id,
                credential_kind: a.credential_kind,
                actor_platform_role: a.platform_role,
                actor_tenant_role: a.tenant_role,
                request_id: Some(r.0),
            };
            execute(
                &s,
                nodes::owned_scope(&a)?,
                p.id,
                cmd,
                TaskAction::$action,
                actor,
            )
            .await
        }
    };
}
commands!(tenant_cancel, platform_cancel, my_cancel, Cancel);
commands!(tenant_archive, platform_archive, my_archive, Archive);
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/tenants/{tenant_id}/tasks/{id}/cancel",
            post(tenant_cancel),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/tasks/{id}/archive",
            post(tenant_archive),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/tasks/{id}/cancel",
            post(platform_cancel),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/tasks/{id}/archive",
            post(platform_archive),
        )
        .route("/api/v1/me/tasks/{id}/cancel", post(my_cancel))
        .route("/api/v1/me/tasks/{id}/archive", post(my_archive))
        .layer(axum::extract::DefaultBodyLimit::max(8 * 1024))
}
