//! Owner-only node registration. Administration only handles metadata.
use crate::{
    error::{ApiError, Result},
    extractors::{ConsoleAuth, RequestId},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use keycompute_db::{
    AuditContext,
    models::{
        node_control::{self as dao, NodeControlScope},
        tenant_control::TenantAuthzSnapshot,
        user_node_gateway_token::{UserNodeGatewayToken, UserNodeGatewayTokenResponse},
    },
};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Serialize;
use uuid::Uuid;
#[derive(Serialize)]
pub struct NodeGatewayTokenDetailResponse {
    pub token: UserNodeGatewayTokenResponse,
    pub registration_token: Option<String>,
    pub message: Option<String>,
}
fn owned(a: &ConsoleAuth) -> Result<NodeControlScope> {
    NodeControlScope::owned(
        a.require_owner(
            a.user_id,
            keycompute_auth::AuthorizationAction::ManagePersonalResource,
        )?,
        TenantAuthzSnapshot {
            token_version: a.token_version,
            tenant_authz_version: a.authz_version,
            membership_authz_version: a.membership_authz_version,
        },
        a.credential_kind,
    )
    .map_err(super::tenant_nodes::map)
}
fn audit(a: &ConsoleAuth, r: RequestId) -> AuditContext {
    AuditContext {
        actor_user_id: a.user_id,
        credential_kind: a.credential_kind,
        actor_platform_role: a.platform_role,
        actor_tenant_role: a.tenant_role,
        request_id: Some(r.0),
    }
}
fn private(value: impl Serialize) -> Response {
    (
        [
            (header::CACHE_CONTROL, "private, no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        Json(value),
    )
        .into_response()
}
pub async fn get_my_node_gateway_token(
    auth: ConsoleAuth,
    r: RequestId,
    State(state): State<AppState>,
) -> Result<Response> {
    let scope = owned(&auth)?;
    let tx = super::tenant_nodes::pool(&state)?.begin().await?;
    tx.execute_unprepared("SET LOCAL lock_timeout='3s'; SET LOCAL statement_timeout='8s'")
        .await?;
    let result = async {
        let t = dao::owner_registration_for_reveal(&tx, scope, &audit(&auth, r))
            .await
            .map_err(super::tenant_nodes::map)?;
        let raw = if t.status == "approved" {
            let secret = state.node_gateway_secret().ok_or_else(|| {
                ApiError::ServiceUnavailable("Registration signing unavailable".into())
            })?;
            let raw = UserNodeGatewayToken::reconstruct_token(secret.as_bytes(), t.id);
            if UserNodeGatewayToken::hash_token(&raw) != t.token_hash {
                return Err(ApiError::ServiceUnavailable(
                    "Registration signing changed; request a new credential".into(),
                ));
            }
            Some(raw)
        } else {
            None
        };
        let message = if t.status == "approved" {
            Some("Keep this owner-only registration credential private.".into())
        } else {
            None
        };
        Ok(NodeGatewayTokenDetailResponse {
            token: UserNodeGatewayTokenResponse::from(t),
            registration_token: raw,
            message,
        })
    }
    .await;
    match result {
        Ok(view) => {
            tx.commit().await?;
            Ok(private(view))
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
pub async fn list_my_node_gateway_tokens(
    auth: ConsoleAuth,
    State(state): State<AppState>,
) -> Result<Response> {
    let scope = owned(&auth)?;
    let db = super::tenant_nodes::pool(&state)?.write_conn();
    let total = dao::count(
        db,
        scope,
        dao::NodeResource::Token,
        &dao::NodeFilter::default(),
    )
    .await
    .map_err(super::tenant_nodes::map)?;
    if total > 100 {
        return Err(ApiError::BadRequest(
            "Use paginated /api/v1/me/node-registrations for this history".into(),
        ));
    }
    let records = dao::tokens(db, scope, &dao::NodeFilter::default(), 100, 0)
        .await
        .map_err(super::tenant_nodes::map)?;
    let values=records.into_iter().map(|token|serde_json::json!({"token":token,"registration_token":null,"message":"Use the owner credential endpoint for the current approved token."})).collect::<Vec<_>>();
    Ok(private(values))
}
pub async fn create_my_node_gateway_token(
    auth: ConsoleAuth,
    r: RequestId,
    State(state): State<AppState>,
) -> Result<Response> {
    let scope = owned(&auth)?;
    let secret = state
        .node_gateway_secret()
        .ok_or_else(|| ApiError::ServiceUnavailable("Registration signing unavailable".into()))?;
    let (id, _plaintext, hash, preview) =
        UserNodeGatewayToken::generate_hmac_token(secret.as_bytes());
    let token = dao::request_owner_registration(
        super::tenant_nodes::pool(&state)?,
        scope,
        id,
        &hash,
        &preview,
        &audit(&auth, r),
    )
    .await
    .map_err(super::tenant_nodes::map)?;
    Ok(private(
        serde_json::json!({"token":token,"registration_token":null,"message":"Registration request requires administrative approval."}),
    ))
}
pub async fn delete_my_node_gateway_token(
    auth: ConsoleAuth,
    r: RequestId,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    dao::delete_owner_rejected_registration(
        super::tenant_nodes::pool(&state)?,
        owned(&auth)?,
        id,
        &audit(&auth, r),
    )
    .await
    .map_err(super::tenant_nodes::map)?;
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({"message":"Token deleted"})),
    ))
}
use axum::extract::Query;
/// Legacy platform URL requires a mandatory target and current root authority.
pub async fn admin_list_pending_tokens(
    auth: crate::extractors::GlobalConsoleAuth,
    State(state): State<AppState>,
    Query(params): Query<super::admin_node_gateway::TargetQuery>,
) -> Result<Json<super::tenant_nodes::Page<keycompute_db::models::node_control::TokenInfo>>> {
    let scope = super::tenant_nodes::platform_scope(
        &auth,
        params.tenant_id,
        keycompute_auth::AuthorizationAction::ManagePlatform,
    )?;
    super::tenant_nodes::token_page(
        &state,
        scope,
        super::tenant_nodes::ListQuery {
            page: params.page,
            page_size: params.page_size,
            status: Some("pending".into()),
            search: params.search,
            owner_user_id: params.owner_user_id,
        },
    )
    .await
}
pub async fn admin_approve_token(
    auth: crate::extractors::GlobalConsoleAuth,
    request_id: crate::extractors::RequestId,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(target): Query<super::admin_node_gateway::Target>,
    Json(body): Json<super::tenant_nodes::TokenCommand>,
) -> Result<Json<serde_json::Value>> {
    let scope = super::tenant_nodes::platform_scope(
        &auth,
        target.tenant_id,
        keycompute_auth::AuthorizationAction::ManagePlatform,
    )?;
    super::tenant_nodes::token_command(
        &state,
        scope,
        id,
        body,
        super::tenant_nodes::platform_audit(&auth, request_id),
    )
    .await
}
