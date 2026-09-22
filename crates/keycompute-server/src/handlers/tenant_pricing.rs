//! Tenant pricing administration. Path/credential authority never comes from payload fields.
use crate::{
    error::{ApiError, Result},
    extractors::RequestId,
    handlers::{
        admin_pricing::{
            self, CreatePricingAdminRequest, PricingInfo, PricingListResponse,
            UpdatePricingAdminRequest,
        },
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
    DbRouter,
    models::pricing_model::{PricingModel, PricingScopeType, TenantPricingScope},
};
use sea_orm::{DatabaseTransaction, TransactionTrait};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PricingQuery {
    pub search: Option<String>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}
#[derive(Debug, Deserialize)]
pub struct PricingPath {
    pub tenant_id: Uuid,
    pub id: Uuid,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTenantPricing {
    pub model_name: String,
    pub billing_dimension: String,
    pub currency: Option<String>,
    pub input_price_per_1k: String,
    pub output_price_per_1k: String,
    #[serde(default)]
    pub is_default: bool,
    pub effective_from: Option<String>,
    pub effective_until: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultSelection {
    pub model_ids: Vec<Uuid>,
}
fn scope(access: &TenantAdmin) -> Result<TenantPricingScope> {
    access.require(AuthorizationAction::ManageTenantResource)?;
    let auth = access.auth();
    TenantPricingScope::checked(
        access.tenant_id(),
        auth.user_id,
        auth.credential_kind,
        auth.token_version,
        auth.authz_version,
        auth.membership_authz_version,
    )
    .map_err(|e| admin_pricing::map_pricing_db_error(e, "scope"))
}
fn pool(state: &AppState) -> Result<&DbRouter> {
    state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Pricing storage unavailable".into()))
}
fn error(e: keycompute_db::DbError) -> ApiError {
    admin_pricing::map_pricing_db_error(e, "tenant pricing")
}
async fn begin(state: &AppState) -> Result<DatabaseTransaction> {
    pool(state)?.begin().await.map_err(|e| error(e.into()))
}
async fn commit(state: &AppState, tx: DatabaseTransaction) -> Result<()> {
    let _fence = state.display_cache.mutation_guard();
    tx.commit().await.map_err(|e| error(e.into()))?;
    state.pricing.clear_cache().await;
    Ok(())
}
pub async fn list(
    access: TenantAdmin,
    State(state): State<AppState>,
    Query(q): Query<PricingQuery>,
) -> Result<Json<PricingListResponse>> {
    let scope = scope(&access)?;
    let (page, page_size, offset) = normalize_list_pagination(q.page, q.page_size, None, None);
    let db = pool(&state)?.write_conn();
    let rows = PricingModel::find_in_tenant(db, scope, q.search.as_deref(), page_size, offset)
        .await
        .map_err(error)?;
    let total = PricingModel::count_in_tenant(db, scope, q.search.as_deref())
        .await
        .map_err(error)?;
    Ok(Json(PricingListResponse {
        pricing: rows.into_iter().map(Into::into).collect(),
        total,
        page,
        page_size,
        total_pages: total_pages(total, page_size),
    }))
}
pub async fn detail(
    access: TenantAdmin,
    Path(path): Path<PricingPath>,
    State(state): State<AppState>,
) -> Result<Json<PricingInfo>> {
    access.require_path_tenant(path.tenant_id)?;
    let row =
        PricingModel::find_in_tenant_by_id(pool(&state)?.write_conn(), scope(&access)?, path.id)
            .await
            .map_err(error)?
            .ok_or_else(|| ApiError::NotFound("Pricing target not found".into()))?;
    Ok(Json(row.into()))
}
pub async fn create(
    access: TenantAdmin,
    request_id: RequestId,
    State(state): State<AppState>,
    Json(req): Json<CreateTenantPricing>,
) -> Result<Json<PricingInfo>> {
    let scope = scope(&access)?;
    let (_, req) = admin_pricing::create_request(CreatePricingAdminRequest {
        scope_type: PricingScopeType::Tenant,
        tenant_id: Some(access.tenant_id()),
        model_name: req.model_name,
        billing_dimension: req.billing_dimension,
        currency: req.currency.unwrap_or_else(|| "CNY".into()),
        input_price_per_1k: req.input_price_per_1k,
        output_price_per_1k: req.output_price_per_1k,
        is_default: req.is_default,
        effective_from: req.effective_from,
        effective_until: req.effective_until,
    })?;
    let tx = begin(&state).await?;
    let row = PricingModel::create_in_tenant(&tx, scope, &req, &access.audit(request_id))
        .await
        .map_err(error)?;
    commit(&state, tx).await?;
    Ok(Json(row.into()))
}
pub async fn update(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<PricingPath>,
    State(state): State<AppState>,
    Json(req): Json<UpdatePricingAdminRequest>,
) -> Result<Json<PricingInfo>> {
    access.require_path_tenant(path.tenant_id)?;
    let scope = scope(&access)?;
    let req = admin_pricing::platform_update_request(req)?;
    let tx = begin(&state).await?;
    let row = PricingModel::update_in_tenant(&tx, scope, path.id, &req, &access.audit(request_id))
        .await
        .map_err(error)?;
    commit(&state, tx).await?;
    Ok(Json(row.into()))
}
pub async fn remove(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<PricingPath>,
    State(state): State<AppState>,
) -> Result<Json<Value>> {
    access.require_path_tenant(path.tenant_id)?;
    let scope = scope(&access)?;
    let tx = begin(&state).await?;
    PricingModel::delete_in_tenant(&tx, scope, path.id, &access.audit(request_id))
        .await
        .map_err(error)?;
    commit(&state, tx).await?;
    Ok(Json(json!({"success":true,"pricing_id":path.id})))
}
pub async fn make_default(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<PricingPath>,
    State(state): State<AppState>,
) -> Result<Json<PricingInfo>> {
    access.require_path_tenant(path.tenant_id)?;
    let scope = scope(&access)?;
    let tx = begin(&state).await?;
    let row = PricingModel::make_default_in_tenant(&tx, scope, path.id, &access.audit(request_id))
        .await
        .map_err(error)?;
    commit(&state, tx).await?;
    Ok(Json(row.into()))
}
pub async fn batch_defaults(
    access: TenantAdmin,
    request_id: RequestId,
    State(state): State<AppState>,
    Json(req): Json<DefaultSelection>,
) -> Result<Json<Vec<PricingInfo>>> {
    let scope = scope(&access)?;
    let tx = begin(&state).await?;
    let rows = PricingModel::batch_make_defaults_in_tenant(
        &tx,
        scope,
        &req.model_ids,
        &access.audit(request_id),
    )
    .await
    .map_err(error)?;
    commit(&state, tx).await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/tenants/{tenant_id}/pricing",
            get(list).post(create),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/pricing/batch-defaults",
            post(batch_defaults),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/pricing/{id}",
            get(detail).patch(update).delete(remove),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/pricing/{id}/make-default",
            post(make_default),
        )
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
}
