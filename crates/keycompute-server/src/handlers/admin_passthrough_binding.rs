//! System-admin account-to-tenant grants. Writes do not call upstreams.
use crate::{
    error::{ApiError, Result},
    extractors::{GlobalConsoleAuth, RequestId},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::Duration as ChronoDuration;
use keycompute_auth::AuthorizationAction;
use keycompute_db::{
    AuditContext, DbError,
    models::account::{AccountManagementScope, ProviderAuthzSnapshot},
    models::passthrough_binding::{
        AccountModelHealth, AccountModelHealthProbe, CreatePassthroughBindingRequest as DbCreate,
        PassthroughBinding, PassthroughBindingListFilter, PassthroughBindingManagementView,
        UpdatePassthroughBindingRequest as DbUpdate,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(3);
static PROBES: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(8)));
fn management_scope(auth: &GlobalConsoleAuth) -> Result<AccountManagementScope> {
    // These are platform handlers, including when mounted without middleware.
    // Tenant administration has its own path-bound TenantAdmin entry points.
    Ok(AccountManagementScope::Platform(
        auth.require_platform(AuthorizationAction::ManagePlatform)?,
    ))
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
fn map(error: DbError) -> ApiError {
    match error {
        DbError::OptimisticConflict { .. } => {
            ApiError::Conflict("Passthrough binding changed; reload and retry".into())
        }
        DbError::Other(s) if s == "passthrough_binding_ambiguous" => ApiError::Conflict(
            "Another account exposes an overlapping model to these tenants; resolve the conflict first"
                .into(),
        ),
        DbError::Other(s)
            if s.contains("authorization")
                || s.contains("administrator")
                || s.contains("root platform") =>
        {
            ApiError::Forbidden("Passthrough binding management authorization denied".into())
        }
        DbError::Other(s) if s.contains("must") || s.contains("invalid") => {
            ApiError::BadRequest(s)
        }
        DbError::Other(s) if s.contains("active") || s.contains("referenced") => {
            ApiError::Conflict(s)
        }
        DbError::DatabaseError(e) if e.to_string().to_ascii_lowercase().contains("unique") => {
            ApiError::Conflict("This account and tenant already have a passthrough binding".into())
        }
        DbError::NotFound { .. } => ApiError::NotFound("Account, tenant or binding not found".into()),
        _ => ApiError::ServiceUnavailable("Passthrough binding state is unavailable".into()),
    }
}
fn binding_info(mut row: PassthroughBindingManagementView) -> PassthroughBindingInfo {
    row.validate_connection(|endpoint, secret| {
        crate::passthrough_binding::connection_metadata(endpoint, secret).is_ok()
    });
    PassthroughBindingInfo {
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
        health_status: Some(row.health_status),
        health_reason_code: row.health_reason_code,
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
    }
}
fn pool(state: &AppState) -> Result<&keycompute_db::DbRouter> {
    state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Database unavailable".into()))
}
fn pagination(page: Option<i64>, size: Option<i64>) -> (i64, i64, i64) {
    let p = page.unwrap_or(1).clamp(1, 1_000_000);
    let s = size.unwrap_or(20).clamp(1, 100);
    (p, s, (p - 1) * s)
}
#[derive(Debug, Deserialize, Default)]
pub struct PassthroughBindingListQuery {
    pub tenant_id: Option<Uuid>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    #[serde(alias = "q")]
    pub search: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePassthroughBindingRequest {
    pub account_id: Uuid,
    pub tenant_id: Uuid,
    #[serde(default)]
    pub is_global: bool,
    #[serde(default)]
    pub pool_enabled: bool,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePassthroughBindingRequest {
    pub account_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub is_global: Option<bool>,
    pub pool_enabled: Option<bool>,
    pub expected_revision: i64,
}
#[derive(Debug, Deserialize)]
pub struct RevisionQuery {
    pub expected_revision: i64,
}
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PassthroughBindingProbeRequest {
    #[serde(default)]
    pub api_capability: Option<String>,
    pub model: Option<String>,
    pub timeout_ms: Option<u64>,
}
#[derive(Debug, Serialize)]
pub struct PassthroughBindingInfo {
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
    pub health_status: Option<String>,
    pub health_reason_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Serialize)]
pub struct PassthroughBindingPage {
    pub bindings: Vec<PassthroughBindingInfo>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}
#[derive(Debug, Serialize)]
pub struct PassthroughAccountOption {
    pub id: Uuid,
    pub name: String,
    pub provider: String,
    pub pool_enabled: bool,
    pub models: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct PassthroughAccountOptions {
    pub accounts: Vec<PassthroughAccountOption>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}
async fn info(
    state: &AppState,
    scope: AccountManagementScope,
    id: Uuid,
) -> Result<PassthroughBindingInfo> {
    let db = pool(state)?.write_conn();
    let row = tokio::time::timeout(TIMEOUT, async {
        match scope {
            AccountManagementScope::Tenant(scope) => {
                PassthroughBinding::find_in_tenant(db, scope, id).await
            }
            AccountManagementScope::Platform(scope) => {
                PassthroughBinding::find_platform(db, scope, id).await
            }
        }
    })
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Binding lookup timed out".into()))?
    .map_err(map)?
    .ok_or_else(|| ApiError::NotFound("Passthrough binding not found".into()))?;
    Ok(binding_info(row))
}
pub async fn list_passthrough_bindings(
    State(state): State<AppState>,
    auth: GlobalConsoleAuth,
    Query(q): Query<PassthroughBindingListQuery>,
) -> Result<Json<PassthroughBindingPage>> {
    let scope = management_scope(&auth)?;
    let (p, s, offset) = pagination(q.page, q.page_size);
    if let AccountManagementScope::Tenant(scope) = scope
        && q.tenant_id.is_some_and(|id| id != scope.tenant_id())
    {
        return Err(ApiError::Forbidden(
            "Cross-tenant binding access is not permitted".into(),
        ));
    }
    let filter = PassthroughBindingListFilter {
        tenant_id: match scope {
            AccountManagementScope::Tenant(scope) => Some(scope.tenant_id()),
            AccountManagementScope::Platform(_) => q.tenant_id,
        },
        search: q.search.clone(),
    };
    let db = pool(&state)?.write_conn();
    let (rows, total) = match scope {
        AccountManagementScope::Tenant(scope) => (
            PassthroughBinding::list_in_tenant(db, scope, &filter, s, offset)
                .await
                .map_err(map)?,
            PassthroughBinding::count_in_tenant(db, scope, &filter)
                .await
                .map_err(map)?,
        ),
        AccountManagementScope::Platform(scope) => (
            PassthroughBinding::list_platform(db, scope, &filter, s, offset)
                .await
                .map_err(map)?,
            PassthroughBinding::count_platform(db, scope, &filter)
                .await
                .map_err(map)?,
        ),
    };
    Ok(Json(PassthroughBindingPage {
        bindings: rows.into_iter().map(binding_info).collect(),
        total,
        page: p,
        page_size: s,
        total_pages: (total + s - 1) / s,
    }))
}
pub async fn get_passthrough_binding(
    State(state): State<AppState>,
    auth: GlobalConsoleAuth,
    Path(id): Path<Uuid>,
) -> Result<Json<PassthroughBindingInfo>> {
    let scope = management_scope(&auth)?;
    Ok(Json(info(&state, scope, id).await?))
}
pub async fn create_passthrough_binding(
    State(state): State<AppState>,
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Json(req): Json<CreatePassthroughBindingRequest>,
) -> Result<Json<PassthroughBindingInfo>> {
    let scope = management_scope(&auth)?;
    let audit = audit_context(&auth, request_id);
    let db_req = DbCreate {
        account_id: req.account_id,
        tenant_id: req.tenant_id,
        is_global: req.is_global,
        pool_enabled: req.pool_enabled,
    };
    let b = match scope {
        AccountManagementScope::Tenant(scope) => {
            PassthroughBinding::create_in_tenant(
                pool(&state)?,
                scope,
                &db_req,
                &audit,
                ProviderAuthzSnapshot::platform(auth.token_version),
            )
            .await
        }
        AccountManagementScope::Platform(scope) => {
            PassthroughBinding::create_platform(
                pool(&state)?,
                scope,
                &db_req,
                &audit,
                ProviderAuthzSnapshot::platform(auth.token_version),
            )
            .await
        }
    }
    .map_err(map)?;
    Ok(Json(info(&state, scope, b.id).await?))
}
pub async fn update_passthrough_binding(
    State(state): State<AppState>,
    auth: GlobalConsoleAuth,
    Path(id): Path<Uuid>,
    request_id: RequestId,
    Json(req): Json<UpdatePassthroughBindingRequest>,
) -> Result<Json<PassthroughBindingInfo>> {
    let scope = management_scope(&auth)?;
    let audit = audit_context(&auth, request_id);
    let db_req = DbUpdate {
        account_id: req.account_id,
        tenant_id: req.tenant_id,
        is_global: req.is_global,
        pool_enabled: req.pool_enabled,
        expected_revision: req.expected_revision,
    };
    let b = match scope {
        AccountManagementScope::Tenant(scope) => {
            PassthroughBinding::update_in_tenant(
                pool(&state)?,
                scope,
                id,
                &db_req,
                &audit,
                ProviderAuthzSnapshot::platform(auth.token_version),
            )
            .await
        }
        AccountManagementScope::Platform(scope) => {
            PassthroughBinding::update_platform(
                pool(&state)?,
                scope,
                id,
                &db_req,
                &audit,
                ProviderAuthzSnapshot::platform(auth.token_version),
            )
            .await
        }
    }
    .map_err(map)?;
    Ok(Json(info(&state, scope, b.id).await?))
}
pub async fn delete_passthrough_binding(
    State(state): State<AppState>,
    auth: GlobalConsoleAuth,
    Path(id): Path<Uuid>,
    request_id: RequestId,
    Query(q): Query<RevisionQuery>,
) -> Result<Json<serde_json::Value>> {
    let scope = management_scope(&auth)?;
    let audit = audit_context(&auth, request_id);
    match scope {
        AccountManagementScope::Tenant(scope) => {
            PassthroughBinding::delete_in_tenant(
                pool(&state)?,
                scope,
                id,
                q.expected_revision,
                &audit,
                ProviderAuthzSnapshot::platform(auth.token_version),
            )
            .await
        }
        AccountManagementScope::Platform(scope) => {
            PassthroughBinding::delete_platform(
                pool(&state)?,
                scope,
                id,
                q.expected_revision,
                &audit,
                ProviderAuthzSnapshot::platform(auth.token_version),
            )
            .await
        }
    }
    .map_err(map)?;
    Ok(Json(
        serde_json::json!({"deleted":true,"message":"Passthrough grant removed; the account does not automatically rejoin the pool"}),
    ))
}
pub async fn passthrough_binding_options(
    State(state): State<AppState>,
    auth: GlobalConsoleAuth,
    Query(q): Query<PassthroughBindingListQuery>,
) -> Result<Json<PassthroughAccountOptions>> {
    let scope = management_scope(&auth)?;
    let (p, s, offset) = pagination(q.page, q.page_size);
    let (rows, total) = match scope {
        AccountManagementScope::Tenant(scope) => PassthroughBinding::options_in_tenant(
            pool(&state)?.write_conn(),
            scope,
            q.search.as_deref(),
            s,
            offset,
        )
        .await
        .map_err(map)?,
        AccountManagementScope::Platform(scope) => PassthroughBinding::options_platform(
            pool(&state)?.write_conn(),
            scope,
            q.search.as_deref(),
            s,
            offset,
        )
        .await
        .map_err(map)?,
    };
    Ok(Json(PassthroughAccountOptions {
        accounts: rows
            .into_iter()
            .map(|row| PassthroughAccountOption {
                id: row.id,
                name: row.name,
                provider: row.provider,
                pool_enabled: row.pool_enabled,
                models: row.models,
            })
            .collect(),
        total,
        page: p,
        page_size: s,
        total_pages: (total + s - 1) / s,
    }))
}
pub(crate) async fn probe_with_scope(
    state: &AppState,
    scope: AccountManagementScope,
    id: Uuid,
    audit: &AuditContext,
    snapshot: keycompute_db::models::account::ProviderAuthzSnapshot,
    req: PassthroughBindingProbeRequest,
) -> Result<Json<serde_json::Value>> {
    use llm_protocol_provider::{
        ProtocolType, UpstreamMessage, UpstreamRequest, normalize_base_url,
    };
    let timeout = Duration::from_millis(req.timeout_ms.unwrap_or(2000).clamp(100, 10000));
    let db = pool(state)?.write_conn();
    let _ = tokio::time::timeout(
        TIMEOUT,
        PassthroughBinding::prepare_probe(pool(state)?, scope, id, audit, snapshot),
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Probe lookup timed out".into()))?
    .map_err(map)?
    .ok_or_else(|| ApiError::NotFound("Passthrough binding not found".into()))?;
    let _permit = tokio::time::timeout(timeout, Arc::clone(&PROBES).acquire_owned())
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Diagnostic probes are busy".into()))?
        .map_err(|_| ApiError::ServiceUnavailable("Diagnostic probes are unavailable".into()))?;
    let prepared = tokio::time::timeout(
        TIMEOUT,
        PassthroughBinding::prepare_probe(pool(state)?, scope, id, audit, snapshot),
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Probe lookup timed out".into()))?
    .map_err(map)?
    .ok_or_else(|| ApiError::NotFound("Passthrough binding not found".into()))?;
    let binding = prepared.binding;
    let account = prepared.account;
    let capability = req.api_capability.as_deref().unwrap_or_else(|| {
        if account.provider == "anthropic" {
            "messages"
        } else if account
            .api_capabilities
            .iter()
            .any(|value| value == "chat_completions")
        {
            "chat_completions"
        } else {
            "responses"
        }
    });
    let valid = matches!(
        (account.provider.as_str(), capability),
        ("openai", "chat_completions" | "responses") | ("anthropic", "messages")
    );
    if !account.enabled || !valid || !account.api_capabilities.iter().any(|v| v == capability) {
        return Err(ApiError::BadRequest(
            "Account must be enabled and support the selected native API capability".into(),
        ));
    }
    let model = req
        .model
        .or_else(|| account.models_supported.first().cloned())
        .ok_or_else(|| ApiError::BadRequest("Account declares no models".into()))?;
    if !account.models_supported.contains(&model) {
        return Err(ApiError::BadRequest(
            "Diagnostic model must be declared by the selected account".into(),
        ));
    }
    let version = account.upstream_config_version;
    let prior = AccountModelHealth::ensure_snapshot(db, account.id, capability, &model, version)
        .await
        .map_err(map)?
        .ok_or_else(|| ApiError::Conflict("Account changed before diagnostic".into()))?;
    let checked = crate::passthrough_binding::database_now(db).await?;
    let endpoint = if account.endpoint.is_empty() {
        ProtocolType::parse(&account.provider)
            .expect("validated protocol")
            .default_endpoint()
            .to_string()
    } else {
        normalize_base_url(&account.endpoint)
            .map_err(|_| ApiError::BadRequest("Invalid upstream endpoint".into()))?
    };
    let key = crate::handlers::admin_account::decrypt_account_api_key(
        &account.upstream_api_key_encrypted,
    )?;
    let request = UpstreamRequest {
        endpoint,
        upstream_api_key: keycompute_types::SensitiveString::new(key),
        model: model.clone(),
        messages: vec![UpstreamMessage {
            role: "user".into(),
            content: keycompute_types::MessageContent::text("ping"),
        }],
        stream: false,
        include_stream_usage: false,
        preserve_native_chat_body: false,
        max_tokens: Some(1),
        temperature: None,
        top_p: None,
        native_openai_chat_request: None,
        native_anthropic_request: (capability == "messages").then(|| {
            Arc::new(serde_json::json!({
                "model": model,
                "messages": [{"role": "user", "content": "ping"}],
                "max_tokens": 1,
                "stream": false
            }))
        }),
        native_anthropic_headers: if capability == "messages" {
            [("anthropic-version".into(), "2023-06-01".into())].into()
        } else {
            Default::default()
        },
    };
    let adapter = crate::providers::get_provider_definition(&account.provider)
        .map(|provider| (provider.create_adapter)())
        .ok_or_else(|| ApiError::ServiceUnavailable("Protocol adapter unavailable".into()))?;
    let client = state
        .http_proxy
        .client_for_provider_and_account(&account.provider, Some(account.id));
    let operation = async {
        if capability == "responses" {
            use futures::StreamExt;
            let mut stream = adapter
                .stream_responses_with_meta(
                    client.as_ref(),
                    request,
                    llm_protocol_provider::NativeResponsesRequest {
                        body: Arc::new(serde_json::json!({
                            "model": model,
                            "input": "ping",
                            "max_output_tokens": 16,
                            "stream": false,
                            "store": false
                        })),
                        path: "/responses".into(),
                        headers: Default::default(),
                    },
                )
                .await
                .map_err(
                    |failure| keycompute_types::KeyComputeError::UpstreamFailure {
                        status: failure.status,
                        stable_code: failure.stable_error_code,
                        retryable: false,
                        summary: String::new(),
                    },
                )?
                .body;
            while let Some(event) = stream.next().await {
                match event? {
                    llm_protocol_provider::StreamEvent::Done => return Ok(()),
                    llm_protocol_provider::StreamEvent::Error { .. } => {
                        return Err(keycompute_types::KeyComputeError::ProviderError(
                            "Diagnostic failed".into(),
                        ));
                    }
                    _ => {}
                }
            }
            Err(keycompute_types::KeyComputeError::ProviderError(
                "Diagnostic ended without completion".into(),
            ))
        } else {
            adapter.chat(client.as_ref(), request).await.map(|_| ())
        }
    };
    let result = tokio::time::timeout(timeout, operation).await;
    let (status, reason) = match result {
        Ok(Ok(_)) => ("healthy", None),
        Err(_) => ("unhealthy", Some("upstream_timeout".to_string())),
        Ok(Err(error)) => {
            let code = match error {
                keycompute_types::KeyComputeError::UpstreamFailure {
                    status: Some(status),
                    ..
                } => format!("upstream_http_{status}"),
                _ => "upstream_probe_failed".into(),
            };
            ("unhealthy", Some(code))
        }
    };
    let expires = checked + ChronoDuration::seconds(crate::passthrough_binding::HEALTH_TTL_SECS);
    let probe = AccountModelHealthProbe {
        account_id: account.id,
        api_capability: capability.into(),
        model: model.clone(),
        status: status.into(),
        reason_code: reason.clone(),
        checked_at: checked,
        expires_at: expires,
        account_config_version: version,
        expected_generation: prior.generation,
    };
    let saved = tokio::time::timeout(
        TIMEOUT,
        PassthroughBinding::record_probe(pool(state)?, scope, &binding, &probe, audit, snapshot),
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Diagnostic persistence timed out".into()))?
    .map_err(map)?
    .ok_or_else(|| {
        ApiError::Conflict(
            "Binding, account or model health changed while probing; reload and retry".into(),
        )
    })?;
    Ok(Json(serde_json::json!({
        "binding_id": binding.id,
        "account_id": account.id,
        "model": model,
        "status": status,
        "reason_code": reason,
        "checked_at": checked.to_rfc3339(),
        "expires_at": expires.to_rfc3339(),
        "generation": saved.generation,
        "scope": "single_model_diagnostic",
        "api_capability": capability
    })))
}

pub async fn probe_passthrough_binding(
    State(state): State<AppState>,
    auth: GlobalConsoleAuth,
    Path(id): Path<Uuid>,
    request_id: RequestId,
    Json(req): Json<PassthroughBindingProbeRequest>,
) -> Result<Json<serde_json::Value>> {
    let scope = management_scope(&auth)?;
    let snapshot = ProviderAuthzSnapshot::platform(auth.token_version);
    probe_with_scope(
        &state,
        scope,
        id,
        &audit_context(&auth, request_id),
        snapshot,
        req,
    )
    .await
}
