//! Read-only tenant financial reporting with fixed scope and safe projections.
use crate::{
    console_session_proof::ConsoleSessionProof,
    error::{ApiError, Result},
    handlers::pagination::total_pages,
    state::AppState,
    tenant_access::TenantAdmin,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderValue, header},
    response::Response,
    routing::get,
};
use chrono::{DateTime, Duration, Utc};
use keycompute_auth::AuthorizationAction;
use keycompute_db::{
    PaymentOrder, UserBalance, UserBalanceDisplaySnapshot,
    models::{
        payment_order::{PaymentOrderReportRow, PaymentOrderStatus},
        usage_log::{CurrencyUsageStats, TenantUsageScope, UsageLogReportRow},
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
pub struct WalletPath {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub owner_user_id: Option<Uuid>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatsQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub owner_user_id: Option<Uuid>,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentQuery {
    pub status: Option<String>,
    pub owner_user_id: Option<Uuid>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}
#[derive(Debug, Serialize)]
pub struct ReportPage<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}
#[derive(Debug, Serialize)]
pub struct ReportTotals {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub currencies: Vec<CurrencyUsageStats>,
}
fn pagination(page: Option<i64>, size: Option<i64>) -> (i64, i64, i64) {
    let page = page.unwrap_or(1).clamp(1, 1_000_000);
    let size = size.unwrap_or(20).clamp(1, 100);
    (page, size, (page - 1) * size)
}
fn page<T>(items: Vec<T>, total: i64, page: i64, size: i64) -> ReportPage<T> {
    ReportPage {
        items,
        total,
        page,
        page_size: size,
        total_pages: total_pages(total, size),
    }
}
fn pool(state: &AppState) -> Result<&keycompute_db::DbRouter> {
    state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Reporting storage unavailable".into()))
}
fn db_error(error: keycompute_db::DbError) -> ApiError {
    if error.is_not_found() {
        return ApiError::NotFound("Report resource not found".into());
    }
    if let keycompute_db::DbError::Other(message) = &error {
        if message.contains("scope required") || message.contains("authorization denied") {
            return ApiError::Forbidden("Tenant reporting is not permitted".into());
        }
        if message.contains("start before end") || message.contains("real UUID") {
            return ApiError::BadRequest(message.clone());
        }
    }
    tracing::error!(error=%error,"tenant reporting storage query failed");
    ApiError::ServiceUnavailable("Reporting storage unavailable".into())
}
fn validate_window(
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    owner: Option<Uuid>,
) -> Result<()> {
    if matches!((from,to),(Some(a),Some(b)) if a>=b) {
        return Err(ApiError::BadRequest("from must be before to".into()));
    }
    if owner.is_some_and(|id| id.is_nil()) {
        return Err(ApiError::BadRequest(
            "owner_user_id must be a real UUID".into(),
        ));
    }
    Ok(())
}
fn scope(access: &TenantAdmin) -> Result<keycompute_types::TenantScope> {
    access.require(AuthorizationAction::View)
}
// A delayed report is not allowed to publish under a revoked or regranted
// original session. DAO ownership predicates remain mandatory and unchanged.
async fn finish_read(state: &AppState, access: &TenantAdmin) -> Result<()> {
    ConsoleSessionProof::from_console(access.auth())?
        .verify_current(pool(state)?.write_conn())
        .await
}
async fn private_report(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}
pub async fn list_usage(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<UsageQuery>,
) -> Result<Json<ReportPage<UsageLogReportRow>>> {
    access.require_path_tenant(path.tenant_id)?;
    validate_window(q.from, q.to, q.owner_user_id)?;
    let usage = TenantUsageScope::new(scope(&access)?).map_err(db_error)?;
    let (number, size, offset) = pagination(q.page, q.page_size);
    let db = pool(&state)?.write_conn();
    let rows = usage
        .list_report(db, q.from, q.to, q.owner_user_id, size, offset)
        .await
        .map_err(db_error)?;
    let total = usage
        .count_report(db, q.from, q.to, q.owner_user_id)
        .await
        .map_err(db_error)?;
    finish_read(&state, &access).await?;
    Ok(Json(page(rows, total, number, size)))
}
pub async fn get_usage(
    access: TenantAdmin,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
) -> Result<Json<UsageLogReportRow>> {
    access.require_path_tenant(path.tenant_id)?;
    let usage = TenantUsageScope::new(scope(&access)?).map_err(db_error)?;
    let row = usage
        .find_report(pool(&state)?.write_conn(), path.id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| ApiError::NotFound("Usage record not found".into()))?;
    finish_read(&state, &access).await?;
    Ok(Json(row))
}
pub async fn usage_stats(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<StatsQuery>,
) -> Result<Json<ReportTotals>> {
    access.require_path_tenant(path.tenant_id)?;
    let to = q.to.unwrap_or_else(Utc::now);
    let from = q
        .from
        .or_else(|| to.checked_sub_signed(Duration::days(30)))
        .ok_or_else(|| ApiError::BadRequest("report time range is invalid".into()))?;
    validate_window(Some(from), Some(to), q.owner_user_id)?;
    let usage = TenantUsageScope::new(scope(&access)?).map_err(db_error)?;
    let currencies = usage
        .currency_stats(
            pool(&state)?.write_conn(),
            Some(from),
            Some(to),
            q.owner_user_id,
        )
        .await
        .map_err(db_error)?;
    finish_read(&state, &access).await?;
    Ok(Json(ReportTotals {
        from,
        to,
        currencies,
    }))
}
pub async fn list_payments(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<PaymentQuery>,
) -> Result<Json<ReportPage<PaymentOrderReportRow>>> {
    access.require_path_tenant(path.tenant_id)?;
    validate_window(None, None, q.owner_user_id)?;
    if q.status
        .as_deref()
        .is_some_and(|s| PaymentOrderStatus::parse(s).is_none())
    {
        return Err(ApiError::BadRequest("invalid payment status".into()));
    }
    let scope = scope(&access)?;
    let (number, size, offset) = pagination(q.page, q.page_size);
    let db = pool(&state)?.write_conn();
    let rows = PaymentOrder::list_report_in_tenant(
        db,
        scope,
        q.status.as_deref(),
        q.owner_user_id,
        size,
        offset,
    )
    .await
    .map_err(db_error)?;
    let total = PaymentOrder::count_in_tenant(db, scope, q.status.as_deref(), q.owner_user_id)
        .await
        .map_err(db_error)?;
    finish_read(&state, &access).await?;
    Ok(Json(page(rows, total, number, size)))
}
pub async fn get_payment(
    access: TenantAdmin,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
) -> Result<Json<PaymentOrderReportRow>> {
    access.require_path_tenant(path.tenant_id)?;
    let row =
        PaymentOrder::find_report_in_tenant(pool(&state)?.write_conn(), scope(&access)?, path.id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| ApiError::NotFound("Payment order not found".into()))?;
    finish_read(&state, &access).await?;
    Ok(Json(row))
}
pub async fn get_balance(
    access: TenantAdmin,
    Path(path): Path<WalletPath>,
    State(state): State<AppState>,
) -> Result<Json<UserBalanceDisplaySnapshot>> {
    access.require_path_tenant(path.tenant_id)?;
    if path.user_id.is_nil() {
        return Err(ApiError::NotFound("Wallet owner not found".into()));
    }
    let mut rows = UserBalance::find_display_snapshots_in_tenant(
        pool(&state)?.write_conn(),
        scope(&access)?,
        &[path.user_id],
    )
    .await
    .map_err(db_error)?;
    finish_read(&state, &access).await?;
    Ok(Json(rows.remove(&path.user_id).ok_or_else(|| {
        ApiError::NotFound("Wallet owner not found".into())
    })?))
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/tenants/{tenant_id}/usage", get(list_usage))
        .route("/api/v1/tenants/{tenant_id}/usage/stats", get(usage_stats))
        .route("/api/v1/tenants/{tenant_id}/usage/{id}", get(get_usage))
        .route(
            "/api/v1/tenants/{tenant_id}/billing/records",
            get(list_usage),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/billing/records/{id}",
            get(get_usage),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/billing/stats",
            get(usage_stats),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/payments/orders",
            get(list_payments),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/payments/orders/{id}",
            get(get_payment),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/balances/{user_id}",
            get(get_balance),
        )
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024))
        .layer(axum::middleware::map_response(private_report))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn query_contract_rejects_unknown_scope_and_invalid_range() {
        assert!(
            serde_json::from_value::<UsageQuery>(serde_json::json!({"tenant_id":Uuid::new_v4()}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<PaymentQuery>(serde_json::json!({"include_secrets":true}))
                .is_err()
        );
        let now = Utc::now();
        assert!(validate_window(Some(now), Some(now), None).is_err());
        assert!(validate_window(None, None, Some(Uuid::nil())).is_err());
        assert_eq!(
            keycompute_types::console::classify("GET", "/api/v1/tenants/tenant-id/billing/stats"),
            Some(keycompute_types::console::ConsoleClass::HeavyRead)
        );
        assert_eq!(
            keycompute_types::console::classify("GET", "/api/v1/tenants/tenant-id/payments/orders"),
            Some(keycompute_types::console::ConsoleClass::Read)
        );
        assert_eq!(
            pagination(Some(i64::MAX), Some(i64::MAX)),
            (1_000_000, 100, 99_999_900)
        );
    }
}
