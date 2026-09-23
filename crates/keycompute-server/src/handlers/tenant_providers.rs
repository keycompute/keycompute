//! Tenant-scoped provider account management.
//!
//! The tenant path is authoritative. Every handler derives its scope from the
//! verified `TenantAdmin` extractor and never accepts tenant or visibility
//! selectors from the request body.

use crate::{
    error::{ApiError, Result},
    extractors::RequestId,
    handlers::{
        admin_account::{
            self, AccountInfo, AccountProbePolicy, normalize_api_capabilities,
            normalize_model_list, refresh_models_update_request, validate_account_priority,
            validate_account_rate_limits,
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
    AuditContext,
    models::account::{
        Account, AccountListFilter, AccountManagementScope, CreateAccountRequest,
        ProviderAuthzSnapshot, UpdateAccountRequest,
    },
};
use sea_orm::{DatabaseTransaction, TransactionTrait};

use llm_protocol_provider::{ProtocolType, normalize_base_url};
use serde::{Deserialize, Serialize};

use uuid::Uuid;

pub const TENANT_PROVIDER_BODY_LIMIT_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
pub struct TenantAccountPath {
    pub tenant_id: Uuid,
    pub id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct TenantPath {
    pub tenant_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantAccountListQuery {
    pub search: Option<String>,
    pub provider: Option<String>,
    pub status: Option<String>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTenantAccountRequest {
    pub name: String,
    pub provider: String,
    pub api_key: String,
    pub api_base: Option<String>,
    pub models: Vec<String>,
    pub api_capabilities: Option<Vec<String>>,
    pub rpm_limit: Option<i32>,
    pub tpm_limit: Option<i32>,
    pub priority: Option<i32>,
    pub pool_enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTenantAccountRequest {
    pub name: Option<String>,
    pub api_key: Option<String>,
    pub api_base: Option<String>,
    pub models: Option<Vec<String>>,
    pub api_capabilities: Option<Vec<String>>,
    pub rpm_limit: Option<i32>,
    pub tpm_limit: Option<i32>,
    pub is_active: Option<bool>,
    pub priority: Option<i32>,
    pub pool_enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct TenantAccountListResponse {
    pub accounts: Vec<AccountInfo>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

fn pool(state: &AppState) -> Result<&keycompute_db::DbRouter> {
    state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Provider storage unavailable".into()))
}

fn scope(access: &TenantAdmin) -> Result<keycompute_types::TenantScope> {
    access.require(AuthorizationAction::ManageTenantResource)
}

fn snapshot(access: &TenantAdmin) -> Result<ProviderAuthzSnapshot> {
    let auth = access.auth();
    let tenant_version = auth.authz_version;
    let membership_version = auth.membership_authz_version;
    if tenant_version <= 0 || membership_version <= 0 {
        return Err(ApiError::Forbidden(
            "Current tenant authorization is unavailable".into(),
        ));
    }
    Ok(ProviderAuthzSnapshot::tenant(
        auth.token_version,
        tenant_version,
        membership_version,
    ))
}

fn parse_status(value: Option<&str>) -> Result<Option<bool>> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("all") => Ok(None),
        Some("active" | "enabled") => Ok(Some(true)),
        Some("inactive" | "disabled") => Ok(Some(false)),
        Some(value) => Err(ApiError::BadRequest(format!(
            "Invalid account status '{value}', expected active or inactive"
        ))),
    }
}

fn audit(access: &TenantAdmin, request_id: RequestId) -> AuditContext {
    access.audit(request_id)
}

async fn commit(state: &AppState, tx: DatabaseTransaction) -> Result<()> {
    let _fence = state.display_cache.mutation_guard();
    tx.commit()
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Provider commit failed".into()))
}
async fn write_transaction(state: &AppState) -> Result<DatabaseTransaction> {
    pool(state)?
        .begin()
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Provider storage unavailable".into()))
}
fn scope_error(error: keycompute_db::DbError) -> ApiError {
    admin_account::account_scope_error(error)
}

fn encrypt_api_key(api_key: &str) -> Result<(String, String)> {
    if let Some(_crypto) = keycompute_runtime::crypto::global_crypto() {
        let encrypted = keycompute_runtime::crypto::encrypt_api_key(api_key)
            .map_err(|error| ApiError::Internal(format!("Failed to encrypt API key: {error}")))?;
        Ok((
            encrypted.into_inner(),
            keycompute_runtime::crypto::ApiKeyCrypto::create_preview(api_key),
        ))
    } else {
        tracing::warn!(
            "Global crypto key not set; storing the tenant account key in development mode"
        );
        Ok((
            api_key.to_owned(),
            keycompute_runtime::crypto::ApiKeyCrypto::create_preview(api_key),
        ))
    }
}

fn account_request(
    tenant_id: Uuid,
    request: CreateTenantAccountRequest,
) -> Result<CreateAccountRequest> {
    validate_account_priority(request.priority)?;
    validate_account_rate_limits(request.rpm_limit, request.tpm_limit)?;
    let protocol = ProtocolType::parse(&request.provider).ok_or_else(|| {
        ApiError::BadRequest("Unsupported protocol; expected openai or anthropic".into())
    })?;
    let models = normalize_model_list(request.models);
    if models.is_empty() {
        return Err(ApiError::BadRequest(
            "At least one model must be specified".into(),
        ));
    }
    let endpoint = match request.api_base.as_deref() {
        Some(value) if value.trim().is_empty() => String::new(),
        Some(value) => normalize_base_url(value).map_err(ApiError::BadRequest)?,
        None => String::new(),
    };
    let capabilities = normalize_api_capabilities(protocol, request.api_capabilities.as_deref())
        .map_err(ApiError::BadRequest)?;
    let (encrypted, preview) = encrypt_api_key(&request.api_key)?;
    Ok(CreateAccountRequest {
        tenant_id,
        provider: protocol.as_str().to_owned(),
        name: request.name,
        endpoint,
        upstream_api_key_encrypted: encrypted,
        upstream_api_key_preview: preview,
        rpm_limit: request.rpm_limit,
        tpm_limit: request.tpm_limit,
        priority: request.priority,
        models_supported: models,
        api_capabilities: capabilities,
        visibility: Some("tenant".into()),
        pool_enabled: request.pool_enabled,
    })
}

fn account_view(
    state: &AppState,
    row: keycompute_db::models::account::AccountManagementView,
) -> AccountInfo {
    admin_account::account_info(state, row)
}

async fn find_account(
    state: &AppState,
    scope: keycompute_types::TenantScope,
    id: Uuid,
) -> Result<AccountInfo> {
    let row = Account::find_in_tenant(pool(state)?.write_conn(), scope, id)
        .await
        .map_err(scope_error)?
        .ok_or_else(|| ApiError::NotFound("Account not found".into()))?;
    Ok(account_view(state, row))
}

pub async fn list_accounts(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(query): Query<TenantAccountListQuery>,
) -> Result<Json<TenantAccountListResponse>> {
    access.require_path_tenant(path.tenant_id)?;
    let scope = scope(&access)?;
    let enabled = parse_status(query.status.as_deref())?;
    let (page, page_size, offset) =
        normalize_list_pagination(query.page, query.page_size, None, None);
    let filter = AccountListFilter {
        provider: query.provider,
        enabled,
        search: query.search,
        tenant_id: Some(path.tenant_id),
    };
    let db = pool(&state)?.write_conn();
    let accounts = Account::list_in_tenant(db, scope, &filter, page_size, offset)
        .await
        .map_err(scope_error)?
        .into_iter()
        .map(|row| account_view(&state, row))
        .collect();
    let total = Account::count_in_tenant(db, scope, &filter)
        .await
        .map_err(scope_error)?;
    Ok(Json(TenantAccountListResponse {
        accounts,
        total,
        page,
        page_size,
        total_pages: total_pages(total, page_size),
    }))
}

pub async fn get_account(
    access: TenantAdmin,
    Path(path): Path<TenantAccountPath>,
    State(state): State<AppState>,
) -> Result<Json<AccountInfo>> {
    access.require_path_tenant(path.tenant_id)?;
    Ok(Json(find_account(&state, scope(&access)?, path.id).await?))
}

pub async fn create_account(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    request_id: RequestId,
    Json(request): Json<CreateTenantAccountRequest>,
) -> Result<Json<AccountInfo>> {
    access.require_path_tenant(path.tenant_id)?;
    let scope = scope(&access)?;
    let authz = snapshot(&access)?;
    let request = account_request(path.tenant_id, request)?;
    let tx = write_transaction(&state).await?;
    let created =
        Account::create_in_tenant(&tx, scope, &request, &audit(&access, request_id), authz)
            .await
            .map_err(scope_error)?;
    commit(&state, tx).await?;
    Ok(Json(find_account(&state, scope, created.id).await?))
}

pub async fn update_account(
    access: TenantAdmin,
    Path(path): Path<TenantAccountPath>,
    State(state): State<AppState>,
    request_id: RequestId,
    Json(request): Json<UpdateTenantAccountRequest>,
) -> Result<Json<AccountInfo>> {
    access.require_path_tenant(path.tenant_id)?;
    let scope = scope(&access)?;
    let authz = snapshot(&access)?;
    validate_account_priority(request.priority)?;
    validate_account_rate_limits(request.rpm_limit, request.tpm_limit)?;
    let audit = audit(&access, request_id);
    let existing = Account::prepare_update(
        pool(&state)?,
        AccountManagementScope::Tenant(scope),
        path.id,
        &audit,
        authz,
    )
    .await
    .map_err(scope_error)?
    .ok_or_else(|| ApiError::NotFound("Account not found".into()))?;
    let protocol = ProtocolType::parse(&existing.provider)
        .ok_or_else(|| ApiError::Conflict("Account protocol is no longer supported".into()))?;
    let endpoint = match request.api_base.as_deref() {
        Some(value) if value.trim().is_empty() => Some(String::new()),
        Some(value) => Some(normalize_base_url(value).map_err(ApiError::BadRequest)?),
        None => None,
    };
    let capabilities = request
        .api_capabilities
        .as_deref()
        .map(|values| normalize_api_capabilities(protocol, Some(values)))
        .transpose()
        .map_err(ApiError::BadRequest)?;
    let models = request.models.map(normalize_model_list);
    let (encrypted, preview) = match request.api_key.as_deref() {
        Some(value) => {
            let (encrypted, preview) = encrypt_api_key(value)?;
            (Some(encrypted), Some(preview))
        }
        None => (None, None),
    };
    let update = UpdateAccountRequest {
        tenant_id: None,
        name: request.name,
        endpoint,
        upstream_api_key_encrypted: encrypted,
        upstream_api_key_preview: preview,
        rpm_limit: request.rpm_limit,
        tpm_limit: request.tpm_limit,
        priority: request.priority,
        enabled: request.is_active,
        models_supported: models,
        api_capabilities: capabilities,
        visibility: None,
        pool_enabled: request.pool_enabled,
    };
    let tx = write_transaction(&state).await?;
    let updated = Account::update_in_tenant(
        &tx,
        scope,
        path.id,
        &update,
        existing.upstream_config_version,
        &audit,
        authz,
    )
    .await
    .map_err(scope_error)?;
    commit(&state, tx).await?;
    Ok(Json(find_account(&state, scope, updated.id).await?))
}

pub async fn delete_account(
    access: TenantAdmin,
    Path(path): Path<TenantAccountPath>,
    State(state): State<AppState>,
    request_id: RequestId,
) -> Result<Json<serde_json::Value>> {
    access.require_path_tenant(path.tenant_id)?;
    let scope = scope(&access)?;
    let tx = write_transaction(&state).await?;
    Account::delete_in_tenant(
        &tx,
        scope,
        path.id,
        &audit(&access, request_id),
        snapshot(&access)?,
    )
    .await
    .map_err(scope_error)?;
    commit(&state, tx).await?;
    state
        .responses_affinity
        .write()
        .await
        .retain(|_, affinity| affinity.account_id != path.id);
    state.provider_health.forget_account_health(&path.id);
    Ok(Json(serde_json::json!({
        "success": true,
        "account_id": path.id
    })))
}

pub async fn test_account(
    access: TenantAdmin,
    Path(path): Path<TenantAccountPath>,
    State(state): State<AppState>,
    request_id: RequestId,
) -> Result<Json<serde_json::Value>> {
    access.require_path_tenant(path.tenant_id)?;
    let scope = AccountManagementScope::Tenant(scope(&access)?);
    let authz = snapshot(&access)?;
    let audit = audit(&access, request_id);
    Account::prepare_probe(pool(&state)?, scope, path.id, &audit, authz)
        .await
        .map_err(scope_error)?
        .ok_or_else(|| ApiError::NotFound("Account not found".into()))?;
    let result = admin_account::probe_account_for_monitoring_with_policy(
        &state,
        path.id,
        AccountProbePolicy::Explicit,
        admin_account::AccountProbeAuthority::Console(crate::financial_auth::tenant_scope(
            &access,
            path.tenant_id,
        )?),
    )
    .await?
    .ok_or_else(|| ApiError::NotFound("Account not found".into()))?;
    Ok(Json(result))
}

pub async fn refresh_account(
    access: TenantAdmin,
    Path(path): Path<TenantAccountPath>,
    State(state): State<AppState>,
    request_id: RequestId,
) -> Result<Json<serde_json::Value>> {
    access.require_path_tenant(path.tenant_id)?;
    let tenant_scope = scope(&access)?;
    let management_scope = AccountManagementScope::Tenant(tenant_scope);
    let authz = snapshot(&access)?;
    let audit = audit(&access, request_id);
    let pool = pool(&state)?;
    Account::prepare_probe(pool, management_scope, path.id, &audit, authz)
        .await
        .map_err(scope_error)?
        .ok_or_else(|| ApiError::NotFound("Account not found".into()))?;
    let account =
        Account::load_authorized_probe(pool.write_conn(), management_scope, path.id, authz)
            .await
            .map_err(scope_error)?
            .ok_or_else(|| {
                ApiError::Forbidden("Account management authorization is no longer active".into())
            })?;
    let protocol = ProtocolType::parse(&account.provider)
        .ok_or_else(|| ApiError::Conflict("Account protocol is no longer supported".into()))?;
    let endpoint = if account.endpoint.is_empty() {
        protocol.default_endpoint().to_string()
    } else {
        normalize_base_url(&account.endpoint)
            .map_err(|_| ApiError::Conflict("Stored account endpoint is invalid".into()))?
    };
    let key = admin_account::decrypt_account_api_key(&account.upstream_api_key_encrypted)?;
    let transport = state
        .http_proxy
        .client_for_provider_and_account(protocol.as_str(), Some(path.id));
    let models =
        admin_account::fetch_upstream_models(protocol, transport.as_ref(), &endpoint, &key)
            .await
            .map_err(|error| ApiError::Internal(format!("Failed to fetch models: {error}")))?;
    let tx = write_transaction(&state).await?;
    let updated = Account::update_in_tenant(
        &tx,
        tenant_scope,
        path.id,
        &refresh_models_update_request(models),
        account.upstream_config_version,
        &audit,
        authz,
    )
    .await
    .map_err(scope_error)?;
    commit(&state, tx).await?;
    state
        .responses_affinity
        .write()
        .await
        .retain(|_, affinity| affinity.account_id != path.id);
    Ok(Json(serde_json::json!({
        "success": true,
        "account_id": updated.id,
        "previous_models": account.models_supported,
        "updated_models": updated.models_supported
    })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/tenants/{tenant_id}/accounts",
            get(list_accounts).post(create_account),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/accounts/{id}",
            get(get_account)
                .put(update_account)
                .patch(update_account)
                .delete(delete_account),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/accounts/{id}/test",
            post(test_account),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/accounts/{id}/refresh",
            post(refresh_account),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            TENANT_PROVIDER_BODY_LIMIT_BYTES,
        ))
}
