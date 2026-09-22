//! Tenant, explicit-platform and personal node control-plane endpoints.
use crate::{
    error::{ApiError, Result},
    extractors::{ConsoleAuth, GlobalConsoleAuth, RequestId},
    state::AppState,
    tenant_access::TenantAdmin,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use keycompute_auth::AuthorizationAction;
use keycompute_db::{
    AuditContext,
    models::{
        node_control::{
            self as dao, NodeAction, NodeChange, NodeControlScope, NodeFilter, NodeInfo, NodePatch,
            NodeResource, TaskInfo, TokenInfo,
        },
        tenant_control::TenantAuthzSnapshot,
    },
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct TenantPath {
    pub tenant_id: Uuid,
}
#[derive(Debug, Deserialize)]
pub struct ResourcePath {
    pub tenant_id: Uuid,
    pub id: Uuid,
}
#[derive(Debug, Deserialize)]
pub struct OwnedPath {
    pub id: Uuid,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub owner_user_id: Option<Uuid>,
    pub status: Option<String>,
    pub search: Option<String>,
}
impl ListQuery {
    pub fn filter(&self) -> NodeFilter {
        NodeFilter {
            owner_user_id: self.owner_user_id,
            status: self.status.clone(),
            search: self.search.clone(),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub expected_updated_at: DateTime<Utc>,
    pub reason: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configure {
    pub expected_updated_at: DateTime<Utc>,
    pub reason: String,
    pub display_name: Option<String>,
    pub failure_threshold: Option<i32>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenCommand {
    pub expected_updated_at: DateTime<Utc>,
    pub reason: String,
    pub action: String,
}
pub(crate) fn pool(s: &AppState) -> Result<&keycompute_db::DbRouter> {
    s.pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Node control storage unavailable".into()))
}
pub(crate) fn map(e: keycompute_db::DbError) -> ApiError {
    match e {
        keycompute_db::DbError::NotFound { .. } => {
            ApiError::NotFound("Node control resource not found".into())
        }
        keycompute_db::DbError::OptimisticConflict { .. } => ApiError::Conflict(
            "Node resource changed or retains required work; refresh before retrying".into(),
        ),
        keycompute_db::DbError::Other(m) if m.starts_with("invalid node control input:") => {
            ApiError::BadRequest(m)
        }
        keycompute_db::DbError::Other(m)
            if m.contains("authority")
                || m.contains("authorization")
                || m == "active actor required"
                || m == "active tenant membership required" =>
        {
            ApiError::Forbidden("Node control authorization denied".into())
        }
        _ => ApiError::ServiceUnavailable("Node control operation unavailable".into()),
    }
}
fn snapshot(a: &ConsoleAuth) -> TenantAuthzSnapshot {
    TenantAuthzSnapshot {
        token_version: a.token_version,
        tenant_authz_version: a.authz_version,
        membership_authz_version: a.membership_authz_version,
    }
}
pub(crate) fn platform_scope(
    a: &GlobalConsoleAuth,
    t: Uuid,
    action: AuthorizationAction,
) -> Result<NodeControlScope> {
    NodeControlScope::platform(
        a.require_platform(action)?,
        t,
        a.token_version,
        a.credential_kind,
    )
    .map_err(map)
}
pub(crate) fn platform_audit(a: &GlobalConsoleAuth, id: RequestId) -> AuditContext {
    AuditContext {
        actor_user_id: a.user_id,
        credential_kind: a.credential_kind,
        actor_platform_role: a.platform_role,
        actor_tenant_role: None,
        request_id: Some(id.0),
    }
}
fn tenant_scope(a: &TenantAdmin, t: Uuid) -> Result<NodeControlScope> {
    a.require_path_tenant(t)?;
    NodeControlScope::tenant(
        a.require(AuthorizationAction::ManageTenantResource)?,
        snapshot(a.auth()),
        a.auth().credential_kind,
    )
    .map_err(map)
}
fn owned_scope(a: &ConsoleAuth) -> Result<NodeControlScope> {
    NodeControlScope::owned(
        a.require_owner(a.user_id, AuthorizationAction::ReadPersonalResource)?,
        snapshot(a),
        a.credential_kind,
    )
    .map_err(map)
}
fn page(q: &ListQuery) -> Result<(i64, i64, i64)> {
    let p = q.page.unwrap_or(1);
    let s = q.page_size.unwrap_or(20);
    if !(1..=1_000_000).contains(&p) || !(1..=100).contains(&s) {
        return Err(ApiError::BadRequest("Invalid node pagination".into()));
    }
    Ok((p, s, (p - 1) * s))
}
pub(crate) async fn bounded<T>(
    f: impl std::future::Future<Output = std::result::Result<T, keycompute_db::DbError>>,
) -> Result<T> {
    tokio::time::timeout(std::time::Duration::from_secs(12), f)
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Node control timed out".into()))?
        .map_err(map)
}
macro_rules! read_family {
    ($page:ident,$detail:ident,$ty:ty,$list:ident,$one:ident,$kind:ident) => {
        pub(crate) async fn $page(
            s: &AppState,
            scope: NodeControlScope,
            q: ListQuery,
        ) -> Result<Json<Page<$ty>>> {
            let (p, n, o) = page(&q)?;
            let f = q.filter();
            let db = pool(s)?.write_conn();
            let items = bounded(dao::$list(db, scope, &f, n, o)).await?;
            let total = bounded(dao::count(db, scope, NodeResource::$kind, &f)).await?;
            Ok(Json(Page {
                items,
                total,
                page: p,
                page_size: n,
                total_pages: crate::handlers::pagination::total_pages(total, n),
            }))
        }
        async fn $detail(s: &AppState, scope: NodeControlScope, id: Uuid) -> Result<Json<$ty>> {
            Ok(Json(
                bounded(dao::$one(pool(s)?.write_conn(), scope, id))
                    .await?
                    .ok_or_else(|| ApiError::NotFound("Node control resource not found".into()))?,
            ))
        }
    };
}
read_family!(node_page, node_detail, NodeInfo, nodes, node, Node);
read_family!(task_page, task_detail, TaskInfo, tasks, task, Task);
read_family!(token_page, token_detail, TokenInfo, tokens, token, Token);
macro_rules! read_handlers {
    ($tl:ident,$td:ident,$pl:ident,$pd:ident,$ml:ident,$md:ident,$page:ident,$detail:ident,$ty:ty,$action:ident) => {
        pub async fn $tl(
            a: TenantAdmin,
            Path(p): Path<TenantPath>,
            State(s): State<AppState>,
            Query(q): Query<ListQuery>,
        ) -> Result<Json<Page<$ty>>> {
            $page(&s, tenant_scope(&a, p.tenant_id)?, q).await
        }
        pub async fn $td(
            a: TenantAdmin,
            Path(p): Path<ResourcePath>,
            State(s): State<AppState>,
        ) -> Result<Json<$ty>> {
            $detail(&s, tenant_scope(&a, p.tenant_id)?, p.id).await
        }
        pub async fn $pl(
            a: GlobalConsoleAuth,
            Path(p): Path<TenantPath>,
            State(s): State<AppState>,
            Query(q): Query<ListQuery>,
        ) -> Result<Json<Page<$ty>>> {
            $page(
                &s,
                platform_scope(&a, p.tenant_id, AuthorizationAction::$action)?,
                q,
            )
            .await
        }
        pub async fn $pd(
            a: GlobalConsoleAuth,
            Path(p): Path<ResourcePath>,
            State(s): State<AppState>,
        ) -> Result<Json<$ty>> {
            $detail(
                &s,
                platform_scope(&a, p.tenant_id, AuthorizationAction::$action)?,
                p.id,
            )
            .await
        }
        pub async fn $ml(
            a: ConsoleAuth,
            State(s): State<AppState>,
            Query(q): Query<ListQuery>,
        ) -> Result<Json<Page<$ty>>> {
            $page(&s, owned_scope(&a)?, q).await
        }
        pub async fn $md(
            a: ConsoleAuth,
            Path(p): Path<OwnedPath>,
            State(s): State<AppState>,
        ) -> Result<Json<$ty>> {
            $detail(&s, owned_scope(&a)?, p.id).await
        }
    };
}
read_handlers!(
    tenant_nodes,
    tenant_node,
    platform_nodes,
    platform_node,
    my_nodes,
    my_node,
    node_page,
    node_detail,
    NodeInfo,
    Diagnostics
);
read_handlers!(
    tenant_tasks,
    tenant_task,
    platform_tasks,
    platform_task,
    my_tasks,
    my_task,
    task_page,
    task_detail,
    TaskInfo,
    Diagnostics
);
read_handlers!(
    tenant_tokens,
    tenant_token,
    platform_tokens,
    platform_token,
    my_tokens,
    my_token,
    token_page,
    token_detail,
    TokenInfo,
    ManagePlatform
);
pub(crate) async fn node_command(
    s: &AppState,
    scope: NodeControlScope,
    id: Uuid,
    action: NodeAction,
    cmd: Command,
    audit: AuditContext,
) -> Result<Json<NodeChange>> {
    Ok(Json(
        bounded(dao::change_node(
            pool(s)?,
            scope,
            &audit,
            dao::NodeMutation {
                id,
                expected_updated_at: cmd.expected_updated_at,
                action,
                patch: &NodePatch::default(),
                reason: &cmd.reason,
            },
        ))
        .await?,
    ))
}
macro_rules! node_commands {
    ($tenant:ident,$platform:ident,$act:ident,$permission:ident) => {
        pub async fn $tenant(
            a: TenantAdmin,
            r: RequestId,
            Path(p): Path<ResourcePath>,
            State(s): State<AppState>,
            Json(cmd): Json<Command>,
        ) -> Result<Json<NodeChange>> {
            node_command(
                &s,
                tenant_scope(&a, p.tenant_id)?,
                p.id,
                NodeAction::$act,
                cmd,
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
        ) -> Result<Json<NodeChange>> {
            node_command(
                &s,
                platform_scope(&a, p.tenant_id, AuthorizationAction::$permission)?,
                p.id,
                NodeAction::$act,
                cmd,
                platform_audit(&a, r),
            )
            .await
        }
    };
}
node_commands!(tenant_exclude, platform_exclude, Exclude, NodeOperations);
node_commands!(tenant_recover, platform_recover, Recover, NodeOperations);
node_commands!(tenant_revoke, platform_revoke, Revoke, ManagePlatform);
async fn configure(
    s: &AppState,
    scope: NodeControlScope,
    id: Uuid,
    q: Configure,
    audit: AuditContext,
) -> Result<Json<NodeChange>> {
    Ok(Json(
        bounded(dao::change_node(
            pool(s)?,
            scope,
            &audit,
            dao::NodeMutation {
                id,
                expected_updated_at: q.expected_updated_at,
                action: NodeAction::Configure,
                patch: &NodePatch {
                    display_name: q.display_name,
                    failure_threshold: q.failure_threshold,
                },
                reason: &q.reason,
            },
        ))
        .await?,
    ))
}
pub async fn tenant_configure(
    a: TenantAdmin,
    r: RequestId,
    Path(p): Path<ResourcePath>,
    State(s): State<AppState>,
    Json(q): Json<Configure>,
) -> Result<Json<NodeChange>> {
    configure(&s, tenant_scope(&a, p.tenant_id)?, p.id, q, a.audit(r)).await
}
pub async fn platform_configure(
    a: GlobalConsoleAuth,
    r: RequestId,
    Path(p): Path<ResourcePath>,
    State(s): State<AppState>,
    Json(q): Json<Configure>,
) -> Result<Json<NodeChange>> {
    configure(
        &s,
        platform_scope(&a, p.tenant_id, AuthorizationAction::ManagePlatform)?,
        p.id,
        q,
        platform_audit(&a, r),
    )
    .await
}
pub async fn tenant_delete(
    a: TenantAdmin,
    r: RequestId,
    Path(p): Path<ResourcePath>,
    State(s): State<AppState>,
    Query(cmd): Query<Command>,
) -> Result<Json<NodeChange>> {
    node_command(
        &s,
        tenant_scope(&a, p.tenant_id)?,
        p.id,
        NodeAction::Delete,
        cmd,
        a.audit(r),
    )
    .await
}
pub async fn platform_delete(
    a: GlobalConsoleAuth,
    r: RequestId,
    Path(p): Path<ResourcePath>,
    State(s): State<AppState>,
    Query(cmd): Query<Command>,
) -> Result<Json<NodeChange>> {
    node_command(
        &s,
        platform_scope(&a, p.tenant_id, AuthorizationAction::ManagePlatform)?,
        p.id,
        NodeAction::Delete,
        cmd,
        platform_audit(&a, r),
    )
    .await
}
async fn approval_notice(s: &AppState, token: &TokenInfo) -> &'static str {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let Ok(pool) = pool(s) else {
        return "unavailable";
    };
    let recipient=pool.write_conn().query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT u.email FROM users u JOIN user_credentials c ON c.user_id=u.id AND c.email_verified JOIN tenant_memberships m ON m.user_id=u.id JOIN user_node_gateway_tokens t ON t.user_id=u.id AND t.tenant_id=m.tenant_id WHERE u.id=$1 AND m.tenant_id=$2 AND t.id=$3 AND t.status='approved' AND u.status='active' AND m.status='active'",[token.user_id.into(),token.tenant_id.into(),token.id.into()])).await;
    let row = match recipient {
        Ok(Some(row)) => row,
        Ok(None) => return "unverified_or_inactive_recipient",
        Err(_) => return "unavailable",
    };
    let Ok(email) = row.try_get::<String>("", "email") else {
        return "unavailable";
    };
    let text = format!(
        "Node registration {} for tenant {} has been approved. Sign in to your console to retrieve the registration credential. Credentials are not sent by email.",
        token.id, token.tenant_id
    );
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        s.email_service
            .send_text_email(&email, "Node registration approved", &text),
    )
    .await
    {
        Ok(Ok(())) => "sent",
        _ => "failed",
    }
}
pub(crate) async fn token_command(
    s: &AppState,
    scope: NodeControlScope,
    id: Uuid,
    cmd: TokenCommand,
    audit: AuditContext,
) -> Result<Json<serde_json::Value>> {
    let action = match cmd.action.as_str() {
        "approve" => NodeAction::ApproveToken,
        "reject" => NodeAction::RejectToken,
        "revoke" => NodeAction::RevokeToken,
        _ => {
            return Err(ApiError::BadRequest(
                "Unsupported registration action".into(),
            ));
        }
    };
    let change = bounded(dao::change_token(
        pool(s)?,
        scope,
        id,
        cmd.expected_updated_at,
        action,
        &audit,
        &cmd.reason,
    ))
    .await?;
    // The transaction is committed before SMTP. Failure is explicit, not a
    // false claim of delivery and not a reason to roll back valid approval.
    let notice = if action == NodeAction::ApproveToken && change.changed {
        approval_notice(s, &change.token).await
    } else {
        "not_applicable"
    };
    Ok(Json(
        serde_json::json!({"token":change.token,"changed":change.changed,"notification":notice}),
    ))
}

pub async fn tenant_decide_token(
    a: TenantAdmin,
    r: RequestId,
    Path(p): Path<ResourcePath>,
    State(s): State<AppState>,
    Json(cmd): Json<TokenCommand>,
) -> Result<Json<serde_json::Value>> {
    token_command(&s, tenant_scope(&a, p.tenant_id)?, p.id, cmd, a.audit(r)).await
}
pub async fn platform_decide_token(
    a: GlobalConsoleAuth,
    r: RequestId,
    Path(p): Path<ResourcePath>,
    State(s): State<AppState>,
    Json(cmd): Json<TokenCommand>,
) -> Result<Json<serde_json::Value>> {
    token_command(
        &s,
        platform_scope(&a, p.tenant_id, AuthorizationAction::ManagePlatform)?,
        p.id,
        cmd,
        platform_audit(&a, r),
    )
    .await
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/tenants/{tenant_id}/nodes", get(tenant_nodes))
        .route(
            "/api/v1/tenants/{tenant_id}/nodes/{id}",
            get(tenant_node)
                .patch(tenant_configure)
                .delete(tenant_delete),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/nodes/{id}/exclude",
            post(tenant_exclude),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/nodes/{id}/recover",
            post(tenant_recover),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/nodes/{id}/revoke",
            post(tenant_revoke),
        )
        .route("/api/v1/tenants/{tenant_id}/tasks", get(tenant_tasks))
        .route("/api/v1/tenants/{tenant_id}/tasks/{id}", get(tenant_task))
        .route(
            "/api/v1/tenants/{tenant_id}/node-registrations",
            get(tenant_tokens),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/node-registrations/{id}",
            get(tenant_token).post(tenant_decide_token),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/nodes",
            get(platform_nodes),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/nodes/{id}",
            get(platform_node)
                .patch(platform_configure)
                .delete(platform_delete),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/nodes/{id}/exclude",
            post(platform_exclude),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/nodes/{id}/recover",
            post(platform_recover),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/nodes/{id}/revoke",
            post(platform_revoke),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/tasks",
            get(platform_tasks),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/tasks/{id}",
            get(platform_task),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/node-registrations",
            get(platform_tokens),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/node-registrations/{id}",
            get(platform_token).post(platform_decide_token),
        )
        .route("/api/v1/me/nodes", get(my_nodes))
        .route("/api/v1/me/nodes/{id}", get(my_node))
        .route("/api/v1/me/tasks", get(my_tasks))
        .route("/api/v1/me/tasks/{id}", get(my_task))
        .route("/api/v1/me/node-registrations", get(my_tokens))
        .route("/api/v1/me/node-registrations/{id}", get(my_token))
        .layer(axum::extract::DefaultBodyLimit::max(8 * 1024))
}
