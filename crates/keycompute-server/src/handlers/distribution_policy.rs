//! Tenant policy writes share one transaction-bound DAO with explicit root support.
use super::tenant_distribution::{ResourcePath, TenantPath};
use crate::{
    error::{ApiError, Result},
    extractors::{GlobalConsoleAuth, RequestId},
    state::AppState,
    tenant_access::TenantAdmin,
};
use axum::{
    Json,
    extract::{Path, State},
};
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use keycompute_auth::AuthorizationAction;
use keycompute_db::models::{
    distribution_policy::{self as dao, PolicyActor, PolicyPatch},
    tenant_control::TenantAuthzSnapshot,
};
use keycompute_db::{
    AuditContext, BeneficiaryScope, CreateDistributionRuleRequest, TenantDistributionRule,
};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use uuid::Uuid;
fn nullable<'de, D, T>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePolicy {
    pub name: String,
    pub description: Option<String>,
    pub commission_rate: BigDecimal,
    pub beneficiary_scope: Option<BeneficiaryScope>,
    pub beneficiary_id: Option<Uuid>,
    pub priority: Option<i32>,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_until: Option<DateTime<Utc>>,
    pub reason: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchPolicy {
    pub expected_updated_at: DateTime<Utc>,
    pub name: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub description: Option<Option<String>>,
    pub commission_rate: Option<BigDecimal>,
    pub priority: Option<i32>,
    pub is_active: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    pub effective_until: Option<Option<DateTime<Utc>>>,
    pub reason: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeletePolicy {
    pub expected_updated_at: DateTime<Utc>,
    pub reason: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultPolicy {
    pub name: String,
    pub commission_rate: BigDecimal,
    pub reason: String,
}
pub(super) fn map(e: keycompute_db::DbError) -> ApiError {
    match e {
        keycompute_db::DbError::NotFound { .. } => {
            ApiError::NotFound("Distribution policy not found".into())
        }
        keycompute_db::DbError::OptimisticConflict { .. } => {
            ApiError::Conflict("Policy or authorization changed; reload and retry".into())
        }
        keycompute_db::DbError::Other(m) if m.starts_with("invalid distribution policy:") => {
            ApiError::BadRequest(m)
        }
        keycompute_db::DbError::Other(m)
            if m.contains("authority")
                || m.contains("authorization")
                || m.contains("active actor")
                || m.contains("membership") =>
        {
            ApiError::Forbidden("Distribution policy access denied".into())
        }
        keycompute_db::DbError::DatabaseError(ref e) if e.to_string().contains("duplicate key") => {
            ApiError::Conflict("Distribution policy already exists".into())
        }
        _ => ApiError::ServiceUnavailable("Distribution policy operation unavailable".into()),
    }
}
fn db(s: &AppState) -> Result<&keycompute_db::DbRouter> {
    s.pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Distribution storage unavailable".into()))
}
fn tenant(a: &TenantAdmin, t: Uuid) -> Result<PolicyActor> {
    a.require_path_tenant(t)?;
    let scope = a.require(AuthorizationAction::ManageTenantResource)?;
    PolicyActor::tenant(
        scope,
        TenantAuthzSnapshot {
            token_version: a.auth().token_version,
            tenant_authz_version: a.auth().authz_version,
            membership_authz_version: a.auth().membership_authz_version,
        },
    )
    .map_err(map)
}
pub(super) fn platform(a: &GlobalConsoleAuth, t: Uuid) -> Result<PolicyActor> {
    PolicyActor::platform(
        a.require_platform(AuthorizationAction::ManagePlatform)?,
        t,
        a.token_version,
    )
    .map_err(map)
}
pub(super) fn platform_audit(a: &GlobalConsoleAuth, id: RequestId) -> AuditContext {
    AuditContext {
        actor_user_id: a.user_id,
        credential_kind: a.credential_kind,
        actor_platform_role: a.platform_role,
        actor_tenant_role: None,
        request_id: Some(id.0),
    }
}
async fn create(
    s: &AppState,
    who: PolicyActor,
    audit: &AuditContext,
    t: Uuid,
    req: CreatePolicy,
) -> Result<Json<TenantDistributionRule>> {
    let input = CreateDistributionRuleRequest {
        tenant_id: t,
        beneficiary_scope: req.beneficiary_scope.unwrap_or(BeneficiaryScope::Everyone),
        beneficiary_id: req.beneficiary_id,
        name: req.name,
        description: req.description,
        commission_rate: req.commission_rate,
        priority: req.priority,
        effective_from: req.effective_from,
        effective_until: req.effective_until,
    };
    Ok(Json(
        dao::create(db(s)?.write_conn(), who, audit, &input, &req.reason)
            .await
            .map_err(map)?,
    ))
}
async fn patch(
    s: &AppState,
    who: PolicyActor,
    audit: &AuditContext,
    id: Uuid,
    req: PatchPolicy,
) -> Result<Json<TenantDistributionRule>> {
    let input = PolicyPatch {
        expected_updated_at: req.expected_updated_at,
        name: req.name,
        description: req.description,
        commission_rate: req.commission_rate,
        priority: req.priority,
        is_active: req.is_active,
        effective_until: req.effective_until,
    };
    Ok(Json(
        dao::update(db(s)?.write_conn(), who, audit, id, &input, &req.reason)
            .await
            .map_err(map)?,
    ))
}
async fn delete(
    s: &AppState,
    who: PolicyActor,
    audit: &AuditContext,
    id: Uuid,
    req: DeletePolicy,
) -> Result<Json<Value>> {
    dao::delete(
        db(s)?.write_conn(),
        who,
        audit,
        id,
        req.expected_updated_at,
        &req.reason,
    )
    .await
    .map_err(map)?;
    Ok(Json(json!({"id":id,"deleted":true})))
}
async fn default(
    s: &AppState,
    who: PolicyActor,
    audit: &AuditContext,
    req: DefaultPolicy,
) -> Result<Json<TenantDistributionRule>> {
    Ok(Json(
        dao::upsert_default(
            db(s)?.write_conn(),
            who,
            audit,
            &req.name,
            req.commission_rate,
            &req.reason,
        )
        .await
        .map_err(map)?,
    ))
}
pub async fn tenant_create(
    a: TenantAdmin,
    id: RequestId,
    Path(p): Path<TenantPath>,
    State(s): State<AppState>,
    Json(req): Json<CreatePolicy>,
) -> Result<Json<TenantDistributionRule>> {
    create(&s, tenant(&a, p.tenant_id)?, &a.audit(id), p.tenant_id, req).await
}
pub async fn tenant_patch(
    a: TenantAdmin,
    id: RequestId,
    Path(p): Path<ResourcePath>,
    State(s): State<AppState>,
    Json(req): Json<PatchPolicy>,
) -> Result<Json<TenantDistributionRule>> {
    patch(&s, tenant(&a, p.tenant_id)?, &a.audit(id), p.id, req).await
}
pub async fn tenant_delete(
    a: TenantAdmin,
    id: RequestId,
    Path(p): Path<ResourcePath>,
    State(s): State<AppState>,
    Json(req): Json<DeletePolicy>,
) -> Result<Json<Value>> {
    delete(&s, tenant(&a, p.tenant_id)?, &a.audit(id), p.id, req).await
}
pub async fn tenant_default(
    a: TenantAdmin,
    id: RequestId,
    Path(p): Path<TenantPath>,
    State(s): State<AppState>,
    Json(req): Json<DefaultPolicy>,
) -> Result<Json<TenantDistributionRule>> {
    default(&s, tenant(&a, p.tenant_id)?, &a.audit(id), req).await
}
pub async fn platform_create(
    a: GlobalConsoleAuth,
    id: RequestId,
    Path(p): Path<TenantPath>,
    State(s): State<AppState>,
    Json(req): Json<CreatePolicy>,
) -> Result<Json<TenantDistributionRule>> {
    create(
        &s,
        platform(&a, p.tenant_id)?,
        &platform_audit(&a, id),
        p.tenant_id,
        req,
    )
    .await
}
pub async fn platform_patch(
    a: GlobalConsoleAuth,
    id: RequestId,
    Path(p): Path<ResourcePath>,
    State(s): State<AppState>,
    Json(req): Json<PatchPolicy>,
) -> Result<Json<TenantDistributionRule>> {
    patch(
        &s,
        platform(&a, p.tenant_id)?,
        &platform_audit(&a, id),
        p.id,
        req,
    )
    .await
}
pub async fn platform_delete(
    a: GlobalConsoleAuth,
    id: RequestId,
    Path(p): Path<ResourcePath>,
    State(s): State<AppState>,
    Json(req): Json<DeletePolicy>,
) -> Result<Json<Value>> {
    delete(
        &s,
        platform(&a, p.tenant_id)?,
        &platform_audit(&a, id),
        p.id,
        req,
    )
    .await
}
pub async fn platform_default(
    a: GlobalConsoleAuth,
    id: RequestId,
    Path(p): Path<TenantPath>,
    State(s): State<AppState>,
    Json(req): Json<DefaultPolicy>,
) -> Result<Json<TenantDistributionRule>> {
    default(&s, platform(&a, p.tenant_id)?, &platform_audit(&a, id), req).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patch_requires_revision_and_distinguishes_omission_from_null() {
        let base = json!({"expected_updated_at":"2026-01-01T00:00:00Z","reason":"test"});
        let p: PatchPolicy = serde_json::from_value(base.clone()).unwrap();
        assert!(p.effective_until.is_none());
        let mut clear = base.clone();
        clear["effective_until"] = Value::Null;
        assert_eq!(
            serde_json::from_value::<PatchPolicy>(clear)
                .unwrap()
                .effective_until,
            Some(None)
        );
        let mut forbidden = base;
        forbidden["tenant_id"] = json!(Uuid::new_v4());
        assert!(serde_json::from_value::<PatchPolicy>(forbidden).is_err());
        assert!(serde_json::from_value::<DeletePolicy>(json!({"reason":"no version"})).is_err());
    }
}
