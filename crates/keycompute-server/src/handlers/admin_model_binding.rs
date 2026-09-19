//! Tenant model-binding administration.
//!
//! These endpoints intentionally use the writer connection for every
//! authorization-sensitive read and write.  A model binding is not an
//! alternate account pool: it is one explicit account selected for one
//! tenant/capability/model tuple.

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    handlers::pagination::{normalize_list_pagination, total_pages},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::Duration as ChronoDuration;
use keycompute_auth::Permission;
use keycompute_db::models::{
    account::Account,
    model_binding::{
        AccountModelHealth, AccountModelHealthProbe, CreateModelBindingRequest as DbCreateRequest,
        MODEL_BINDING_CAPABILITY, MODEL_BINDING_MAX_PAGE_SIZE, ModelBinding,
        UpdateModelBindingRequest as DbUpdateRequest,
    },
};
use llm_protocol_provider::{ProtocolType, UpstreamMessage, UpstreamRequest, normalize_base_url};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::sync::Semaphore;
use uuid::Uuid;

const DEFAULT_PROBE_TIMEOUT_MS: u64 = 2_000;
const MAX_PROBE_TIMEOUT_MS: u64 = 10_000;
use crate::model_binding::HEALTH_TTL_SECS;
const MODEL_PROBE_CONCURRENCY: usize = 8;

static MODEL_PROBE_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MODEL_PROBE_CONCURRENCY)));

#[derive(Debug, Deserialize)]
pub struct ModelBindingListQuery {
    pub tenant_id: Option<Uuid>,
    pub model: Option<String>,
    pub api_capability: Option<String>,
    pub enabled: Option<bool>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ModelBindingInfoResponse {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub api_capability: String,
    pub model: String,
    pub account_id: Uuid,
    pub account_name: Option<String>,
    pub enabled: bool,
    pub revision: i64,
    pub health_status: Option<String>,
    pub health_reason_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct ModelBindingPageResponse {
    pub bindings: Vec<ModelBindingInfoResponse>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateModelBindingRequest {
    pub tenant_id: Uuid,
    pub api_capability: Option<String>,
    pub model: String,
    pub account_id: Uuid,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateModelBindingRequest {
    pub model: Option<String>,
    pub account_id: Option<Uuid>,
    pub enabled: Option<bool>,
    pub expected_revision: i64,
}

#[derive(Debug, Deserialize)]
pub struct ModelBindingProbeRequest {
    pub model: String,
    #[serde(default = "default_capability")]
    pub api_capability: String,
    pub timeout_ms: Option<u64>,
}

fn default_capability() -> String {
    MODEL_BINDING_CAPABILITY.to_string()
}

#[derive(Debug, Deserialize, Default)]
pub struct RevisionQuery {
    pub expected_revision: i64,
}

#[derive(Debug, Serialize)]
pub struct ModelBindingProbeResponse {
    pub binding_id: Uuid,
    pub account_id: Uuid,
    pub model: String,
    pub status: String,
    pub reason_code: Option<String>,
    pub checked_at: String,
    pub expires_at: String,
    pub generation: i64,
}

#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub message: String,
}

fn require_management(auth: &AuthExtractor) -> Result<()> {
    // ManageProviders is intentionally checked from the permission list, not
    // the role string. API keys only carry UseApi and therefore cannot gain
    // access even when their owner is an administrator.
    if !auth.has_permission(&Permission::ManageProviders) {
        return Err(ApiError::Forbidden(
            "Provider management permission required".to_string(),
        ));
    }
    Ok(())
}

fn requested_tenant(auth: &AuthExtractor, requested: Option<Uuid>) -> Result<Option<Uuid>> {
    if auth.is_admin() {
        return Ok(requested);
    }
    if let Some(requested) = requested
        && requested != auth.tenant_id
    {
        return Err(ApiError::Forbidden(
            "Cross-tenant model binding access is not permitted".to_string(),
        ));
    }
    Ok(Some(auth.tenant_id))
}

fn map_db_error(error: keycompute_db::DbError) -> ApiError {
    match error {
        keycompute_db::DbError::OptimisticConflict { .. } => {
            ApiError::Conflict("Model binding changed; reload and retry".to_string())
        }
        keycompute_db::DbError::DatabaseError(db_error) => {
            let message = db_error.to_string().to_ascii_lowercase();
            if message.contains("unique") || message.contains("duplicate") {
                ApiError::Conflict("A binding already exists for this tenant and model".to_string())
            } else {
                ApiError::Internal("Failed to persist model binding".to_string())
            }
        }
        keycompute_db::DbError::Other(message)
            if message.contains("only support")
                || message.contains("non-empty")
                || message.contains("canonical")
                || message.contains("unsupported")
                || message.contains("must be") =>
        {
            ApiError::BadRequest(message)
        }
        keycompute_db::DbError::Other(message)
            if message.contains("not visible")
                || message.contains("not configured for this model") =>
        {
            ApiError::Forbidden("Account is not visible or does not support this model".to_string())
        }
        keycompute_db::DbError::NotFound { entity, id } => {
            ApiError::NotFound(format!("{entity} not found: {id}"))
        }
        _ => ApiError::Internal("Failed to persist model binding".to_string()),
    }
}

async fn load_binding_for_admin(
    pool: &keycompute_db::DbRouter,
    auth: &AuthExtractor,
    id: Uuid,
) -> Result<ModelBinding> {
    let binding = ModelBinding::find_by_id(pool.write_conn(), id)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| ApiError::NotFound(format!("Model binding not found: {id}")))?;
    if !auth.is_admin() && binding.tenant_id != auth.tenant_id {
        return Err(ApiError::Forbidden(
            "Cross-tenant model binding access is not permitted".to_string(),
        ));
    }
    Ok(binding)
}

#[derive(FromQueryResult)]
struct BindingDisplay {
    binding_id: Uuid,
    binding_revision: i64,
    account_name: Option<String>,
    health_status: Option<String>,
    health_reason_code: Option<String>,
}

/// Batch display metadata on the writer; never N+1 reads per listed binding.
/// Preserve explicit failures/unknown status, and never show an expired or
/// configuration-mismatched successful probe as currently healthy.
async fn display_rows(
    db: &impl ConnectionTrait,
    rows: Vec<ModelBinding>,
) -> Result<Vec<ModelBindingInfoResponse>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let metadata = tokio::time::timeout(
        Duration::from_secs(3),
        BindingDisplay::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
        SELECT mb.id AS binding_id,mb.revision AS binding_revision,a.name AS account_name,
               CASE WHEN h.status IS NULL THEN NULL
                    WHEN h.status <> 'healthy' THEN h.status
                    WHEN h.checked_at <= statement_timestamp()
                     AND h.expires_at > statement_timestamp()
                     AND h.account_config_version=a.updated_at THEN 'healthy'
                    ELSE 'stale' END AS health_status,
               h.reason_code AS health_reason_code
        FROM model_bindings mb LEFT JOIN accounts a ON a.id=mb.account_id
        LEFT JOIN account_model_health h ON h.account_id=mb.account_id
          AND h.api_capability=mb.api_capability AND h.model=mb.model
        WHERE mb.id=ANY($1::UUID[])
        "#,
            [ids.into()],
        ))
        .all(db),
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Model binding metadata unavailable".into()))?
    .map_err(|_| ApiError::ServiceUnavailable("Model binding metadata unavailable".into()))?;
    let mut metadata: std::collections::HashMap<Uuid, BindingDisplay> =
        metadata.into_iter().map(|r| (r.binding_id, r)).collect();
    rows.into_iter()
        .map(|binding| {
            let display = metadata.remove(&binding.id).ok_or_else(|| {
                ApiError::Conflict("Model bindings changed; refresh the list".into())
            })?;
            if display.binding_revision != binding.revision {
                return Err(ApiError::Conflict(
                    "Model binding changed; refresh the list".into(),
                ));
            }
            Ok(ModelBindingInfoResponse {
                id: binding.id,
                tenant_id: binding.tenant_id,
                api_capability: binding.api_capability,
                model: binding.model,
                account_id: binding.account_id,
                account_name: display.account_name,
                enabled: binding.enabled,
                revision: binding.revision,
                health_status: display.health_status,
                health_reason_code: display.health_reason_code,
                created_at: binding.created_at.to_rfc3339(),
                updated_at: binding.updated_at.to_rfc3339(),
            })
        })
        .collect()
}
async fn to_response(
    db: &impl ConnectionTrait,
    binding: ModelBinding,
) -> Result<ModelBindingInfoResponse> {
    display_rows(db, vec![binding])
        .await?
        .pop()
        .ok_or_else(|| ApiError::Conflict("Model binding changed; refresh the list".into()))
}

/// GET /api/v1/admin/model-bindings
pub async fn list_model_bindings(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Query(params): Query<ModelBindingListQuery>,
) -> Result<Json<ModelBindingPageResponse>> {
    require_management(&auth)?;
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;
    let tenant_id = requested_tenant(&auth, params.tenant_id)?;
    if let Some(capability) = params.api_capability.as_deref()
        && capability != MODEL_BINDING_CAPABILITY
    {
        return Err(ApiError::BadRequest(
            "model bindings only support chat_completions".to_string(),
        ));
    }
    let (page, page_size, _) = normalize_list_pagination(params.page, params.page_size, None, None);
    let page_size = page_size.clamp(1, MODEL_BINDING_MAX_PAGE_SIZE);
    let offset = (page - 1).saturating_mul(page_size);
    let rows = ModelBinding::find_all_filtered(
        pool.write_conn(),
        tenant_id,
        params.enabled,
        params.model.as_deref(),
        page_size,
        offset,
    )
    .await
    .map_err(map_db_error)?;
    let total = ModelBinding::count_filtered(
        pool.write_conn(),
        tenant_id,
        params.enabled,
        params.model.as_deref(),
    )
    .await
    .map_err(map_db_error)?;
    let bindings = display_rows(pool.write_conn(), rows).await?;
    Ok(Json(ModelBindingPageResponse {
        bindings,
        total,
        page,
        page_size,
        total_pages: total_pages(total, page_size),
    }))
}

/// POST /api/v1/admin/model-bindings
pub async fn create_model_binding(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Json(request): Json<CreateModelBindingRequest>,
) -> Result<Json<ModelBindingInfoResponse>> {
    require_management(&auth)?;
    if !auth.is_admin() && request.tenant_id != auth.tenant_id {
        return Err(ApiError::Forbidden(
            "Cross-tenant model binding access is not permitted".to_string(),
        ));
    }
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;
    let request = DbCreateRequest {
        tenant_id: request.tenant_id,
        api_capability: request.api_capability.unwrap_or_else(default_capability),
        model: request.model,
        account_id: request.account_id,
        enabled: request.enabled,
    };
    let binding = ModelBinding::create(pool.write_conn(), &request)
        .await
        .map_err(map_db_error)?;
    Ok(Json(to_response(pool.write_conn(), binding).await?))
}

/// PUT /api/v1/admin/model-bindings/{id}
pub async fn update_model_binding(
    auth: AuthExtractor,
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Json(request): Json<UpdateModelBindingRequest>,
) -> Result<Json<ModelBindingInfoResponse>> {
    require_management(&auth)?;
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;
    let binding = load_binding_for_admin(pool, &auth, id).await?;
    let request = DbUpdateRequest {
        model: request.model,
        account_id: request.account_id,
        enabled: request.enabled,
        expected_revision: request.expected_revision,
    };
    let updated = binding
        .update(pool.write_conn(), &request)
        .await
        .map_err(map_db_error)?;
    Ok(Json(to_response(pool.write_conn(), updated).await?))
}

/// DELETE /api/v1/admin/model-bindings/{id}?expected_revision=N
pub async fn delete_model_binding(
    auth: AuthExtractor,
    Path(id): Path<Uuid>,
    Query(query): Query<RevisionQuery>,
    State(state): State<AppState>,
) -> Result<Json<MessageResponse>> {
    require_management(&auth)?;
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;
    let binding = load_binding_for_admin(pool, &auth, id).await?;
    if query.expected_revision <= 0 {
        return Err(ApiError::BadRequest(
            "expected_revision must be positive".to_string(),
        ));
    }
    binding
        .delete_if_revision(pool.write_conn(), query.expected_revision)
        .await
        .map_err(map_db_error)?;
    Ok(Json(MessageResponse {
        message: "Model binding deleted".to_string(),
    }))
}

/// POST /api/v1/admin/model-bindings/{id}/probe
pub async fn probe_model_binding(
    auth: AuthExtractor,
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Json(request): Json<ModelBindingProbeRequest>,
) -> Result<Json<ModelBindingProbeResponse>> {
    require_management(&auth)?;
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;
    let binding = load_binding_for_admin(pool, &auth, id).await?;
    if request.api_capability != MODEL_BINDING_CAPABILITY || request.model != binding.model {
        return Err(ApiError::BadRequest(
            "probe model and capability must exactly match the binding".to_string(),
        ));
    }
    let account = Account::find_by_id_for_key_share(pool.write_conn(), binding.account_id)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| ApiError::ServiceUnavailable("model_binding_unavailable".to_string()))?;
    ModelBinding::validate_account_for_tenant(
        pool.write_conn(),
        binding.tenant_id,
        account.id,
        &binding.api_capability,
        &binding.model,
    )
    .await
    .map_err(|error| ApiError::ServiceUnavailable(format!("model_binding_unavailable: {error}")))?;
    if !account.enabled {
        return Err(ApiError::ServiceUnavailable(
            "model_binding_unavailable".to_string(),
        ));
    }
    let api_key = crate::handlers::admin_account::decrypt_account_api_key(
        &account.upstream_api_key_encrypted,
    )?;
    let protocol = ProtocolType::parse(&account.provider).ok_or_else(|| {
        ApiError::ServiceUnavailable("model_binding_protocol_unsupported".to_string())
    })?;
    if protocol != ProtocolType::Openai {
        return Err(ApiError::ServiceUnavailable(
            "model_binding_protocol_unsupported".to_string(),
        ));
    }
    let endpoint = if account.endpoint.is_empty() {
        protocol.default_endpoint().to_string()
    } else {
        normalize_base_url(&account.endpoint)
            .map_err(|_| ApiError::ServiceUnavailable("model_binding_endpoint_invalid".into()))?
    };
    let timeout_ms = request
        .timeout_ms
        .unwrap_or(DEFAULT_PROBE_TIMEOUT_MS)
        .clamp(100, MAX_PROBE_TIMEOUT_MS);
    // Reuse the account-aware production transport so egress/proxy and
    // credential policies cannot be bypassed by an admin probe. The outer
    // timeout still bounds this explicit operation.
    let transport = state
        .http_proxy
        .client_for_provider_and_account(protocol.as_str(), Some(account.id));
    let adapter = crate::providers::get_provider_definition(protocol.as_str())
        .map(|definition| (definition.create_adapter)())
        .ok_or_else(|| ApiError::ServiceUnavailable("model_binding_protocol_unsupported".into()))?;
    let request_to_upstream = UpstreamRequest {
        endpoint,
        upstream_api_key: keycompute_types::SensitiveString::new(api_key),
        model: binding.model.clone(),
        messages: vec![UpstreamMessage {
            role: "user".to_string(),
            content: keycompute_types::MessageContent::text("ping"),
        }],
        stream: false,
        include_stream_usage: true,
        preserve_native_chat_body: false,
        max_tokens: Some(1),
        temperature: None,
        top_p: None,
        native_openai_chat_request: None,
        native_anthropic_request: None,
        native_anthropic_headers: Default::default(),
    };
    let _probe_permit = tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        MODEL_PROBE_SEMAPHORE.clone().acquire_owned(),
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("model_probe_busy".to_string()))?
    .map_err(|_| ApiError::ServiceUnavailable("model_probe_busy".to_string()))?;
    // Capture both fences before any external I/O. A delayed probe must never
    // overwrite a newer observation that won while this request was in flight.
    let previous_generation = AccountModelHealth::find(
        pool.write_conn(),
        binding.account_id,
        &binding.api_capability,
        &binding.model,
    )
    .await
    .map_err(map_db_error)?
    .map(|health| health.generation)
    .unwrap_or(0);
    let checked_at = crate::model_binding::database_now(pool.write_conn()).await?;
    let probe_result = tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        adapter.chat(transport.as_ref(), request_to_upstream),
    )
    .await;
    let (status, reason_code) = match probe_result {
        Ok(Ok(_)) => ("healthy".to_string(), None),
        Ok(Err(error)) => ("unhealthy".to_string(), Some(probe_reason_code(&error))),
        Err(_) => (
            "unhealthy".to_string(),
            Some("upstream_timeout".to_string()),
        ),
    };
    // Both binding and health/configuration revisions are compared in the
    // same SQL statement below, not in a separate post-I/O check.
    let expires_at = checked_at + ChronoDuration::seconds(HEALTH_TTL_SECS);
    let health = AccountModelHealth::upsert_binding_probe_if_current(
        pool.write_conn(),
        &AccountModelHealthProbe {
            account_id: binding.account_id,
            api_capability: binding.api_capability.clone(),
            model: binding.model.clone(),
            status: status.clone(),
            reason_code: reason_code.clone(),
            checked_at,
            expires_at,
            account_config_version: account.updated_at,
            expected_generation: previous_generation,
        },
        &binding,
    )
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| ApiError::Conflict("A newer model health probe won; retry".to_string()))?;
    Ok(Json(ModelBindingProbeResponse {
        binding_id: binding.id,
        account_id: binding.account_id,
        model: binding.model,
        status,
        reason_code,
        checked_at: checked_at.to_rfc3339(),
        expires_at: expires_at.to_rfc3339(),
        generation: health.generation,
    }))
}

fn probe_reason_code(error: &keycompute_types::KeyComputeError) -> String {
    match error {
        keycompute_types::KeyComputeError::UpstreamFailure { stable_code, .. } => {
            stable_code.clone()
        }
        keycompute_types::KeyComputeError::ProviderTimeout(_, _)
        | keycompute_types::KeyComputeError::Timeout(_) => "upstream_timeout".to_string(),
        keycompute_types::KeyComputeError::NetworkError(_) => "upstream_transport".to_string(),
        keycompute_types::KeyComputeError::SerializationError(_) => "upstream_protocol".to_string(),
        _ => "probe_failed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{map_db_error, requested_tenant};
    use crate::extractors::AuthExtractor;
    use keycompute_auth::Permission;
    use keycompute_db::DbError;
    use uuid::Uuid;

    #[test]
    fn tenant_admin_scope_cannot_be_widened_by_query() {
        let tenant = Uuid::new_v4();
        let other = Uuid::new_v4();
        let auth = AuthExtractor::new(Uuid::new_v4(), tenant, Uuid::nil(), "admin")
            .with_permissions(vec![Permission::ManageProviders]);
        assert_eq!(requested_tenant(&auth, None).unwrap(), Some(tenant));
        assert!(requested_tenant(&auth, Some(other)).is_err());
    }

    #[test]
    fn system_admin_can_filter_all_or_one_tenant() {
        let auth = AuthExtractor::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::nil(), "admin")
            .with_permissions(vec![Permission::SystemAdmin, Permission::ManageProviders]);
        assert_eq!(requested_tenant(&auth, None).unwrap(), None);
        let tenant = Uuid::new_v4();
        assert_eq!(requested_tenant(&auth, Some(tenant)).unwrap(), Some(tenant));
    }

    #[test]
    fn model_binding_conflicts_are_client_visible() {
        let error = map_db_error(DbError::OptimisticConflict {
            entity: "model binding".to_string(),
            id: "binding".to_string(),
        });
        assert!(
            matches!(error, crate::error::ApiError::Conflict(message) if message.contains("reload"))
        );
    }
}
