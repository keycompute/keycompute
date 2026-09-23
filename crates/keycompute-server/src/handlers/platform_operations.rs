//! Operator allowlist: metadata, bounded aggregates and process diagnostics only.
use crate::{
    error::{ApiError, Result},
    extractors::GlobalConsoleAuth,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    response::Response,
    routing::get,
};
use chrono::{DateTime, Duration, Utc};
use keycompute_auth::AuthorizationAction;
use keycompute_db::models::platform_operations::{
    OperationsMembership, OperationsSession, OperationsTarget, PlatformOperationsScope,
    TenantHealth, TenantHealthPage, TenantHealthQuery, UsageOperations,
};
use serde::Deserialize;
use uuid::Uuid;
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthQuery {
    pub search: Option<String>,
    pub status: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}
#[derive(Debug, Deserialize)]
pub struct TenantPath {
    tenant_id: Uuid,
}
fn map_error(error: keycompute_db::DbError) -> ApiError {
    if error.is_not_found() {
        return ApiError::NotFound("Operational target not found".into());
    }
    match error {
        keycompute_db::DbError::Other(code) if code == "platform_operations_authority_invalid" => {
            ApiError::Forbidden("Current platform operations authority required".into())
        }
        keycompute_db::DbError::Other(code) if code == "platform_operations_query_invalid" => {
            ApiError::BadRequest("Invalid or excessive operational query".into())
        }
        _ => ApiError::ServiceUnavailable("Platform diagnostics unavailable".into()),
    }
}
fn db(state: &AppState) -> Result<&sea_orm::DatabaseConnection> {
    Ok(state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Platform diagnostics unavailable".into()))?
        .write_conn())
}
fn scope(auth: &GlobalConsoleAuth, action: AuthorizationAction) -> Result<PlatformOperationsScope> {
    let platform = auth.require_platform(action)?;
    let selected = auth
        .selected_tenant_id
        .map(|tenant_id| {
            Ok::<_, ApiError>(OperationsMembership {
                tenant_id,
                tenant_role: auth
                    .tenant_role
                    .ok_or_else(|| ApiError::Forbidden("Current membership required".into()))?,
                tenant_authz_version: auth
                    .authz_version
                    .ok_or_else(|| ApiError::Forbidden("Current tenant version required".into()))?,
                membership_authz_version: auth.membership_authz_version.ok_or_else(|| {
                    ApiError::Forbidden("Current membership version required".into())
                })?,
            })
        })
        .transpose()?;
    PlatformOperationsScope::checked(
        platform,
        OperationsSession {
            credential_kind: auth.credential_kind,
            token_version: auth.token_version,
            expires_at: auth
                .credential_expires_at
                .ok_or_else(|| ApiError::Auth("Expiring console session required".into()))?,
            selected,
        },
    )
    .map_err(map_error)
}
pub async fn tenants(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
    Query(query): Query<HealthQuery>,
) -> Result<Json<TenantHealthPage>> {
    scope(&auth, AuthorizationAction::ReadTenantHealth)?
        .tenants(
            db(&state)?,
            &TenantHealthQuery {
                search: query.search,
                status: query.status,
                limit: query.limit.unwrap_or(20),
                offset: query.offset.unwrap_or(0),
            },
        )
        .await
        .map(Json)
        .map_err(map_error)
}
pub async fn tenant(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
    Path(path): Path<TenantPath>,
) -> Result<Json<TenantHealth>> {
    scope(&auth, AuthorizationAction::ReadTenantHealth)?
        .tenant(db(&state)?, path.tenant_id)
        .await
        .map(Json)
        .map_err(map_error)
}
async fn aggregate(
    auth: GlobalConsoleAuth,
    state: AppState,
    target: OperationsTarget,
    query: AggregateQuery,
) -> Result<Json<UsageOperations>> {
    let to = query.to.unwrap_or_else(Utc::now);
    let from = query
        .from
        .or_else(|| to.checked_sub_signed(Duration::days(1)))
        .ok_or_else(|| ApiError::BadRequest("Invalid report range".into()))?;
    scope(&auth, AuthorizationAction::AggregateStats)?
        .usage(db(&state)?, target, from, to)
        .await
        .map(Json)
        .map_err(map_error)
}
pub async fn platform_usage(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
    Query(query): Query<AggregateQuery>,
) -> Result<Json<UsageOperations>> {
    aggregate(auth, state, OperationsTarget::Platform, query).await
}
pub async fn tenant_usage(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
    Path(path): Path<TenantPath>,
    Query(query): Query<AggregateQuery>,
) -> Result<Json<UsageOperations>> {
    aggregate(auth, state, OperationsTarget::Tenant(path.tenant_id), query).await
}
pub async fn capacity(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    let access = scope(&auth, AuthorizationAction::Diagnostics)?;
    access
        .validate_current(db(&state)?)
        .await
        .map_err(map_error)?;
    let snapshot = super::admin_capacity::process_snapshot(&state);
    // Only allowlisted process counters, no future fields added to raw diagnostics.
    let mut safe = serde_json::Map::new();
    for key in [
        "scope",
        "managed_payload_bytes",
        "ingress",
        "generation",
        "writer_pool",
        "redis_commands",
        "redis_cache",
        "stages",
    ] {
        if let Some(value) = snapshot.get(key) {
            safe.insert(key.into(), value.clone());
        }
    }
    Ok(Json(serde_json::Value::Object(safe)))
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
        .route("/api/v1/platform/operations/tenants", get(tenants))
        .route(
            "/api/v1/platform/operations/tenants/{tenant_id}",
            get(tenant),
        )
        .route("/api/v1/platform/operations/usage", get(platform_usage))
        .route(
            "/api/v1/platform/operations/tenants/{tenant_id}/usage",
            get(tenant_usage),
        )
        .route("/api/v1/platform/operations/capacity", get(capacity))
        .layer(axum::middleware::map_response(private_response))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
}
