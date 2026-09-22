//! Tenant-scoped passthrough binding management.
//!
//! A tenant administrator can manage only non-global bindings whose account
//! is owned by the same tenant. The route tenant is fixed by `TenantAdmin`;
//! tenant/global ownership selectors are intentionally absent from DTOs.

use crate::{
    error::{ApiError, Result},
    extractors::RequestId,
    handlers::{
        admin_account,
        admin_passthrough_binding::{self, PassthroughBindingProbeRequest},
        pagination::{normalize_list_pagination, total_pages},
    },
    state::AppState,
    tenant_access::TenantAdmin,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use keycompute_auth::AuthorizationAction;
use keycompute_db::{
    AuditContext,
    models::{
        account::{AccountManagementScope, ProviderAuthzSnapshot},
        passthrough_binding::{
            CreatePassthroughBindingRequest as DbCreate, PassthroughAccountOption,
            PassthroughBinding, PassthroughBindingListFilter, PassthroughBindingManagementView,
            UpdatePassthroughBindingRequest as DbUpdate,
        },
    },
};
use sea_orm::{DatabaseTransaction, TransactionTrait};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const TENANT_BINDING_BODY_LIMIT_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
pub struct TenantBindingPath {
    pub tenant_id: Uuid,
    pub id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct TenantPath {
    pub tenant_id: Uuid,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TenantBindingListQuery {
    pub search: Option<String>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTenantBindingRequest {
    pub account_id: Uuid,
    #[serde(default)]
    pub pool_enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTenantBindingRequest {
    pub account_id: Option<Uuid>,
    pub pool_enabled: Option<bool>,
    pub expected_revision: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionQuery {
    pub expected_revision: i64,
}

#[derive(Debug, Serialize)]
pub struct TenantBindingInfo {
    pub id: Uuid,
    pub account_id: Uuid,
    pub account_name: String,
    pub tenant_id: Uuid,
    pub tenant_name: String,
    pub provider: String,
    pub is_global: bool,
    pub pool_enabled: bool,
    pub revision: i64,
    pub models_supported: Vec<String>,
    pub health_status: String,
    pub health_reason_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct TenantBindingListResponse {
    pub bindings: Vec<TenantBindingInfo>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

#[derive(Debug, Serialize)]
pub struct TenantBindingOptionsResponse {
    pub accounts: Vec<PassthroughAccountOption>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

fn pool(state: &AppState) -> Result<&keycompute_db::DbRouter> {
    state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Binding storage unavailable".into()))
}

async fn commit(state: &AppState, tx: DatabaseTransaction) -> Result<()> {
    let _fence = state.display_cache.mutation_guard();
    tx.commit()
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Binding commit failed".into()))
}
async fn write_transaction(state: &AppState) -> Result<DatabaseTransaction> {
    pool(state)?
        .begin()
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Binding storage unavailable".into()))
}
fn scope(access: &TenantAdmin) -> Result<keycompute_types::TenantScope> {
    access.require(AuthorizationAction::ManageTenantResource)
}

fn snapshot(access: &TenantAdmin) -> Result<ProviderAuthzSnapshot> {
    let auth = access.auth();
    if auth.authz_version <= 0 || auth.membership_authz_version <= 0 {
        return Err(ApiError::Forbidden(
            "Current tenant authorization is unavailable".into(),
        ));
    }
    Ok(ProviderAuthzSnapshot::tenant(
        auth.token_version,
        auth.authz_version,
        auth.membership_authz_version,
    ))
}

fn audit(access: &TenantAdmin, request_id: RequestId) -> AuditContext {
    access.audit(request_id)
}

fn map(error: keycompute_db::DbError) -> ApiError {
    match error {
        keycompute_db::DbError::OptimisticConflict { .. } => {
            ApiError::Conflict("Passthrough binding changed; reload and retry".into())
        }
        keycompute_db::DbError::Other(message) if message == "passthrough_binding_ambiguous" => {
            ApiError::Conflict("Another account exposes an overlapping model to this tenant".into())
        }
        keycompute_db::DbError::Other(message)
            if message.contains("authorization")
                || message.contains("administrator")
                || message.contains("root platform") =>
        {
            ApiError::Forbidden("Passthrough binding management authorization denied".into())
        }
        keycompute_db::DbError::Other(message)
            if message.contains("active")
                || message.contains("referenced")
                || message.contains("must") =>
        {
            ApiError::Conflict(message)
        }
        keycompute_db::DbError::NotFound { .. } => {
            ApiError::NotFound("Passthrough binding not found".into())
        }
        _ => admin_account::account_scope_error(error),
    }
}

fn binding_info(mut row: PassthroughBindingManagementView) -> TenantBindingInfo {
    row.validate_connection(|endpoint, secret| {
        crate::passthrough_binding::connection_metadata(endpoint, secret).is_ok()
    });
    TenantBindingInfo {
        id: row.id,
        account_id: row.account_id,
        account_name: row.account_name,
        tenant_id: row.tenant_id,
        tenant_name: row.tenant_name,
        provider: row.provider,
        is_global: row.is_global,
        pool_enabled: row.pool_enabled,
        revision: row.revision,
        models_supported: row.models_supported,
        health_status: row.health_status,
        health_reason_code: row.health_reason_code,
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
    }
}

fn page(page: Option<i64>, page_size: Option<i64>) -> (i64, i64, i64) {
    let (page, page_size, offset) =
        normalize_list_pagination(page, page_size.map(|v| v.clamp(1, 100)), None, None);
    (page, page_size, offset)
}

pub async fn list_bindings(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(query): Query<TenantBindingListQuery>,
) -> Result<Json<TenantBindingListResponse>> {
    access.require_path_tenant(path.tenant_id)?;
    let scope = scope(&access)?;
    let (page_number, page_size, offset) = page(query.page, query.page_size);
    let filter = PassthroughBindingListFilter {
        tenant_id: Some(path.tenant_id),
        search: query.search,
    };
    let db = pool(&state)?.write_conn();
    let rows = PassthroughBinding::list_in_tenant(db, scope, &filter, page_size, offset)
        .await
        .map_err(map)?;
    let total = PassthroughBinding::count_in_tenant(db, scope, &filter)
        .await
        .map_err(map)?;
    Ok(Json(TenantBindingListResponse {
        bindings: rows.into_iter().map(binding_info).collect(),
        total,
        page: page_number,
        page_size,
        total_pages: total_pages(total, page_size),
    }))
}

pub async fn get_binding(
    access: TenantAdmin,
    Path(path): Path<TenantBindingPath>,
    State(state): State<AppState>,
) -> Result<Json<TenantBindingInfo>> {
    access.require_path_tenant(path.tenant_id)?;
    let row =
        PassthroughBinding::find_in_tenant(pool(&state)?.write_conn(), scope(&access)?, path.id)
            .await
            .map_err(map)?
            .ok_or_else(|| ApiError::NotFound("Passthrough binding not found".into()))?;
    Ok(Json(binding_info(row)))
}

pub async fn create_binding(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    request_id: RequestId,
    Json(request): Json<CreateTenantBindingRequest>,
) -> Result<Json<TenantBindingInfo>> {
    access.require_path_tenant(path.tenant_id)?;
    let scope = scope(&access)?;
    let tx = write_transaction(&state).await?;
    let created = PassthroughBinding::create_in_tenant(
        &tx,
        scope,
        &DbCreate {
            account_id: request.account_id,
            tenant_id: path.tenant_id,
            is_global: false,
            pool_enabled: request.pool_enabled,
        },
        &audit(&access, request_id),
        snapshot(&access)?,
    )
    .await
    .map_err(map)?;
    commit(&state, tx).await?;
    let row = PassthroughBinding::find_in_tenant(pool(&state)?.write_conn(), scope, created.id)
        .await
        .map_err(map)?
        .ok_or_else(|| ApiError::NotFound("Passthrough binding not found".into()))?;
    Ok(Json(binding_info(row)))
}

pub async fn update_binding(
    access: TenantAdmin,
    Path(path): Path<TenantBindingPath>,
    State(state): State<AppState>,
    request_id: RequestId,
    Json(request): Json<UpdateTenantBindingRequest>,
) -> Result<Json<TenantBindingInfo>> {
    access.require_path_tenant(path.tenant_id)?;
    let scope = scope(&access)?;
    let tx = write_transaction(&state).await?;
    let updated = PassthroughBinding::update_in_tenant(
        &tx,
        scope,
        path.id,
        &DbUpdate {
            account_id: request.account_id,
            tenant_id: None,
            is_global: None,
            pool_enabled: request.pool_enabled,
            expected_revision: request.expected_revision,
        },
        &audit(&access, request_id),
        snapshot(&access)?,
    )
    .await
    .map_err(map)?;
    commit(&state, tx).await?;
    let row = PassthroughBinding::find_in_tenant(pool(&state)?.write_conn(), scope, updated.id)
        .await
        .map_err(map)?
        .ok_or_else(|| ApiError::NotFound("Passthrough binding not found".into()))?;
    Ok(Json(binding_info(row)))
}

pub async fn delete_binding(
    access: TenantAdmin,
    Path(path): Path<TenantBindingPath>,
    State(state): State<AppState>,
    request_id: RequestId,
    Query(query): Query<RevisionQuery>,
) -> Result<Json<serde_json::Value>> {
    access.require_path_tenant(path.tenant_id)?;
    let tx = write_transaction(&state).await?;
    PassthroughBinding::delete_in_tenant(
        &tx,
        scope(&access)?,
        path.id,
        query.expected_revision,
        &audit(&access, request_id),
        snapshot(&access)?,
    )
    .await
    .map_err(map)?;
    commit(&state, tx).await?;
    Ok(Json(serde_json::json!({
        "deleted": true,
        "binding_id": path.id
    })))
}

pub async fn binding_options(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(query): Query<TenantBindingListQuery>,
) -> Result<Json<TenantBindingOptionsResponse>> {
    access.require_path_tenant(path.tenant_id)?;
    let (page_number, page_size, offset) = page(query.page, query.page_size);
    let rows = PassthroughBinding::options_in_tenant(
        pool(&state)?.write_conn(),
        scope(&access)?,
        query.search.as_deref(),
        page_size,
        offset,
    )
    .await
    .map_err(map)?;
    Ok(Json(TenantBindingOptionsResponse {
        accounts: rows.0,
        total: rows.1,
        page: page_number,
        page_size,
        total_pages: total_pages(rows.1, page_size),
    }))
}

pub async fn probe_binding(
    access: TenantAdmin,
    Path(path): Path<TenantBindingPath>,
    State(state): State<AppState>,
    request_id: RequestId,
    Json(request): Json<PassthroughBindingProbeRequest>,
) -> Result<Json<serde_json::Value>> {
    access.require_path_tenant(path.tenant_id)?;
    let scope = AccountManagementScope::Tenant(scope(&access)?);
    let result = admin_passthrough_binding::probe_with_scope(
        &state,
        scope,
        path.id,
        &audit(&access, request_id),
        snapshot(&access)?,
        request,
    )
    .await?;
    Ok(result)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/tenants/{tenant_id}/passthrough-bindings",
            get(list_bindings).post(create_binding),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/passthrough-bindings/{id}",
            get(get_binding)
                .put(update_binding)
                .patch(update_binding)
                .delete(delete_binding),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/passthrough-bindings/{id}/probe",
            post(probe_binding),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/passthrough-bindings/options",
            get(binding_options),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            TENANT_BINDING_BODY_LIMIT_BYTES,
        ))
}
