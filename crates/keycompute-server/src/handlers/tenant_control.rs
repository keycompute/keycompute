//! Phase 3/4 tenant control-plane HTTP handlers.
//!
//! The handlers keep route identity, current console identity, database
//! authority, and mutation versioning separate. The database methods are the
//! final authority check; the extractors are defense in depth for bare mounts.

use crate::{
    error::{ApiError, Result},
    extractors::{GlobalConsoleAuth, RequestId},
    handlers::pagination::{normalize_list_pagination, total_pages},
    state::AppState,
    tenant_access::{TenantAdmin, TenantMember as TenantMemberAccess},
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use keycompute_auth::AuthorizationAction;
use keycompute_db::{
    AuditContext,
    models::tenant_control::{
        self, MAX_INVITATION_TTL_SECONDS, MAX_TENANT_CONTROL_PAGE_SIZE, MIN_INVITATION_TTL_SECONDS,
        MemberPatch, Page, TenantAuthzSnapshot, TenantContext, TenantMember as TenantMemberRecord,
        TenantPatch,
    },
};
use keycompute_types::{MembershipStatus, TenantRole};
use sea_orm::{DatabaseTransaction, TransactionTrait};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use url::Url;
use uuid::Uuid;

pub const TENANT_CONTROL_BODY_LIMIT_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
pub struct TenantPath {
    pub tenant_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct MemberPath {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct InvitationPath {
    pub tenant_id: Uuid,
    pub id: Uuid,
}

#[derive(Deserialize)]
pub struct AcceptInvitationPath {
    pub token: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantPatchRequest {
    pub expected_authz_version: i64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub default_rpm_limit: Option<i32>,
    pub default_tpm_limit: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberPatchRequest {
    pub expected_authz_version: i64,
    pub tenant_role: Option<TenantRole>,
    pub status: Option<MembershipStatus>,
}

#[derive(Debug, Deserialize)]
pub struct MemberListQuery {
    pub search: Option<String>,
    pub status: Option<MembershipStatus>,
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}

#[derive(Debug, Deserialize)]
pub struct InvitationListQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateInvitationRequest {
    pub email: String,
    pub tenant_role: TenantRole,
    pub expires_in_seconds: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferOwnershipRequest {
    pub new_owner_user_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct AuditListQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}

#[derive(Debug, Serialize)]
pub struct TenantPage<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

#[derive(Serialize)]
pub struct InvitationCreateResponse {
    pub invitation: tenant_control::TenantInvitationView,
    pub outcome: InvitationOutcome,
    pub notification: NotificationStatus,
    /// Present only for a newly created invitation. This is the only endpoint
    /// response that can contain a one-time recovery link; it contains the
    /// token in a frontend fragment, not an API path.
    pub acceptance_link: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InvitationOutcome {
    Created,
    AlreadyPending,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationStatus {
    Sent,
    Failed,
    Unconfigured,
    NotApplicable,
}

#[derive(Debug, Serialize)]
pub struct AcceptInvitationResponse {
    pub tenant: TenantContext,
    pub membership: TenantMemberRecord,
}

fn default_page() -> i64 {
    1
}

fn default_page_size() -> i64 {
    20
}

fn page(page: i64, page_size: i64) -> Page {
    Page::bounded(page, page_size.min(MAX_TENANT_CONTROL_PAGE_SIZE))
}

fn pool(state: &AppState) -> Result<&keycompute_db::DbRouter> {
    state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))
}

fn db_error(error: keycompute_db::DbError) -> ApiError {
    use keycompute_db::DbError;
    match error {
        DbError::NotFound { .. } => ApiError::NotFound("tenant resource not found".into()),
        DbError::OptimisticConflict { .. } => {
            ApiError::Conflict("the resource changed; refresh and retry".into())
        }
        DbError::DuplicateKey { .. } => {
            ApiError::Conflict("the requested tenant state already exists".into())
        }
        DbError::Other(message) => {
            let lower = message.to_ascii_lowercase();
            if lower.contains("conflict")
                || lower.contains("last ")
                || lower.contains("must retain")
                || lower.contains("removed membership")
                || lower.contains("pending invitation")
                || lower.contains("active tenant member cannot receive")
                || lower.contains("different role")
                || lower.contains("already used")
                || lower.contains("unavailable")
                || lower.contains("tenant owner")
            {
                ApiError::Conflict(message)
            } else if lower.contains("expected_authz_version")
                || lower.contains("at least one")
                || lower.contains("invalid")
            {
                ApiError::BadRequest(message)
            } else if lower.contains("required")
                || lower.contains("permission")
                || lower.contains("administrator")
                || lower.contains("owner")
            {
                ApiError::Forbidden(message)
            } else {
                ApiError::Internal(message)
            }
        }
        DbError::DatabaseError(error) => {
            let message = error.to_string();
            let lower = message.to_ascii_lowercase();
            if lower.contains("last admin")
                || lower.contains("active admin")
                || lower.contains("owner")
                || lower.contains("constraint")
            {
                ApiError::Conflict("tenant membership invariant rejected the change".into())
            } else {
                tracing::error!(error = %error, "tenant control database error");
                ApiError::ServiceUnavailable(
                    "tenant control storage is temporarily unavailable".into(),
                )
            }
        }
        other => {
            tracing::error!(error = %other, "tenant control database error");
            ApiError::ServiceUnavailable("tenant control storage is temporarily unavailable".into())
        }
    }
}

fn audit_from_request(admin: &TenantAdmin, request_id: RequestId) -> AuditContext {
    admin.audit(request_id)
}

fn audit_from_global(auth: &GlobalConsoleAuth, request_id: RequestId) -> AuditContext {
    AuditContext {
        actor_user_id: auth.user_id,
        credential_kind: auth.credential_kind,
        actor_platform_role: auth.platform_role,
        actor_tenant_role: auth.tenant_role,
        request_id: Some(request_id.0),
    }
}

fn snapshot(access: &TenantAdmin) -> TenantAuthzSnapshot {
    TenantAuthzSnapshot {
        token_version: access.auth().token_version,
        tenant_authz_version: access.auth().authz_version,
        membership_authz_version: access.auth().membership_authz_version,
    }
}

async fn rollback<T>(tx: DatabaseTransaction, error: ApiError) -> Result<T> {
    if let Err(rollback_error) = tx.rollback().await {
        tracing::error!(error = %rollback_error, "tenant control transaction rollback failed");
    }
    Err(error)
}

/// Fence caches only for an authorized mutation ready to commit. Rejected
/// requests cannot evict snapshots; uncertain/cancelled commits remain fenced.
async fn commit_mutation(tx: DatabaseTransaction, state: &AppState) -> Result<()> {
    let _mutation = state.display_cache.mutation_guard();
    tx.commit().await.map_err(|error| db_error(error.into()))
}

pub async fn get_tenant(
    access: TenantMemberAccess,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
) -> Result<Json<TenantContext>> {
    access.require_path_tenant(path.tenant_id)?;
    let scope = access.scope();
    let context = tenant_control::get_tenant_context(pool(&state)?.write_conn(), scope)
        .await
        .map_err(db_error)?
        .ok_or_else(|| ApiError::NotFound("tenant not found".into()))?;
    Ok(Json(context))
}

pub async fn patch_tenant(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    request_id: RequestId,
    Json(request): Json<TenantPatchRequest>,
) -> Result<Json<TenantContext>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageTenantResource)?;
    let patch = TenantPatch {
        expected_authz_version: request.expected_authz_version,
        name: request.name,
        description: request.description,
        default_rpm_limit: request.default_rpm_limit,
        default_tpm_limit: request.default_tpm_limit,
    };
    let tx = pool(&state)?
        .begin()
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let result = match tenant_control::update_tenant(
        &tx,
        access.scope(),
        snapshot(&access),
        &patch,
        &audit_from_request(&access, request_id),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return rollback(tx, db_error(error)).await,
    };
    commit_mutation(tx, &state).await?;
    Ok(Json(result))
}

pub async fn list_members(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(query): Query<MemberListQuery>,
) -> Result<Json<TenantPage<TenantMemberRecord>>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageMembers)?;
    let scope = access.scope();
    let (page_number, page_size, _) =
        normalize_list_pagination(Some(query.page), Some(query.page_size), None, None);
    let page = page(page_number, page_size);
    let db = pool(&state)?;
    let items = tenant_control::list_members(
        db.write_conn(),
        scope,
        query.search.as_deref(),
        query.status,
        page,
    )
    .await
    .map_err(db_error)?;
    let total = tenant_control::count_members(
        db.write_conn(),
        scope,
        query.search.as_deref(),
        query.status,
    )
    .await
    .map_err(db_error)?;
    Ok(Json(TenantPage {
        items,
        total,
        page: page.page,
        page_size: page.page_size,
        total_pages: total_pages(total, page.page_size),
    }))
}

pub async fn get_member(
    access: TenantAdmin,
    Path(path): Path<MemberPath>,
    State(state): State<AppState>,
) -> Result<Json<TenantMemberRecord>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageMembers)?;
    tenant_control::get_member(pool(&state)?.write_conn(), access.scope(), path.user_id)
        .await
        .map_err(db_error)?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound("tenant member not found".into()))
}

pub async fn patch_member(
    access: TenantAdmin,
    Path(path): Path<MemberPath>,
    State(state): State<AppState>,
    request_id: RequestId,
    Json(request): Json<MemberPatchRequest>,
) -> Result<Json<TenantMemberRecord>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageMembers)?;
    let patch = MemberPatch {
        expected_authz_version: request.expected_authz_version,
        tenant_role: request.tenant_role,
        status: request.status,
    };
    let tx = pool(&state)?
        .begin()
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let result = match tenant_control::update_member(
        &tx,
        access.scope(),
        snapshot(&access),
        path.user_id,
        &patch,
        &audit_from_request(&access, request_id),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return rollback(tx, db_error(error)).await,
    };
    commit_mutation(tx, &state).await?;
    Ok(Json(result))
}

pub async fn delete_member(
    access: TenantAdmin,
    Path(path): Path<MemberPath>,
    State(state): State<AppState>,
    request_id: RequestId,
    Json(request): Json<ExpectedVersionRequest>,
) -> Result<Json<TenantMemberRecord>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageMembers)?;
    let tx = pool(&state)?
        .begin()
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let result = match tenant_control::remove_member(
        &tx,
        access.scope(),
        snapshot(&access),
        path.user_id,
        request.expected_authz_version,
        &audit_from_request(&access, request_id),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return rollback(tx, db_error(error)).await,
    };
    commit_mutation(tx, &state).await?;
    Ok(Json(result))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedVersionRequest {
    pub expected_authz_version: i64,
}

pub async fn list_invitations(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(query): Query<InvitationListQuery>,
) -> Result<Json<TenantPage<tenant_control::TenantInvitationView>>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageMembers)?;
    let (page_number, page_size, _) =
        normalize_list_pagination(Some(query.page), Some(query.page_size), None, None);
    let page = page(page_number, page_size);
    let db = pool(&state)?;
    let scope = access.scope();
    let items = tenant_control::list_invitations(db.write_conn(), scope, page)
        .await
        .map_err(db_error)?;
    let total = tenant_control::count_invitations(db.write_conn(), scope)
        .await
        .map_err(db_error)?;
    Ok(Json(TenantPage {
        total,
        total_pages: total_pages(total, page.page_size),
        page: page.page,
        page_size: page.page_size,
        items,
    }))
}

pub async fn create_invitation(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    request_id: RequestId,
    Json(request): Json<CreateInvitationRequest>,
) -> Result<(StatusCode, Json<InvitationCreateResponse>)> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::InviteMembers)?;
    if !(MIN_INVITATION_TTL_SECONDS..=MAX_INVITATION_TTL_SECONDS)
        .contains(&request.expires_in_seconds)
    {
        return Err(ApiError::BadRequest(format!(
            "expires_in_seconds must be between {MIN_INVITATION_TTL_SECONDS} and {MAX_INVITATION_TTL_SECONDS}"
        )));
    }
    let db = pool(&state)?;
    let tx = db
        .begin()
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let tenant = match tenant_control::get_tenant_context(&tx, access.scope()).await {
        Ok(Some(tenant)) => tenant,
        Ok(None) => return rollback(tx, ApiError::NotFound("tenant not found".into())).await,
        Err(error) => return rollback(tx, db_error(error)).await,
    };
    let created = match tenant_control::create_invitation(
        &tx,
        access.scope(),
        snapshot(&access),
        &request.email,
        request.tenant_role,
        request.expires_in_seconds,
        &audit_from_request(&access, request_id),
    )
    .await
    {
        Ok(created) => created,
        Err(error) => return rollback(tx, db_error(error)).await,
    };
    let was_created = created.token.is_some();
    let token = created.token.clone();
    let invitation = tenant_control::invitation_view(created.invitation);
    commit_mutation(tx, &state).await?;

    let acceptance_link = token
        .as_deref()
        .and_then(|token| invitation_link(state.app_base_url.as_deref(), token));
    let notification = if !was_created {
        NotificationStatus::NotApplicable
    } else if state
        .app_base_url
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
        || !state.email_service.is_configured().await
    {
        NotificationStatus::Unconfigured
    } else {
        match acceptance_link.as_deref() {
            None => NotificationStatus::Unconfigured,
            Some(link) => {
                let (text, html) = invitation_email(&tenant.name, link);
                match state
                    .email_service
                    .send_html_email(
                        &invitation.email,
                        "You have been invited to a KeyCompute tenant",
                        &text,
                        &html,
                    )
                    .await
                {
                    Ok(()) => NotificationStatus::Sent,
                    Err(error) => {
                        tracing::warn!(
                            invitation_id = %invitation.id,
                            error_kind = ?std::mem::discriminant(&error),
                            "tenant invitation notification failed after commit"
                        );
                        NotificationStatus::Failed
                    }
                }
            }
        }
    };
    Ok((
        if was_created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(InvitationCreateResponse {
            invitation,
            outcome: if was_created {
                InvitationOutcome::Created
            } else {
                InvitationOutcome::AlreadyPending
            },
            notification,
            acceptance_link,
        }),
    ))
}

pub async fn revoke_invitation(
    access: TenantAdmin,
    Path(path): Path<InvitationPath>,
    State(state): State<AppState>,
    request_id: RequestId,
) -> Result<Json<tenant_control::TenantInvitationView>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageMembers)?;
    let tx = pool(&state)?
        .begin()
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let result = match tenant_control::revoke_invitation(
        &tx,
        access.scope(),
        snapshot(&access),
        path.id,
        &audit_from_request(&access, request_id),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return rollback(tx, db_error(error)).await,
    };
    commit_mutation(tx, &state).await?;
    Ok(Json(result))
}

pub async fn accept_invitation(
    auth: GlobalConsoleAuth,
    Path(path): Path<AcceptInvitationPath>,
    State(state): State<AppState>,
    request_id: RequestId,
) -> Result<Json<AcceptInvitationResponse>> {
    if path.token.len() != 64 || !path.token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ApiError::NotFound("invitation not found".into()));
    }
    let tx = pool(&state)?
        .begin()
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let (invitation, member) = match tenant_control::accept_invitation(
        &tx,
        auth.user_id,
        auth.token_version,
        &path.token,
        &audit_from_global(&auth, request_id),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            let api_error = match error {
                keycompute_db::DbError::Other(message)
                    if message.contains("unavailable") || message.contains("expired") =>
                {
                    ApiError::NotFound("invitation not found".into())
                }
                other => db_error(other),
            };
            return rollback(tx, api_error).await;
        }
    };
    let tenant_role = member
        .tenant_role
        .parse()
        .map_err(|_| ApiError::Internal("accepted role is invalid".into()))?;
    let tenant = match tenant_control::get_tenant_context(
        &tx,
        keycompute_types::TenantScope::checked(invitation.tenant_id, auth.user_id, tenant_role)
            .map_err(ApiError::Internal)?,
    )
    .await
    {
        Ok(Some(tenant)) => tenant,
        Ok(None) => {
            return rollback(
                tx,
                ApiError::Internal("accepted tenant context disappeared".into()),
            )
            .await;
        }
        Err(error) => return rollback(tx, db_error(error)).await,
    };
    commit_mutation(tx, &state).await?;
    Ok(Json(AcceptInvitationResponse {
        tenant,
        membership: member,
    }))
}

pub async fn transfer_ownership(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    request_id: RequestId,
    Json(request): Json<TransferOwnershipRequest>,
) -> Result<Json<TenantContext>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageTenantResource)?;
    let tx = pool(&state)?
        .begin()
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let result = match tenant_control::transfer_ownership(
        &tx,
        access.scope(),
        snapshot(&access),
        request.new_owner_user_id,
        &audit_from_request(&access, request_id),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return rollback(tx, db_error(error)).await,
    };
    commit_mutation(tx, &state).await?;
    Ok(Json(result))
}

pub async fn list_audit_events(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(query): Query<AuditListQuery>,
) -> Result<Json<TenantPage<tenant_control::TenantAuditView>>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageMembers)?;
    let (page_number, page_size, _) =
        normalize_list_pagination(Some(query.page), Some(query.page_size), None, None);
    let page = page(page_number, page_size);
    let items = tenant_control::list_audit_events(pool(&state)?.write_conn(), access.scope(), page)
        .await
        .map_err(db_error)?;
    let total = tenant_control::count_audit_events(pool(&state)?.write_conn(), access.scope())
        .await
        .map_err(db_error)?;
    Ok(Json(TenantPage {
        total,
        total_pages: total_pages(total, page.page_size),
        page: page.page,
        page_size: page.page_size,
        items,
    }))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/tenants/{tenant_id}",
            get(get_tenant).patch(patch_tenant),
        )
        .route("/api/v1/tenants/{tenant_id}/members", get(list_members))
        .route(
            "/api/v1/tenants/{tenant_id}/members/{user_id}",
            get(get_member).patch(patch_member).delete(delete_member),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/invitations",
            get(list_invitations).post(create_invitation),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/invitations/{id}/revoke",
            post(revoke_invitation),
        )
        .route(
            "/api/v1/invitations/{token}/accept",
            post(accept_invitation),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/transfer-ownership",
            post(transfer_ownership),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/audit-events",
            get(list_audit_events),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            TENANT_CONTROL_BODY_LIMIT_BYTES,
        ))
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false),
        None => false,
    }
}

fn validated_app_base_url(value: &str) -> Option<Url> {
    let parsed = Url::parse(value.trim()).ok()?;
    if parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    match parsed.scheme() {
        "https" => Some(parsed),
        "http" if is_loopback_host(&parsed) => Some(parsed),
        _ => None,
    }
}

fn invitation_link(app_base_url: Option<&str>, token: &str) -> Option<String> {
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut base = validated_app_base_url(app_base_url?)?;
    let path = format!("{}/invite", base.path().trim_end_matches('/'));
    base.set_path(&path);
    base.set_fragment(Some(&format!("token={token}")));
    Some(base.into())
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn invitation_email(tenant_name: &str, link: &str) -> (String, String) {
    let tenant_name = tenant_name.trim();
    let safe_tenant_name = escape_html(tenant_name);
    let safe_link = escape_html(link);
    (
        format!(
            "You have been invited to join {tenant_name} on KeyCompute.\n\nAccept the invitation:\n{link}\n\nThis link is single-use and expires according to the invitation."
        ),
        format!(
            "<html><body><p>You have been invited to join <strong>{safe_tenant_name}</strong> on KeyCompute.</p><p><a href=\"{safe_link}\">Accept invitation</a></p><p>This link is single-use and expires according to the invitation.</p></body></html>"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitation_link_keeps_token_in_fragment() {
        let link = invitation_link(Some("https://console.example/"), &"a".repeat(64)).unwrap();
        assert_eq!(
            link,
            format!("https://console.example/invite#token={}", "a".repeat(64))
        );
        assert!(!link.contains("/accept"));
    }

    #[test]
    fn invitation_email_escapes_html_inputs() {
        let (_, html) = invitation_email("<tenant>", "https://console.test/invite#token=x");
        assert!(html.contains("&lt;tenant&gt;"));
        assert!(!html.contains("<tenant>"));
    }

    #[test]
    fn unknown_patch_fields_are_rejected() {
        assert!(
            serde_json::from_value::<MemberPatchRequest>(serde_json::json!({
                "expected_authz_version": 1,
                "platform_role": "root"
            }))
            .is_err()
        );
    }
}
