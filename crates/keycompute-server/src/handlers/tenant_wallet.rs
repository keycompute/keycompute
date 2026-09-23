//! Canonical wallet control: root money adjustments, tenant expired-work recovery.
use super::admin_user as admin;
use crate::{
    error::{ApiError, Result},
    extractors::{GlobalConsoleAuth, RequestId},
    state::AppState,
    tenant_access::TenantAdmin,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Response,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Deserialize)]
struct WalletPath {
    tenant_id: Uuid,
    user_id: Uuid,
}
#[derive(Deserialize)]
struct RecoveryPath {
    tenant_id: Uuid,
    user_id: Uuid,
    request_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    cursor: Option<String>,
    limit: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoneyBody {
    amount: String,
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryBody {
    expected_version: Uuid,
    reason: String,
}

macro_rules! money_handler {
    ($name:ident,$original:ident) => {
        async fn $name(
            auth: GlobalConsoleAuth,
            id: RequestId,
            Path(path): Path<WalletPath>,
            State(state): State<AppState>,
            headers: HeaderMap,
            Json(body): Json<MoneyBody>,
        ) -> Result<Json<Value>> {
            crate::financial_auth::root_scope(&auth.0, path.tenant_id)?;
            let _fence = state.display_cache.mutation_guard();
            admin::$original(
                auth,
                id,
                Path(path.user_id),
                State(state),
                headers,
                Json(admin::UpdateBalanceRequest {
                    tenant_id: path.tenant_id,
                    amount: body.amount,
                    reason: body.reason,
                }),
            )
            .await
        }
    };
}
money_handler!(root_adjust, update_user_balance);
money_handler!(root_freeze, freeze_user_balance);
money_handler!(root_unfreeze, unfreeze_user_balance);

async fn root_reservations(
    auth: GlobalConsoleAuth,
    Path(path): Path<WalletPath>,
    State(state): State<AppState>,
    Query(q): Query<PageQuery>,
) -> Result<Json<admin::AdminBalanceReservationsResponse>> {
    admin::list_user_balance_reservations(
        auth,
        Path(path.user_id),
        Query(admin::AdminBalanceReservationsQuery {
            tenant_id: path.tenant_id,
            cursor: q.cursor,
            limit: q.limit,
        }),
        State(state),
    )
    .await
}
async fn root_release(
    auth: GlobalConsoleAuth,
    id: RequestId,
    Path(path): Path<RecoveryPath>,
    State(state): State<AppState>,
    Json(body): Json<RecoveryBody>,
) -> Result<Json<admin::ReleaseBalanceReservationResponse>> {
    crate::financial_auth::root_scope(&auth.0, path.tenant_id)?;
    let _fence = state.display_cache.mutation_guard();
    admin::release_user_balance_reservation(
        auth,
        id,
        Path((path.user_id, path.request_id)),
        State(state),
        Json(admin::ReleaseBalanceReservationRequest {
            tenant_id: path.tenant_id,
            expected_version: body.expected_version,
            reason: body.reason,
        }),
    )
    .await
}
fn wallet_error(error: keycompute_types::KeyComputeError) -> ApiError {
    match error {
        keycompute_types::KeyComputeError::ValidationError(message)
            if message == "Financial target unavailable" =>
        {
            ApiError::NotFound("Financial target unavailable".into())
        }
        other => ApiError::from(other),
    }
}

async fn tenant_reservations(
    access: TenantAdmin,
    Path(path): Path<WalletPath>,
    State(state): State<AppState>,
    Query(q): Query<PageQuery>,
) -> Result<Json<admin::AdminBalanceReservationsResponse>> {
    let scope = crate::financial_auth::tenant_scope(&access, path.tenant_id)?;
    let cursor = q
        .cursor
        .as_deref()
        .map(admin::decode_balance_reservation_cursor)
        .transpose()?;
    let service = state
        .billing
        .balance_service()
        .ok_or_else(|| ApiError::ServiceUnavailable("Financial storage unavailable".into()))?;
    let page = service
        .find_breakdown_page_in_scope(
            scope,
            path.user_id,
            cursor,
            q.limit.unwrap_or(50).clamp(1, 100),
        )
        .await
        .map_err(wallet_error)?;
    admin::reservation_page_response(path.user_id, page)
}
async fn tenant_release(
    access: TenantAdmin,
    id: RequestId,
    Path(path): Path<RecoveryPath>,
    State(state): State<AppState>,
    Json(body): Json<RecoveryBody>,
) -> Result<Json<admin::ReleaseBalanceReservationResponse>> {
    let scope = crate::financial_auth::tenant_scope(&access, path.tenant_id)?;
    let _fence = state.display_cache.mutation_guard();
    let service = state
        .billing
        .balance_service()
        .ok_or_else(|| ApiError::ServiceUnavailable("Financial storage unavailable".into()))?;
    let result=service.admin_release_request_reservation(scope,&access.audit(id),&keycompute_billing::balance::ReleaseReservationCommand {
        user_id:path.user_id,request_id:path.request_id,expected_owner_token:body.expected_version,reason:&body.reason}).await?
        .ok_or_else(||ApiError::Conflict("Only an expired reservation with its current version can be recovered by the tenant administrator".into()))?;
    admin::reservation_release_response(
        path.user_id,
        path.request_id,
        access.auth().user_id,
        body.reason.trim(),
        result,
    )
}
async fn private_response(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        "private, no-store".parse().unwrap(),
    );
    response
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/platform/tenants/{tenant_id}/users/{user_id}/balance",post(root_adjust))
        .route("/api/v1/platform/tenants/{tenant_id}/users/{user_id}/balance/freeze",post(root_freeze))
        .route("/api/v1/platform/tenants/{tenant_id}/users/{user_id}/balance/unfreeze",post(root_unfreeze))
        .route("/api/v1/platform/tenants/{tenant_id}/users/{user_id}/balance/reservations",get(root_reservations))
        .route("/api/v1/platform/tenants/{tenant_id}/users/{user_id}/balance/reservations/{request_id}/release",post(root_release))
        .route("/api/v1/tenants/{tenant_id}/users/{user_id}/balance/reservations",get(tenant_reservations))
        .route("/api/v1/tenants/{tenant_id}/users/{user_id}/balance/reservations/{request_id}/release",post(tenant_release))
        .layer(axum::middleware::map_response(private_response))
        .layer(axum::extract::DefaultBodyLimit::max(16*1024))
}
