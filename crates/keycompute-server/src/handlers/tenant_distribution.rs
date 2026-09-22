//! Scoped distribution reporting; policy writes and settlement are separate capabilities.
use crate::{
    error::{ApiError, Result},
    extractors::{ConsoleAuth, GlobalConsoleAuth},
    handlers::pagination::total_pages,
    state::AppState,
    tenant_access::TenantAdmin,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use chrono::{DateTime, Utc};
use keycompute_auth::AuthorizationAction;
use keycompute_db::models::distribution_scope::{
    self as dao, DistributionScope, RecordFilter, RuleFilter,
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
pub struct PersonalResourcePath {
    pub id: Uuid,
}
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RecordQuery {
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub beneficiary_id: Option<Uuid>,
    pub status: Option<String>,
    pub level: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub currency: Option<String>,
}
impl RecordQuery {
    fn filter(&self) -> RecordFilter {
        RecordFilter {
            beneficiary_id: self.beneficiary_id,
            status: self.status.clone(),
            level: self.level.clone(),
            from: self.from,
            until: self.until,
            currency: self.currency.clone(),
        }
    }
}
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RuleQuery {
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub search: Option<String>,
    pub beneficiary_id: Option<Uuid>,
    pub is_active: Option<bool>,
}
#[derive(Debug, Serialize)]
pub struct RecordPage {
    pub records: Vec<dao::DistributionRecordReport>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}
#[derive(Debug, Serialize)]
pub struct RulePage {
    pub rules: Vec<keycompute_db::TenantDistributionRule>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}
#[derive(Debug, Serialize)]
pub struct Stats {
    pub tenant_id: Uuid,
    pub currencies: Vec<dao::DistributionCurrencyStats>,
}
fn pool(s: &AppState) -> Result<&keycompute_db::DbRouter> {
    s.pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Distribution storage unavailable".into()))
}
fn map(e: keycompute_db::DbError) -> ApiError {
    match e {
        keycompute_db::DbError::Other(m) if m.starts_with("invalid distribution input:") => {
            ApiError::BadRequest(m)
        }
        keycompute_db::DbError::Other(m) if m.contains("authorization") => {
            ApiError::Forbidden("Distribution access denied".into())
        }
        keycompute_db::DbError::NotFound { .. } => {
            ApiError::NotFound("Distribution resource not found".into())
        }
        _ => ApiError::ServiceUnavailable("Distribution query unavailable".into()),
    }
}
fn page(page: Option<i64>, size: Option<i64>) -> Result<(i64, i64, i64)> {
    let p = page.unwrap_or(1);
    let s = size.unwrap_or(20);
    if !(1..=1_000_000).contains(&p) || !(1..=100).contains(&s) {
        return Err(ApiError::BadRequest(
            "page or page_size outside supported range".into(),
        ));
    }
    Ok((p, s, (p - 1) * s))
}
fn tenant(a: &TenantAdmin, t: Uuid) -> Result<DistributionScope> {
    a.require_path_tenant(t)?;
    Ok(DistributionScope::Tenant(
        a.require(AuthorizationAction::ManageTenantResource)?,
    ))
}
fn platform(a: &GlobalConsoleAuth, t: Uuid) -> Result<DistributionScope> {
    if t.is_nil() {
        return Err(ApiError::BadRequest("target tenant required".into()));
    }
    Ok(DistributionScope::Platform(
        a.require_platform(AuthorizationAction::ManagePlatform)?,
        t,
    ))
}
fn personal(a: &ConsoleAuth) -> Result<DistributionScope> {
    Ok(DistributionScope::Owned(a.require_owner(
        a.user_id,
        AuthorizationAction::ReadPersonalResource,
    )?))
}
async fn bounded<T>(
    f: impl std::future::Future<Output = std::result::Result<T, keycompute_db::DbError>>,
) -> Result<T> {
    tokio::time::timeout(std::time::Duration::from_secs(3), f)
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Distribution query timed out".into()))?
        .map_err(map)
}
async fn record_page(
    s: &AppState,
    scope: DistributionScope,
    q: RecordQuery,
) -> Result<Json<RecordPage>> {
    let (p, n, offset) = page(q.page, q.page_size)?;
    let db = pool(s)?.write_conn();
    let f = q.filter();
    let records = bounded(dao::records(db, scope, &f, n, offset)).await?;
    let total = bounded(dao::record_count(db, scope, &f)).await?;
    Ok(Json(RecordPage {
        records,
        total,
        page: p,
        page_size: n,
        total_pages: total_pages(total, n),
    }))
}
async fn record_detail(
    s: &AppState,
    scope: DistributionScope,
    id: Uuid,
) -> Result<Json<dao::DistributionRecordReport>> {
    Ok(Json(
        bounded(dao::record(pool(s)?.write_conn(), scope, id))
            .await?
            .ok_or_else(|| ApiError::NotFound("Distribution record not found".into()))?,
    ))
}
async fn statistics(s: &AppState, scope: DistributionScope, q: RecordQuery) -> Result<Json<Stats>> {
    page(q.page, q.page_size)?;
    let currencies = bounded(dao::record_stats(pool(s)?.write_conn(), scope, &q.filter())).await?;
    Ok(Json(Stats {
        tenant_id: scope.tenant_id(),
        currencies,
    }))
}
async fn rule_page(s: &AppState, scope: DistributionScope, q: RuleQuery) -> Result<Json<RulePage>> {
    let (p, n, offset) = page(q.page, q.page_size)?;
    let f = RuleFilter {
        search: q.search,
        beneficiary_id: q.beneficiary_id,
        is_active: q.is_active,
    };
    let db = pool(s)?.write_conn();
    let rules = bounded(dao::rules(db, scope, &f, n, offset)).await?;
    let total = bounded(dao::rule_count(db, scope, &f)).await?;
    Ok(Json(RulePage {
        rules,
        total,
        page: p,
        page_size: n,
        total_pages: total_pages(total, n),
    }))
}
async fn rule_detail(
    s: &AppState,
    scope: DistributionScope,
    id: Uuid,
) -> Result<Json<keycompute_db::TenantDistributionRule>> {
    Ok(Json(
        bounded(dao::rule(pool(s)?.write_conn(), scope, id))
            .await?
            .ok_or_else(|| ApiError::NotFound("Distribution rule not found".into()))?,
    ))
}
async fn enabled(s: &AppState) -> Result<()> {
    super::distribution::check_distribution_enabled(pool(s)?.write_conn()).await
}

pub async fn tenant_records(
    a: TenantAdmin,
    Path(p): Path<TenantPath>,
    State(s): State<AppState>,
    Query(q): Query<RecordQuery>,
) -> Result<Json<RecordPage>> {
    record_page(&s, tenant(&a, p.tenant_id)?, q).await
}
pub async fn tenant_record(
    a: TenantAdmin,
    Path(p): Path<ResourcePath>,
    State(s): State<AppState>,
) -> Result<Json<dao::DistributionRecordReport>> {
    record_detail(&s, tenant(&a, p.tenant_id)?, p.id).await
}
pub async fn tenant_stats(
    a: TenantAdmin,
    Path(p): Path<TenantPath>,
    State(s): State<AppState>,
    Query(q): Query<RecordQuery>,
) -> Result<Json<Stats>> {
    statistics(&s, tenant(&a, p.tenant_id)?, q).await
}
pub async fn tenant_rules(
    a: TenantAdmin,
    Path(p): Path<TenantPath>,
    State(s): State<AppState>,
    Query(q): Query<RuleQuery>,
) -> Result<Json<RulePage>> {
    rule_page(&s, tenant(&a, p.tenant_id)?, q).await
}
pub async fn tenant_rule(
    a: TenantAdmin,
    Path(p): Path<ResourcePath>,
    State(s): State<AppState>,
) -> Result<Json<keycompute_db::TenantDistributionRule>> {
    rule_detail(&s, tenant(&a, p.tenant_id)?, p.id).await
}
pub async fn platform_records(
    a: GlobalConsoleAuth,
    Path(p): Path<TenantPath>,
    State(s): State<AppState>,
    Query(q): Query<RecordQuery>,
) -> Result<Json<RecordPage>> {
    record_page(&s, platform(&a, p.tenant_id)?, q).await
}
pub async fn platform_record(
    a: GlobalConsoleAuth,
    Path(p): Path<ResourcePath>,
    State(s): State<AppState>,
) -> Result<Json<dao::DistributionRecordReport>> {
    record_detail(&s, platform(&a, p.tenant_id)?, p.id).await
}
pub async fn platform_stats(
    a: GlobalConsoleAuth,
    Path(p): Path<TenantPath>,
    State(s): State<AppState>,
    Query(q): Query<RecordQuery>,
) -> Result<Json<Stats>> {
    statistics(&s, platform(&a, p.tenant_id)?, q).await
}
pub async fn platform_rules(
    a: GlobalConsoleAuth,
    Path(p): Path<TenantPath>,
    State(s): State<AppState>,
    Query(q): Query<RuleQuery>,
) -> Result<Json<RulePage>> {
    rule_page(&s, platform(&a, p.tenant_id)?, q).await
}
pub async fn platform_rule(
    a: GlobalConsoleAuth,
    Path(p): Path<ResourcePath>,
    State(s): State<AppState>,
) -> Result<Json<keycompute_db::TenantDistributionRule>> {
    rule_detail(&s, platform(&a, p.tenant_id)?, p.id).await
}
pub async fn my_records(
    a: ConsoleAuth,
    State(s): State<AppState>,
    Query(q): Query<RecordQuery>,
) -> Result<Json<RecordPage>> {
    let scope = personal(&a)?;
    enabled(&s).await?;
    record_page(&s, scope, q).await
}
pub async fn my_record(
    a: ConsoleAuth,
    Path(p): Path<PersonalResourcePath>,
    State(s): State<AppState>,
) -> Result<Json<dao::DistributionRecordReport>> {
    let scope = personal(&a)?;
    enabled(&s).await?;
    record_detail(&s, scope, p.id).await
}
pub async fn my_stats(
    a: ConsoleAuth,
    State(s): State<AppState>,
    Query(q): Query<RecordQuery>,
) -> Result<Json<Stats>> {
    let scope = personal(&a)?;
    enabled(&s).await?;
    statistics(&s, scope, q).await
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/tenants/{tenant_id}/distribution/rules/default",
            axum::routing::post(super::distribution_policy::tenant_default),
        )
        .route(
            "/api/v1/platform/distribution/tenants/{tenant_id}/rules/default",
            axum::routing::post(super::distribution_policy::platform_default),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/distribution/records",
            get(tenant_records),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/distribution/records/{id}",
            get(tenant_record),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/distribution/stats",
            get(tenant_stats),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/distribution/rules",
            get(tenant_rules).post(super::distribution_policy::tenant_create),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/distribution/rules/{id}",
            get(tenant_rule)
                .patch(super::distribution_policy::tenant_patch)
                .delete(super::distribution_policy::tenant_delete),
        )
        .route(
            "/api/v1/platform/distribution/tenants/{tenant_id}/records",
            get(platform_records),
        )
        .route(
            "/api/v1/platform/distribution/tenants/{tenant_id}/records/{id}",
            get(platform_record),
        )
        .route(
            "/api/v1/platform/distribution/tenants/{tenant_id}/stats",
            get(platform_stats),
        )
        .route(
            "/api/v1/platform/distribution/tenants/{tenant_id}/rules",
            get(platform_rules).post(super::distribution_policy::platform_create),
        )
        .route(
            "/api/v1/platform/distribution/tenants/{tenant_id}/rules/{id}",
            get(platform_rule)
                .patch(super::distribution_policy::platform_patch)
                .delete(super::distribution_policy::platform_delete),
        )
        .route("/api/v1/me/distribution/records", get(my_records))
        .route("/api/v1/me/distribution/records/{id}", get(my_record))
        .route("/api/v1/me/distribution/stats", get(my_stats))
        .layer(axum::extract::DefaultBodyLimit::max(8 * 1024))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_reject_implicit_scope_and_privilege_fields() {
        assert!(
            serde_json::from_value::<RecordQuery>(serde_json::json!({"tenant_id":Uuid::new_v4()}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<RuleQuery>(serde_json::json!({"platform_role":"root"}))
                .is_err()
        );
        assert!(page(Some(0), None).is_err());
        assert!(page(None, Some(101)).is_err());
    }
    #[test]
    fn monetary_stats_keep_heavy_read_admission() {
        for path in [
            "/api/v1/me/distribution/stats",
            "/api/v1/tenants/a/distribution/stats",
            "/api/v1/platform/distribution/tenants/a/stats",
        ] {
            assert_eq!(
                keycompute_types::console::classify("GET", path),
                Some(keycompute_types::console::ConsoleClass::HeavyRead)
            );
        }
    }
}
