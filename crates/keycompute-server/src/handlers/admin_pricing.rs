//! Platform pricing management handlers.
//!
//! These legacy URLs remain available only as root platform-console endpoints.
//! Tenant pricing is exposed by the tenant route phase through the scoped DAO
//! methods; this module never treats a selected tenant as a management grant.

use crate::{
    console_session_proof::ConsoleSessionProof,
    error::{ApiError, Result},
    extractors::{GlobalConsoleAuth, RequestId},
    handlers::pagination::{normalize_list_pagination, total_pages},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use keycompute_auth::AuthorizationAction;
use keycompute_db::AuditContext;
use keycompute_db::models::pricing_model::{
    BillingDimension, CreatePricingRequest, PlatformPricingScope, PricingModel, PricingScopeType,
    PricingTarget, UpdatePricingRequest,
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn require_platform_scope(auth: &GlobalConsoleAuth) -> Result<PlatformPricingScope> {
    auth.require_platform(AuthorizationAction::ManagePlatform)?;
    PlatformPricingScope::checked(auth.user_id, auth.credential_kind, auth.token_version)
        .map_err(|error| ApiError::Forbidden(error.to_string()))
}

pub(crate) async fn commit_pricing(
    state: &AppState,
    tx: sea_orm::DatabaseTransaction,
    proof: ConsoleSessionProof,
) -> Result<()> {
    let tx = proof.prepare_commit(tx).await?;
    let _fence = state.display_cache.mutation_guard();
    let committed = tx.commit().await;
    // An unconfirmed commit may still have persisted. Drop local price snapshots
    // before returning either the acknowledgement error or an expired-session error.
    state.pricing.clear_cache().await;
    committed.map_err(|_| {
        ApiError::ServiceUnavailable(
            "Pricing commit is unconfirmed; refresh records before retrying".into(),
        )
    })?;
    proof.check_deadline()
}

fn audit_context(auth: &GlobalConsoleAuth, request_id: RequestId) -> AuditContext {
    AuditContext {
        actor_user_id: auth.user_id,
        credential_kind: auth.credential_kind,
        actor_platform_role: auth.platform_role,
        actor_tenant_role: auth.tenant_role,
        request_id: Some(request_id.0),
    }
}

pub(crate) fn map_pricing_db_error(error: keycompute_db::DbError, operation: &str) -> ApiError {
    if error.is_duplicate() {
        return ApiError::Conflict("Pricing already exists for this scope/model/dimension".into());
    }
    if error.is_optimistic_conflict() {
        return ApiError::Conflict("Pricing or its authority changed; reload and retry".into());
    }
    if error.is_not_found() {
        return ApiError::NotFound("Pricing target not found".into());
    }
    if let keycompute_db::DbError::Other(message) = &error {
        if message.contains("current root pricing actor")
            || message.contains("current pricing actor")
        {
            return ApiError::Auth("Pricing authorization has changed".into());
        }
        if message.contains("authority")
            || message.contains("administrator")
            || message.contains("membership")
            || message.contains("JWT pricing")
        {
            return ApiError::Forbidden("Pricing administration is not permitted".into());
        }
        if message == "platform pricing rows cannot be deleted"
            || message == "target pricing tenant is inactive"
        {
            return ApiError::Conflict(message.clone());
        }
        if message.contains("pricing")
            || message.contains("DECIMAL")
            || message.contains("effective_until")
            || message.contains("non-negative")
            || message.contains("input_price")
            || message.contains("output_price")
        {
            return ApiError::BadRequest(message.clone());
        }
    }
    tracing::error!(operation, error = %error, "pricing operation failed");
    ApiError::ServiceUnavailable("Pricing storage is temporarily unavailable".into())
}

#[derive(Debug, Serialize)]
pub struct PricingInfo {
    pub id: Uuid,
    pub scope_type: String,
    pub tenant_id: Option<Uuid>,
    pub model_name: String,
    pub billing_dimension: String,
    pub currency: String,
    pub input_price_per_1k: String,
    pub output_price_per_1k: String,
    pub is_default: bool,
    pub is_effective: bool,
    pub effective_from: String,
    pub effective_until: Option<String>,
    pub created_at: String,
    pub version: i64,
}

impl From<PricingModel> for PricingInfo {
    fn from(pricing: PricingModel) -> Self {
        let is_effective = pricing.is_effective();
        Self {
            id: pricing.id,
            scope_type: pricing.scope_type.to_string(),
            tenant_id: pricing.tenant_id,
            model_name: pricing.model_name,
            billing_dimension: pricing.billing_dimension.as_str().to_string(),
            currency: pricing.currency,
            input_price_per_1k: pricing.input_price_per_1k.to_string(),
            output_price_per_1k: pricing.output_price_per_1k.to_string(),
            is_default: pricing.is_default,
            is_effective,
            effective_from: pricing.effective_from.to_rfc3339(),
            effective_until: pricing.effective_until.map(|value| value.to_rfc3339()),
            created_at: pricing.created_at.to_rfc3339(),
            version: pricing.version,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct PricingListQueryParams {
    pub search: Option<String>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub scope_type: Option<PricingScopeType>,
    pub tenant_id: Option<Uuid>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PricingTargetQueryParams {
    pub scope_type: Option<PricingScopeType>,
    pub tenant_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct PricingListResponse {
    pub pricing: Vec<PricingInfo>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePricingAdminRequest {
    pub scope_type: PricingScopeType,
    pub model_name: String,
    pub billing_dimension: String,
    pub tenant_id: Option<Uuid>,
    #[serde(default = "default_currency")]
    pub currency: String,
    pub input_price_per_1k: String,
    pub output_price_per_1k: String,
    #[serde(default)]
    pub is_default: bool,
    pub effective_from: Option<String>,
    pub effective_until: Option<String>,
}

fn default_currency() -> String {
    "CNY".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePricingAdminRequest {
    pub input_price_per_1k: Option<String>,
    pub output_price_per_1k: Option<String>,
    pub effective_until: Option<String>,
    pub expected_version: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct SetDefaultPricingAdminRequest {
    #[serde(default, alias = "pricing_ids")]
    pub model_ids: Vec<Uuid>,
}

pub(crate) fn parse_price(raw: &str, field: &str) -> Result<BigDecimal> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 256 {
        return Err(ApiError::BadRequest(format!(
            "{field} must be a bounded decimal number"
        )));
    }
    if let Some((_, exponent)) = raw.split_once(['e', 'E']) {
        let exponent = exponent
            .parse::<i64>()
            .map_err(|_| ApiError::BadRequest(format!("Invalid {field} exponent")))?;
        if !(-4096..=4096).contains(&exponent) {
            return Err(ApiError::BadRequest(format!(
                "{field} exponent is out of range"
            )));
        }
    }
    let value = raw
        .parse::<BigDecimal>()
        .map_err(|_| ApiError::BadRequest(format!("Invalid {field}: expected a decimal number")))?;
    keycompute_db::models::pricing_model::validate_pricing_amount(&value)
        .map_err(|error| ApiError::BadRequest(format!("{field}: {error}")))?;
    Ok(value)
}

pub(crate) fn parse_timestamp(raw: Option<&String>, field: &str) -> Result<Option<DateTime<Utc>>> {
    raw.map(|value| {
        DateTime::parse_from_rfc3339(value.trim())
            .map(|timestamp| timestamp.with_timezone(&Utc))
            .map_err(|_| ApiError::BadRequest(format!("Invalid {field}: expected RFC3339")))
    })
    .transpose()
}

fn request_target(scope_type: PricingScopeType, tenant_id: Option<Uuid>) -> Result<PricingTarget> {
    match (scope_type, tenant_id) {
        (PricingScopeType::Platform, None) => Ok(PricingTarget::Platform),
        (PricingScopeType::Tenant, Some(tenant_id)) => {
            PricingTarget::Tenant(tenant_id)
                .validate()
                .map_err(|error| ApiError::BadRequest(error.to_string()))?;
            Ok(PricingTarget::Tenant(tenant_id))
        }
        (PricingScopeType::Platform, Some(_)) => Err(ApiError::BadRequest(
            "platform pricing cannot specify tenant_id".into(),
        )),
        (PricingScopeType::Tenant, None) => Err(ApiError::BadRequest(
            "tenant pricing requires tenant_id".into(),
        )),
    }
}

fn target_from_query(
    auth: &GlobalConsoleAuth,
    scope_type: Option<PricingScopeType>,
    tenant_id: Option<Uuid>,
) -> Result<PricingTarget> {
    match (scope_type, tenant_id, auth.selected_tenant_id) {
        (Some(scope_type), tenant_id, _) => request_target(scope_type, tenant_id),
        (None, Some(tenant_id), _) => request_target(PricingScopeType::Tenant, Some(tenant_id)),
        (None, None, Some(tenant_id)) => request_target(PricingScopeType::Tenant, Some(tenant_id)),
        (None, None, None) => Ok(PricingTarget::Platform),
    }
}

fn validate_model_name(value: &str) -> Result<String> {
    keycompute_db::models::pricing_model::validate_pricing_model_name(value)
        .map_err(|error| ApiError::BadRequest(error.to_string()))
}

pub(crate) fn create_request(
    req: CreatePricingAdminRequest,
) -> Result<(PricingTarget, CreatePricingRequest)> {
    let target = request_target(req.scope_type, req.tenant_id)?;
    let billing_dimension = req
        .billing_dimension
        .parse::<BillingDimension>()
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    Ok((
        target,
        CreatePricingRequest {
            scope_type: req.scope_type,
            tenant_id: req.tenant_id,
            model_name: validate_model_name(&req.model_name)?,
            billing_dimension,
            currency: Some(req.currency),
            input_price_per_1k: parse_price(&req.input_price_per_1k, "input_price_per_1k")?,
            output_price_per_1k: parse_price(&req.output_price_per_1k, "output_price_per_1k")?,
            is_default: Some(req.is_default),
            effective_from: parse_timestamp(req.effective_from.as_ref(), "effective_from")?,
            effective_until: parse_timestamp(req.effective_until.as_ref(), "effective_until")?,
        },
    ))
}

pub(crate) fn platform_update_request(
    req: UpdatePricingAdminRequest,
) -> Result<UpdatePricingRequest> {
    let input_price_per_1k = req
        .input_price_per_1k
        .as_deref()
        .map(|value| parse_price(value, "input_price_per_1k"))
        .transpose()?;
    let output_price_per_1k = req
        .output_price_per_1k
        .as_deref()
        .map(|value| parse_price(value, "output_price_per_1k"))
        .transpose()?;
    let effective_until = parse_timestamp(req.effective_until.as_ref(), "effective_until")?;
    let expected_version = req
        .expected_version
        .ok_or_else(|| ApiError::BadRequest("expected_version is required".into()))?;
    if expected_version <= 0 {
        return Err(ApiError::BadRequest(
            "expected_version must be positive".into(),
        ));
    }
    if input_price_per_1k.is_none()
        && output_price_per_1k.is_none()
        && req.effective_until.is_none()
    {
        return Err(ApiError::BadRequest(
            "At least one pricing field must be provided".into(),
        ));
    }
    Ok(UpdatePricingRequest {
        input_price_per_1k,
        output_price_per_1k,
        effective_until,
        expected_version,
    })
}

async fn mutation_target(
    tx: &sea_orm::DatabaseTransaction,
    scope: PlatformPricingScope,
    auth: &GlobalConsoleAuth,
    params: &PricingTargetQueryParams,
    id: Uuid,
) -> Result<PricingTarget> {
    if params.scope_type.is_some() || params.tenant_id.is_some() {
        return target_from_query(auth, params.scope_type, params.tenant_id);
    }
    PricingModel::resolve_platform_target(tx, scope, id)
        .await
        .map_err(|error| map_pricing_db_error(error, "resolve pricing target"))?
        .ok_or_else(|| ApiError::NotFound(format!("Pricing not found: {id}")))
}

pub async fn list_pricing(
    auth: GlobalConsoleAuth,
    _request_id: RequestId,
    State(state): State<AppState>,
    Query(params): Query<PricingListQueryParams>,
) -> Result<Json<PricingListResponse>> {
    let scope = require_platform_scope(&auth)?;
    let proof = ConsoleSessionProof::from_context(&auth)?;
    let target = target_from_query(&auth, params.scope_type, params.tenant_id)?;
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;
    let (page, page_size, offset) =
        normalize_list_pagination(params.page, params.page_size, params.limit, params.offset);
    let rows = PricingModel::find_platform_filtered(
        pool.write_conn(),
        scope,
        target,
        params.search.as_deref(),
        page_size,
        offset,
    )
    .await
    .map_err(|error| map_pricing_db_error(error, "list pricing"))?;
    let total = PricingModel::count_platform_filtered(
        pool.write_conn(),
        scope,
        target,
        params.search.as_deref(),
    )
    .await
    .map_err(|error| map_pricing_db_error(error, "count pricing"))?;
    proof.verify_current(pool.write_conn()).await?;
    Ok(Json(PricingListResponse {
        pricing: rows.into_iter().map(PricingInfo::from).collect(),
        total,
        page,
        page_size,
        total_pages: total_pages(total, page_size),
    }))
}

pub async fn create_pricing(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    State(state): State<AppState>,
    Json(req): Json<CreatePricingAdminRequest>,
) -> Result<Json<serde_json::Value>> {
    let scope = require_platform_scope(&auth)?;
    let proof = ConsoleSessionProof::from_context(&auth)?;
    let (target, request) = create_request(req)?;
    let tx = proof.begin(&state).await?;
    let row = PricingModel::create_platform(
        &tx,
        scope,
        target,
        &request,
        &audit_context(&auth, request_id),
    )
    .await
    .map_err(|error| map_pricing_db_error(error, "create pricing"))?;
    commit_pricing(&state, tx, proof).await?;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing created",
        "pricing_id": row.id,
        "model_name": row.model_name,
        "billing_dimension": row.billing_dimension.as_str(),
        "input_price_per_1k": row.input_price_per_1k.to_string(),
        "output_price_per_1k": row.output_price_per_1k.to_string(),
        "is_default": row.is_default,
        "version": row.version,
        "created_by": auth.user_id,
    })))
}

pub async fn update_pricing(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(pricing_id): Path<Uuid>,
    State(state): State<AppState>,
    Query(target_query): Query<PricingTargetQueryParams>,
    Json(req): Json<UpdatePricingAdminRequest>,
) -> Result<Json<serde_json::Value>> {
    let scope = require_platform_scope(&auth)?;
    let proof = ConsoleSessionProof::from_context(&auth)?;
    let request = platform_update_request(req)?;
    let tx = proof.begin(&state).await?;
    let target = mutation_target(&tx, scope, &auth, &target_query, pricing_id).await?;
    let row = PricingModel::update_platform(
        &tx,
        scope,
        target,
        pricing_id,
        &request,
        &audit_context(&auth, request_id),
    )
    .await
    .map_err(|error| map_pricing_db_error(error, "update pricing"))?;
    commit_pricing(&state, tx, proof).await?;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing updated",
        "pricing_id": row.id,
        "version": row.version,
        "updated_by": auth.user_id,
    })))
}

pub async fn delete_pricing(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(pricing_id): Path<Uuid>,
    State(state): State<AppState>,
    Query(target_query): Query<PricingTargetQueryParams>,
) -> Result<Json<serde_json::Value>> {
    let scope = require_platform_scope(&auth)?;
    let proof = ConsoleSessionProof::from_context(&auth)?;
    let tx = proof.begin(&state).await?;
    let target = mutation_target(&tx, scope, &auth, &target_query, pricing_id).await?;
    PricingModel::delete_platform(
        &tx,
        scope,
        target,
        pricing_id,
        &audit_context(&auth, request_id),
    )
    .await
    .map_err(|error| map_pricing_db_error(error, "delete pricing"))?;
    commit_pricing(&state, tx, proof).await?;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing deleted",
        "pricing_id": pricing_id,
        "deleted_by": auth.user_id,
    })))
}

pub async fn make_pricing_default(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(pricing_id): Path<Uuid>,
    State(state): State<AppState>,
    Query(target_query): Query<PricingTargetQueryParams>,
) -> Result<Json<serde_json::Value>> {
    let scope = require_platform_scope(&auth)?;
    let proof = ConsoleSessionProof::from_context(&auth)?;
    let tx = proof.begin(&state).await?;
    let target = mutation_target(&tx, scope, &auth, &target_query, pricing_id).await?;
    let row = PricingModel::make_default_platform(
        &tx,
        scope,
        target,
        pricing_id,
        &audit_context(&auth, request_id),
    )
    .await
    .map_err(|error| map_pricing_db_error(error, "set pricing default"))?;
    commit_pricing(&state, tx, proof).await?;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing set as default",
        "pricing_id": row.id,
        "version": row.version,
    })))
}

pub async fn set_default_pricing(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    State(state): State<AppState>,
    Query(target_query): Query<PricingTargetQueryParams>,
    Json(req): Json<SetDefaultPricingAdminRequest>,
) -> Result<Json<serde_json::Value>> {
    let scope = require_platform_scope(&auth)?;
    let proof = ConsoleSessionProof::from_context(&auth)?;
    let target = target_from_query(&auth, target_query.scope_type, target_query.tenant_id)?;
    if req.model_ids.is_empty() {
        return Err(ApiError::BadRequest(
            "model_ids must contain at least one pricing id".into(),
        ));
    }
    let tx = proof.begin(&state).await?;
    let rows = PricingModel::batch_make_defaults_platform(
        &tx,
        scope,
        target,
        &req.model_ids,
        &audit_context(&auth, request_id),
    )
    .await
    .map_err(|error| map_pricing_db_error(error, "set pricing defaults"))?;
    commit_pricing(&state, tx, proof).await?;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing defaults set",
        "pricing_ids": rows.into_iter().map(|row| row.id).collect::<Vec<_>>(),
        "set_by": auth.user_id,
    })))
}

#[cfg(test)]
mod tests {
    use super::{
        CreatePricingAdminRequest, SetDefaultPricingAdminRequest, UpdatePricingAdminRequest,
        create_request, map_pricing_db_error, parse_price, parse_timestamp,
        platform_update_request, validate_model_name,
    };
    use uuid::Uuid;

    #[test]
    fn pricing_model_names_do_not_select_an_execution_mode() {
        for model in [
            "gemma3:270m",
            "node:gemma3:270m",
            "NODE:gemma3:270m",
            "provider:model",
        ] {
            assert_eq!(validate_model_name(model).unwrap(), model);
        }
    }

    #[test]
    fn pricing_creation_requires_an_existing_active_tenant() {
        assert!(matches!(
            map_pricing_db_error(
                keycompute_db::DbError::Other("target pricing tenant is inactive".into()),
                "create pricing"
            ),
            crate::error::ApiError::Conflict(_)
        ));
        assert!(matches!(
            map_pricing_db_error(
                keycompute_db::DbError::not_found("pricing tenant", Uuid::new_v4()),
                "create pricing"
            ),
            crate::error::ApiError::NotFound(_)
        ));
    }

    #[test]
    fn platform_request_rejects_inconsistent_target() {
        let request = CreatePricingAdminRequest {
            scope_type: keycompute_db::models::pricing_model::PricingScopeType::Platform,
            model_name: "model".into(),
            billing_dimension: "provideraccount".into(),
            tenant_id: Some(Uuid::new_v4()),
            currency: "USD".into(),
            input_price_per_1k: "1".into(),
            output_price_per_1k: "2".into(),
            is_default: false,
            effective_from: None,
            effective_until: None,
        };
        assert!(create_request(request).is_err());
    }

    #[test]
    fn pricing_inputs_reject_negative_or_malformed_values() {
        assert!(parse_price("-0.1", "input_price").is_err());
        assert!(parse_price("0.125", "input_price").is_ok());
        assert!(parse_price("not-a-number", "input_price").is_err());
        assert!(parse_price("10000000000", "input_price").is_err());
        assert!(parse_price("1.23000000000", "input_price").is_ok());
        assert!(parse_timestamp(Some(&"not-a-date".to_string()), "effective_until").is_err());
        assert!(validate_model_name(&"模".repeat(100)).is_ok());
        for malformed in [
            "1e9223372036854775807",
            "1e-9223372036854775808",
            "1e9999999999999999999999",
        ] {
            assert!(parse_price(malformed, "input_price").is_err());
        }
        assert!(parse_price(&"0".repeat(257), "input_price").is_err());
        assert!(parse_price("0.12345678901", "input_price").is_err());
        assert!(parse_price("9999999999.9999999999", "input_price").is_ok());
    }

    #[test]
    fn update_requires_version_and_a_field() {
        assert!(
            platform_update_request(UpdatePricingAdminRequest {
                input_price_per_1k: None,
                output_price_per_1k: None,
                effective_until: None,
                expected_version: None,
            })
            .is_err()
        );
    }

    #[test]
    fn batch_default_payload_accepts_legacy_and_explicit_id_field_names() {
        let id = Uuid::new_v4();
        let request: SetDefaultPricingAdminRequest =
            serde_json::from_value(serde_json::json!({"pricing_ids": [id]})).unwrap();
        assert_eq!(request.model_ids, vec![id]);
        let original: SetDefaultPricingAdminRequest =
            serde_json::from_value(serde_json::json!({"model_ids": [id]})).unwrap();
        assert_eq!(original.model_ids, vec![id]);
    }
}
