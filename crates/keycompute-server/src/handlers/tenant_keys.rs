//! Tenant API-key metadata administration and owner-only provisioning.
//!
//! Tenant administrators can manage metadata and revoke/remove keys in their
//! tenant, but they never receive another user's raw key.  A requested
//! creation or rotation is an inert issuance intent until the key owner claims
//! it through the `/me/key-issuance` routes.

use crate::{
    error::{ApiError, Result},
    extractors::{ConsoleAuth, RequestId},
    handlers::pagination::total_pages,
    state::AppState,
    tenant_access::TenantAdmin,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use keycompute_auth::AuthorizationAction;
use keycompute_db::{
    ProduceAiKey, ProduceAiKeyResponse,
    models::{
        api_key::KeyRemoval,
        key_issuance::{
            self, ClaimedKey, KeyIssuanceIntent, KeyIssuancePage, KeyMetadataPatch,
            MAX_KEY_ISSUANCE_PAGE_SIZE,
        },
        tenant_control::TenantAuthzSnapshot,
    },
};
use keycompute_types::TenantScope;
use sea_orm::{DatabaseTransaction, TransactionTrait};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const TENANT_KEYS_BODY_LIMIT_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
pub struct TenantPath {
    pub tenant_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct TenantKeyPath {
    pub tenant_id: Uuid,
    pub id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct OwnerIntentPath {
    pub id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantKeyListQuery {
    pub owner_user_id: Option<Uuid>,
    #[serde(default)]
    pub include_revoked: bool,
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssuanceListQuery {
    pub owner_user_id: Option<Uuid>,
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestIssuance {
    pub owner_user_id: Uuid,
    pub name: String,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestRotation {
    pub name: String,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchTenantKey {
    pub expected_updated_at: DateTime<Utc>,
    pub name: Option<String>,
    /// Omitted preserves the existing expiration; JSON `null` clears it.
    #[serde(default, deserialize_with = "nullable_field")]
    pub expires_at: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Serialize)]
pub struct TenantKeyMetadata {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub owner_user_id: Uuid,
    pub name: String,
    pub key_preview: String,
    pub revoked: bool,
    pub revoked_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct TenantKeyPage {
    pub keys: Vec<TenantKeyMetadata>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

#[derive(Debug, Serialize)]
pub struct IssuancePage {
    pub intents: Vec<KeyIssuanceIntent>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

#[derive(Debug, Serialize)]
pub struct IssuanceResponse {
    pub intent: KeyIssuanceIntent,
    pub outcome: &'static str,
    pub message: &'static str,
}

#[derive(Serialize)]
pub struct ClaimResponse {
    pub outcome: &'static str,
    pub message: &'static str,
    pub intent_id: Uuid,
    pub key_id: Uuid,
    pub name: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// The only response field that can contain the raw credential. It is
    /// intentionally not repeated by any metadata endpoint or audit record.
    pub key: String,
    pub secret_returned_once: bool,
}

#[derive(Debug, Serialize)]
pub struct KeyMutationResponse {
    pub success: bool,
    pub key: Option<TenantKeyMetadata>,
    pub key_id: Uuid,
    pub revoked_at: Option<DateTime<Utc>>,
    pub deleted: bool,
}

fn default_page() -> i64 {
    1
}

fn default_page_size() -> i64 {
    20
}

fn pool(state: &AppState) -> Result<&keycompute_db::DbRouter> {
    state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Tenant key storage unavailable".into()))
}

// Actual key mutations retain their validated locks until the display cache
// is fenced and the outer transaction commits. Denied operations never flush
// cached snapshots; uncertain commit results remain conservatively fenced.
async fn key_mutation_transaction(state: &AppState) -> Result<DatabaseTransaction> {
    pool(state)?
        .begin()
        .await
        .map_err(keycompute_db::DbError::from)
        .map_err(map_db_error)
}
async fn commit_key_mutation(state: &AppState, tx: DatabaseTransaction) -> Result<()> {
    let _fence = state.display_cache.mutation_guard();
    tx.commit()
        .await
        .map_err(keycompute_db::DbError::from)
        .map_err(map_db_error)
}

fn snapshot(auth: &ConsoleAuth) -> TenantAuthzSnapshot {
    TenantAuthzSnapshot {
        token_version: auth.token_version,
        tenant_authz_version: auth.authz_version,
        membership_authz_version: auth.membership_authz_version,
    }
}

fn page(page: i64, page_size: i64) -> KeyIssuancePage {
    KeyIssuancePage::bounded(page, page_size.clamp(1, MAX_KEY_ISSUANCE_PAGE_SIZE))
}

fn key_metadata(key: ProduceAiKeyResponse) -> TenantKeyMetadata {
    TenantKeyMetadata {
        id: key.id,
        tenant_id: key.tenant_id,
        owner_user_id: key.user_id,
        name: key.name,
        key_preview: key.produce_ai_key_preview,
        revoked: key.revoked,
        revoked_at: key.revoked_at,
        expires_at: key.expires_at,
        last_used_at: key.last_used_at,
        created_at: key.created_at,
        updated_at: key.updated_at,
    }
}

fn key_metadata_from_removal(removal: &KeyRemoval) -> Option<TenantKeyMetadata> {
    match removal {
        KeyRemoval::Revoked(key) => Some(key_metadata(key.clone())),
        KeyRemoval::Deleted(_) => None,
    }
}

fn map_db_error(error: keycompute_db::DbError) -> ApiError {
    use keycompute_db::DbError;
    match error {
        DbError::NotFound { .. } => ApiError::NotFound("tenant key resource not found".into()),
        DbError::OptimisticConflict { .. } => {
            ApiError::Conflict("the key changed; refresh and retry".into())
        }
        DbError::DuplicateKey { .. } => ApiError::Conflict("key issuance request conflicts".into()),
        DbError::Other(message) => {
            let lower = message.to_ascii_lowercase();
            if lower.contains("conflict")
                || lower.contains("already claimed")
                || lower.contains("already pending")
                || lower.contains("no longer pending")
                || lower.contains("revoked or expired")
                || lower.contains("changed")
            {
                ApiError::Conflict(message)
            } else if lower.contains("administrator")
                || lower.contains("authority")
                || lower.contains("owner")
                || lower.contains("membership")
                || lower.contains("permission")
            {
                ApiError::Forbidden(message)
            } else if lower.contains("invalid")
                || lower.contains("required")
                || lower.contains("expiration")
                || lower.contains("name")
            {
                ApiError::BadRequest(message)
            } else {
                ApiError::Internal(message)
            }
        }
        DbError::DatabaseError(error) => {
            tracing::error!(error = %error, "tenant key database error");
            ApiError::ServiceUnavailable("tenant key storage is temporarily unavailable".into())
        }
        other => {
            tracing::error!(error = %other, "tenant key database error");
            ApiError::ServiceUnavailable("tenant key storage is temporarily unavailable".into())
        }
    }
}

fn ensure_owner_filter(owner_user_id: Option<Uuid>) -> Result<Option<Uuid>> {
    if owner_user_id.is_some_and(|id| id.is_nil()) {
        return Err(ApiError::BadRequest(
            "owner_user_id must be a real UUID".into(),
        ));
    }
    Ok(owner_user_id)
}

fn audit(auth: &TenantAdmin, request_id: RequestId) -> keycompute_db::AuditContext {
    auth.audit(request_id)
}

fn self_scope(auth: &ConsoleAuth) -> Result<TenantScope> {
    auth.require_owner(auth.user_id, AuthorizationAction::ReadPersonalResource)
}

fn self_mutation_scope(auth: &ConsoleAuth) -> Result<TenantScope> {
    auth.require_owner(auth.user_id, AuthorizationAction::ManagePersonalResource)
}

pub async fn list_tenant_keys(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(query): Query<TenantKeyListQuery>,
) -> Result<Json<TenantKeyPage>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageTenantResource)?;
    let owner = ensure_owner_filter(query.owner_user_id)?;
    let p = page(query.page, query.page_size);
    let (page_number, page_size, offset) = (p.page, p.page_size, p.offset);
    let scope = access.scope();
    let db = pool(&state)?;
    let keys = ProduceAiKey::list_in_tenant(
        db.write_conn(),
        scope,
        owner,
        query.include_revoked,
        page_size,
        offset,
    )
    .await
    .map_err(map_db_error)?;
    let total = ProduceAiKey::count_in_tenant(db.write_conn(), scope, owner, query.include_revoked)
        .await
        .map_err(map_db_error)?;
    Ok(Json(TenantKeyPage {
        keys: keys.into_iter().map(key_metadata).collect(),
        total,
        page: page_number,
        page_size,
        total_pages: total_pages(total, page_size),
    }))
}

pub async fn get_tenant_key(
    access: TenantAdmin,
    Path(path): Path<TenantKeyPath>,
    State(state): State<AppState>,
) -> Result<Json<TenantKeyMetadata>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageTenantResource)?;
    let key = ProduceAiKey::find_in_tenant(pool(&state)?.write_conn(), access.scope(), path.id)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| ApiError::NotFound("tenant key resource not found".into()))?;
    Ok(Json(key_metadata(key)))
}

pub async fn patch_tenant_key(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<TenantKeyPath>,
    State(state): State<AppState>,
    Json(request): Json<PatchTenantKey>,
) -> Result<Json<TenantKeyMetadata>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageTenantResource)?;
    if request.name.is_none() && request.expires_at.is_none() {
        return Err(ApiError::BadRequest(
            "name or expires_at is required".into(),
        ));
    }
    let patch = KeyMetadataPatch {
        expected_updated_at: request.expected_updated_at,
        name: request.name,
        expires_at: request.expires_at,
    };
    let tx = key_mutation_transaction(&state).await?;
    let updated = key_issuance::update_key(
        &tx,
        access.scope(),
        snapshot(access.auth()),
        path.id,
        &patch,
        &audit(&access, request_id),
    )
    .await
    .map_err(map_db_error)?;
    commit_key_mutation(&state, tx).await?;
    Ok(Json(key_metadata(updated)))
}

pub async fn revoke_tenant_key(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<TenantKeyPath>,
    State(state): State<AppState>,
) -> Result<Json<KeyMutationResponse>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageTenantResource)?;
    let tx = key_mutation_transaction(&state).await?;
    let result = key_issuance::remove_key(
        &tx,
        access.scope(),
        snapshot(access.auth()),
        path.id,
        &audit(&access, request_id),
        true,
    )
    .await
    .map_err(map_db_error)?;
    commit_key_mutation(&state, tx).await?;
    Ok(Json(KeyMutationResponse {
        success: true,
        key: key_metadata_from_removal(&result),
        key_id: match &result {
            KeyRemoval::Revoked(key) => key.id,
            KeyRemoval::Deleted(id) => *id,
        },
        revoked_at: match &result {
            KeyRemoval::Revoked(key) => key.revoked_at,
            KeyRemoval::Deleted(_) => None,
        },
        deleted: matches!(result, KeyRemoval::Deleted(_)),
    }))
}

pub async fn delete_tenant_key(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<TenantKeyPath>,
    State(state): State<AppState>,
) -> Result<Json<KeyMutationResponse>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageTenantResource)?;
    let tx = key_mutation_transaction(&state).await?;
    let result = key_issuance::remove_key(
        &tx,
        access.scope(),
        snapshot(access.auth()),
        path.id,
        &audit(&access, request_id),
        false,
    )
    .await
    .map_err(map_db_error)?;
    commit_key_mutation(&state, tx).await?;
    Ok(Json(KeyMutationResponse {
        success: true,
        key: key_metadata_from_removal(&result),
        key_id: match &result {
            KeyRemoval::Revoked(key) => key.id,
            KeyRemoval::Deleted(id) => *id,
        },
        revoked_at: match &result {
            KeyRemoval::Revoked(key) => key.revoked_at,
            KeyRemoval::Deleted(_) => None,
        },
        deleted: matches!(result, KeyRemoval::Deleted(_)),
    }))
}

fn issuance_response(intent: KeyIssuanceIntent, already_pending: bool) -> IssuanceResponse {
    IssuanceResponse {
        intent,
        outcome: if already_pending {
            "already_pending"
        } else {
            "created"
        },
        message: "awaiting_owner_claim",
    }
}

pub async fn request_tenant_key(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Json(request): Json<RequestIssuance>,
) -> Result<(StatusCode, Json<IssuanceResponse>)> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageTenantResource)?;
    let (intent, already_pending) = key_issuance::request_new(
        pool(&state)?.write_conn(),
        access.scope(),
        snapshot(access.auth()),
        request.owner_user_id,
        &request.name,
        request.expires_at,
        &audit(&access, request_id),
    )
    .await
    .map_err(map_db_error)?;
    Ok((
        if already_pending {
            StatusCode::OK
        } else {
            StatusCode::ACCEPTED
        },
        Json(issuance_response(intent, already_pending)),
    ))
}

pub async fn rotate_tenant_key(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<TenantKeyPath>,
    State(state): State<AppState>,
    Json(request): Json<RequestRotation>,
) -> Result<(StatusCode, Json<IssuanceResponse>)> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageTenantResource)?;
    let (intent, already_pending) = key_issuance::request_rotation(
        pool(&state)?.write_conn(),
        access.scope(),
        snapshot(access.auth()),
        path.id,
        &request.name,
        request.expires_at,
        &audit(&access, request_id),
    )
    .await
    .map_err(map_db_error)?;
    Ok((
        if already_pending {
            StatusCode::OK
        } else {
            StatusCode::ACCEPTED
        },
        Json(issuance_response(intent, already_pending)),
    ))
}

pub async fn list_tenant_key_issuances(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(query): Query<IssuanceListQuery>,
) -> Result<Json<IssuancePage>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageTenantResource)?;
    let owner = ensure_owner_filter(query.owner_user_id)?;
    let page = page(query.page, query.page_size);
    let db = pool(&state)?;
    let intents = key_issuance::list_in_tenant(db.write_conn(), access.scope(), owner, page)
        .await
        .map_err(map_db_error)?;
    let total = key_issuance::count_in_tenant(db.write_conn(), access.scope(), owner)
        .await
        .map_err(map_db_error)?;
    Ok(Json(IssuancePage {
        intents,
        total,
        page: page.page,
        page_size: page.page_size,
        total_pages: total_pages(total, page.page_size),
    }))
}

pub async fn cancel_tenant_key_issuance(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<TenantKeyPath>,
    State(state): State<AppState>,
) -> Result<Json<IssuanceResponse>> {
    access.require_path_tenant(path.tenant_id)?;
    access.require(AuthorizationAction::ManageTenantResource)?;
    let intent = key_issuance::cancel_in_tenant(
        pool(&state)?.write_conn(),
        access.scope(),
        snapshot(access.auth()),
        path.id,
        &audit(&access, request_id),
    )
    .await
    .map_err(map_db_error)?;
    Ok(Json(IssuanceResponse {
        intent,
        outcome: "cancelled",
        message: "owner_claim_denied",
    }))
}

pub async fn list_my_key_issuances(
    auth: ConsoleAuth,
    State(state): State<AppState>,
    Query(query): Query<OwnerIssuanceListQuery>,
) -> Result<Json<IssuancePage>> {
    let scope = self_scope(&auth)?;
    let page = page(query.page, query.page_size);
    let db = pool(&state)?;
    let intents = key_issuance::list_for_owner(db.write_conn(), scope, page)
        .await
        .map_err(map_db_error)?;
    let total = key_issuance::count_for_owner(db.write_conn(), scope)
        .await
        .map_err(map_db_error)?;
    Ok(Json(IssuancePage {
        intents,
        total,
        page: page.page,
        page_size: page.page_size,
        total_pages: total_pages(total, page.page_size),
    }))
}

fn claim_response(claimed: ClaimedKey) -> ClaimResponse {
    ClaimResponse {
        outcome: "claimed",
        message: "the raw key is returned exactly once; it cannot be recovered",
        intent_id: claimed.intent.id,
        key_id: claimed.key.id,
        name: claimed.key.name,
        expires_at: claimed.key.expires_at,
        created_at: claimed.key.created_at,
        key: claimed.secret,
        secret_returned_once: true,
    }
}

pub async fn claim_my_key_issuance(
    auth: ConsoleAuth,
    request_id: RequestId,
    Path(path): Path<OwnerIntentPath>,
    State(state): State<AppState>,
) -> Result<Response> {
    let scope = self_mutation_scope(&auth)?;
    let tx = key_mutation_transaction(&state).await?;
    let claimed = key_issuance::claim(
        &tx,
        scope,
        snapshot(&auth),
        path.id,
        &keycompute_db::AuditContext {
            actor_user_id: auth.user_id,
            credential_kind: auth.credential_kind,
            actor_platform_role: auth.platform_role,
            actor_tenant_role: auth.tenant_role,
            request_id: Some(request_id.0),
        },
    )
    .await
    .map_err(map_db_error)?;
    commit_key_mutation(&state, tx).await?;
    Ok((
        [
            (header::CACHE_CONTROL, "private, no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        Json(claim_response(claimed)),
    )
        .into_response())
}

pub async fn decline_my_key_issuance(
    auth: ConsoleAuth,
    request_id: RequestId,
    Path(path): Path<OwnerIntentPath>,
    State(state): State<AppState>,
) -> Result<Json<IssuanceResponse>> {
    let scope = self_mutation_scope(&auth)?;
    let intent = key_issuance::decline_for_owner(
        pool(&state)?.write_conn(),
        scope,
        snapshot(&auth),
        path.id,
        &keycompute_db::AuditContext {
            actor_user_id: auth.user_id,
            credential_kind: auth.credential_kind,
            actor_platform_role: auth.platform_role,
            actor_tenant_role: auth.tenant_role,
            request_id: Some(request_id.0),
        },
    )
    .await
    .map_err(map_db_error)?;
    Ok(Json(IssuanceResponse {
        intent,
        outcome: "declined",
        message: "owner_declined",
    }))
}

fn nullable_field<'de, D, T>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerIssuanceListQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}

pub fn router() -> axum::Router<AppState> {
    use axum::{
        Router,
        routing::{get, post},
    };
    Router::new()
        .route("/api/v1/tenants/{tenant_id}/keys", get(list_tenant_keys))
        .route(
            "/api/v1/tenants/{tenant_id}/keys/{id}",
            get(get_tenant_key)
                .patch(patch_tenant_key)
                .delete(delete_tenant_key),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/keys/{id}/revoke",
            post(revoke_tenant_key),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/keys/issuance",
            post(request_tenant_key),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/keys/{id}/rotate",
            post(rotate_tenant_key),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/key-issuance",
            get(list_tenant_key_issuances),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/key-issuance/{id}/cancel",
            post(cancel_tenant_key_issuance),
        )
        .route("/api/v1/me/key-issuance", get(list_my_key_issuances))
        .route(
            "/api/v1/me/key-issuance/{id}/claim",
            post(claim_my_key_issuance),
        )
        .route(
            "/api/v1/me/key-issuance/{id}/decline",
            post(decline_my_key_issuance),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            TENANT_KEYS_BODY_LIMIT_BYTES,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_expiration_patch_distinguishes_omitted_null_and_value() {
        let base = serde_json::json!({"expected_updated_at":"2026-01-01T00:00:00Z","name":"same"});
        let omitted: PatchTenantKey = serde_json::from_value(base.clone()).unwrap();
        assert!(omitted.expires_at.is_none());
        let mut clear = base.clone();
        clear["expires_at"] = serde_json::Value::Null;
        let clear: PatchTenantKey = serde_json::from_value(clear).unwrap();
        assert_eq!(clear.expires_at, Some(None));
        let mut set = base;
        set["expires_at"] = serde_json::json!("2030-01-01T00:00:00Z");
        let set: PatchTenantKey = serde_json::from_value(set).unwrap();
        assert!(set.expires_at.unwrap().is_some());
    }
    #[test]
    fn personal_issuance_query_has_no_owner_or_tenant_override() {
        assert!(
            serde_json::from_value::<OwnerIssuanceListQuery>(
                serde_json::json!({"owner_user_id":Uuid::new_v4()})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<OwnerIssuanceListQuery>(
                serde_json::json!({"tenant_id":Uuid::new_v4()})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<TenantKeyListQuery>(
                serde_json::json!({"platform_role":"root"})
            )
            .is_err()
        );
    }
}
