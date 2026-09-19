//! System-admin account-to-tenant grants. Writes do not call upstreams.
use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use keycompute_auth::Permission;
use keycompute_db::{
    DbError,
    models::passthrough_binding::{
        AccountModelHealth, AccountModelHealthProbe, CreatePassthroughBindingRequest as DbCreate,
        PassthroughBinding, UpdatePassthroughBindingRequest as DbUpdate,
    },
};
use sea_orm::{DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(3);
static PROBES: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(8)));
fn admin(a: &AuthExtractor) -> Result<()> {
    if !a.is_admin() || !a.has_permission(&Permission::ManageProviders) {
        return Err(ApiError::Forbidden(
            "System administrator permission required".into(),
        ));
    }
    Ok(())
}
fn map(error: DbError) -> ApiError {
    match error{
    DbError::OptimisticConflict{..}=>ApiError::Conflict("Passthrough binding changed; reload and retry".into()),
    DbError::Other(s) if s=="passthrough_binding_ambiguous"=>ApiError::Conflict("Another account exposes an overlapping model to these tenants; resolve the conflict first".into()),
    DbError::Other(s) if s.contains("must")||s.contains("invalid")=>ApiError::BadRequest(s),
    DbError::DatabaseError(e) if e.to_string().to_ascii_lowercase().contains("unique")=>ApiError::Conflict("This account and tenant already have a passthrough binding".into()),
    DbError::NotFound{..}=>ApiError::NotFound("Account, tenant or binding not found".into()),
    _=>ApiError::ServiceUnavailable("Passthrough binding state is unavailable".into()),
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
    let s = size.unwrap_or(20).clamp(1, 200);
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
#[derive(FromQueryResult)]
struct InfoRow {
    // Internal metadata is never copied into the serialized public DTO.
    endpoint: String,
    upstream_api_key_encrypted: String,
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
impl From<InfoRow> for PassthroughBindingInfo {
    fn from(r: InfoRow) -> Self {
        let usable = crate::passthrough_binding::connection_metadata(
            &r.endpoint,
            &r.upstream_api_key_encrypted,
        )
        .is_ok();
        Self {
            id: r.id,
            account_id: r.account_id,
            account_name: r.account_name,
            tenant_id: r.tenant_id,
            tenant_name: r.tenant_name,
            provider: r.provider,
            is_global: r.is_global,
            pool_enabled: r.pool_enabled,
            revision: r.revision,
            models_supported: r.models_supported,
            health_status: if usable {
                r.health_status
            } else {
                Some("unavailable".into())
            },
            health_reason_code: if usable {
                r.health_reason_code
            } else {
                Some("invalid_connection_metadata".into())
            },
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }
    }
}
const INFO_SELECT: &str = "SELECT pb.*,a.endpoint,a.upstream_api_key_encrypted,a.name AS account_name,t.name AS tenant_name,a.provider,a.models_supported,CASE WHEN a.provider<>'openai' OR NOT ('chat_completions'=ANY(a.api_capabilities)) OR NOT a.enabled OR t.status<>'active' OR owner.status<>'active' THEN 'unavailable' ELSE a.health_status END AS health_status,a.health_reason AS health_reason_code FROM passthrough_bindings pb JOIN accounts a ON a.id=pb.account_id JOIN tenants t ON t.id=pb.tenant_id JOIN tenants owner ON owner.id=a.tenant_id";
async fn info(state: &AppState, id: Uuid) -> Result<PassthroughBindingInfo> {
    let row = tokio::time::timeout(
        TIMEOUT,
        InfoRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("{INFO_SELECT} WHERE pb.id=$1"),
            [id.into()],
        ))
        .one(pool(state)?.write_conn()),
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Binding lookup timed out".into()))?
    .map_err(|_| ApiError::ServiceUnavailable("Binding lookup failed".into()))?
    .ok_or_else(|| ApiError::NotFound("Passthrough binding not found".into()))?;
    Ok(row.into())
}
#[derive(FromQueryResult)]
struct Total {
    total: i64,
}
pub async fn list_passthrough_bindings(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Query(q): Query<PassthroughBindingListQuery>,
) -> Result<Json<PassthroughBindingPage>> {
    admin(&auth)?;
    let (p, s, offset) = pagination(q.page, q.page_size);
    let search = PassthroughBinding::escaped_search(q.search.as_deref());
    let filter = "WHERE ($1::UUID IS NULL OR pb.tenant_id=$1) AND ($2::TEXT IS NULL OR a.name ILIKE '%'||$2||'%' ESCAPE '\\' OR t.name ILIKE '%'||$2||'%' ESCAPE '\\')";
    let read = async {
        let total=Total::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,format!("SELECT count(*)::BIGINT AS total FROM passthrough_bindings pb JOIN accounts a ON a.id=pb.account_id JOIN tenants t ON t.id=pb.tenant_id {filter}"),[q.tenant_id.into(),search.clone().into()])).one(pool(&state)?.write_conn()).await.map_err(|_|ApiError::ServiceUnavailable("Binding listing failed".into()))?.map_or(0,|r|r.total);
        let rows = InfoRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("{INFO_SELECT} {filter} ORDER BY a.name,t.name,pb.id LIMIT $3 OFFSET $4"),
            [q.tenant_id.into(), search.into(), s.into(), offset.into()],
        ))
        .all(pool(&state)?.write_conn())
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Binding listing failed".into()))?;
        Ok::<_, ApiError>(PassthroughBindingPage {
            bindings: rows.into_iter().map(Into::into).collect(),
            total,
            page: p,
            page_size: s,
            total_pages: (total + s - 1) / s,
        })
    };
    Ok(Json(tokio::time::timeout(TIMEOUT, read).await.map_err(
        |_| ApiError::ServiceUnavailable("Binding listing timed out".into()),
    )??))
}
pub async fn get_passthrough_binding(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Path(id): Path<Uuid>,
) -> Result<Json<PassthroughBindingInfo>> {
    admin(&auth)?;
    Ok(Json(info(&state, id).await?))
}
pub async fn create_passthrough_binding(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Json(req): Json<CreatePassthroughBindingRequest>,
) -> Result<Json<PassthroughBindingInfo>> {
    admin(&auth)?;
    let b = tokio::time::timeout(
        TIMEOUT,
        PassthroughBinding::create(
            pool(&state)?.write_conn(),
            &DbCreate {
                account_id: req.account_id,
                tenant_id: req.tenant_id,
                is_global: req.is_global,
                pool_enabled: req.pool_enabled,
            },
        ),
    )
    .await
    .map_err(|_| {
        ApiError::ServiceUnavailable("Binding creation timed out; reload before retrying".into())
    })?
    .map_err(map)?;
    Ok(Json(info(&state, b.id).await?))
}
pub async fn update_passthrough_binding(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdatePassthroughBindingRequest>,
) -> Result<Json<PassthroughBindingInfo>> {
    admin(&auth)?;
    let write = async {
        let b = PassthroughBinding::find_by_id(pool(&state)?.write_conn(), id)
            .await
            .map_err(map)?
            .ok_or_else(|| ApiError::NotFound("Passthrough binding not found".into()))?;
        b.update(
            pool(&state)?.write_conn(),
            &DbUpdate {
                account_id: req.account_id,
                tenant_id: req.tenant_id,
                is_global: req.is_global,
                pool_enabled: req.pool_enabled,
                expected_revision: req.expected_revision,
            },
        )
        .await
        .map_err(map)
    };
    let b = tokio::time::timeout(TIMEOUT, write).await.map_err(|_| {
        ApiError::ServiceUnavailable("Binding update timed out; reload before retrying".into())
    })??;
    Ok(Json(info(&state, b.id).await?))
}
pub async fn delete_passthrough_binding(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Path(id): Path<Uuid>,
    Query(q): Query<RevisionQuery>,
) -> Result<Json<serde_json::Value>> {
    admin(&auth)?;
    let write = async {
        let b = PassthroughBinding::find_by_id(pool(&state)?.write_conn(), id)
            .await
            .map_err(map)?
            .ok_or_else(|| ApiError::NotFound("Passthrough binding not found".into()))?;
        b.delete_if_revision(pool(&state)?.write_conn(), q.expected_revision)
            .await
            .map_err(map)
    };
    tokio::time::timeout(TIMEOUT, write).await.map_err(|_| {
        ApiError::ServiceUnavailable("Binding deletion timed out; reload before retrying".into())
    })??;
    Ok(Json(
        serde_json::json!({"deleted":true,"message":"Passthrough grant removed; the account does not automatically rejoin the pool"}),
    ))
}
pub async fn passthrough_binding_options(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Query(q): Query<PassthroughBindingListQuery>,
) -> Result<Json<PassthroughAccountOptions>> {
    admin(&auth)?;
    let (p, s, offset) = pagination(q.page, q.page_size);
    let search = PassthroughBinding::escaped_search(q.search.as_deref());
    #[derive(FromQueryResult)]
    struct Row {
        id: Uuid,
        name: String,
        provider: String,
        pool_enabled: bool,
        models: Vec<String>,
    }
    let filter = "FROM accounts a JOIN tenants t ON t.id=a.tenant_id WHERE a.enabled AND t.status='active' AND a.provider='openai' AND a.api_capabilities @> ARRAY['chat_completions']::TEXT[] AND ($1::TEXT IS NULL OR a.name ILIKE '%'||$1||'%' ESCAPE '\\')";
    let read = async {
        let total = Total::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT count(*)::BIGINT AS total {filter}"),
            [search.clone().into()],
        ))
        .one(pool(&state)?.write_conn())
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Account options unavailable".into()))?
        .map_or(0, |r| r.total);
        let rows=Row::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,format!("SELECT a.id,a.name,a.provider,a.pool_enabled,a.models_supported AS models {filter} ORDER BY a.name,a.id LIMIT $2 OFFSET $3"),[search.into(),s.into(),offset.into()])).all(pool(&state)?.write_conn()).await.map_err(|_|ApiError::ServiceUnavailable("Account options unavailable".into()))?;
        Ok::<_, ApiError>(PassthroughAccountOptions {
            accounts: rows
                .into_iter()
                .map(|r| PassthroughAccountOption {
                    id: r.id,
                    name: r.name,
                    provider: r.provider,
                    pool_enabled: r.pool_enabled,
                    models: r.models,
                })
                .collect(),
            total,
            page: p,
            page_size: s,
            total_pages: (total + s - 1) / s,
        })
    };
    Ok(Json(tokio::time::timeout(TIMEOUT, read).await.map_err(
        |_| ApiError::ServiceUnavailable("Account options timed out".into()),
    )??))
}
pub async fn probe_passthrough_binding(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Path(id): Path<Uuid>,
    Json(req): Json<PassthroughBindingProbeRequest>,
) -> Result<Json<serde_json::Value>> {
    use llm_protocol_provider::{
        ProtocolType, UpstreamMessage, UpstreamRequest, normalize_base_url,
    };
    admin(&auth)?;
    let timeout = Duration::from_millis(req.timeout_ms.unwrap_or(2000).clamp(100, 10000));
    let _permit = tokio::time::timeout(timeout, Arc::clone(&PROBES).acquire_owned())
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Diagnostic probes are busy".into()))?
        .map_err(|_| ApiError::ServiceUnavailable("Diagnostic probes are unavailable".into()))?;
    let db = pool(&state)?.write_conn();
    let load = async {
        let binding = PassthroughBinding::find_by_id(db, id)
            .await
            .map_err(map)?
            .ok_or_else(|| ApiError::NotFound("Passthrough binding not found".into()))?;
        let account = keycompute_db::Account::find_by_id(db, binding.account_id)
            .await
            .map_err(map)?
            .ok_or_else(|| ApiError::NotFound("Account not found".into()))?;
        Ok::<_, ApiError>((binding, account))
    };
    let (binding, account) = tokio::time::timeout(TIMEOUT, load)
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Probe lookup timed out".into()))??;
    if !account.enabled
        || account.provider != "openai"
        || !account
            .api_capabilities
            .iter()
            .any(|v| v == "chat_completions")
    {
        return Err(ApiError::BadRequest(
            "Account must be enabled and OpenAI Chat capable".into(),
        ));
    }
    let model = req
        .model
        .or_else(|| account.models_supported.first().cloned())
        .ok_or_else(|| ApiError::BadRequest("Account declares no models".into()))?;
    if !account.models_supported.contains(&model) || model.to_lowercase().starts_with("node:") {
        return Err(ApiError::BadRequest(
            "Diagnostic model must be declared by the selected account".into(),
        ));
    }
    let version = account.upstream_config_version;
    let prior =
        AccountModelHealth::ensure_snapshot(db, account.id, "chat_completions", &model, version)
            .await
            .map_err(map)?
            .ok_or_else(|| ApiError::Conflict("Account changed before diagnostic".into()))?;
    let checked = crate::passthrough_binding::database_now(db).await?;
    let endpoint = if account.endpoint.is_empty() {
        ProtocolType::Openai.default_endpoint().to_string()
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
        native_anthropic_request: None,
        native_anthropic_headers: Default::default(),
    };
    let adapter = crate::providers::get_provider_definition("openai")
        .map(|p| (p.create_adapter)())
        .ok_or_else(|| ApiError::ServiceUnavailable("OpenAI adapter unavailable".into()))?;
    let client = state
        .http_proxy
        .client_for_provider_and_account("openai", Some(account.id));
    let result = tokio::time::timeout(timeout, adapter.chat(client.as_ref(), request)).await;
    let (status, reason) = match result {
        Ok(Ok(_)) => ("healthy", None),
        Err(_) => ("unhealthy", Some("upstream_timeout".to_string())),
        Ok(Err(error)) => {
            let code = match error {
                keycompute_types::KeyComputeError::UpstreamFailure {
                    status: Some(s), ..
                } => format!("upstream_http_{s}"),
                _ => "upstream_probe_failed".into(),
            };
            ("unhealthy", Some(code))
        }
    };
    let expires = checked + ChronoDuration::seconds(crate::passthrough_binding::HEALTH_TTL_SECS);
    let saved = tokio::time::timeout(
        TIMEOUT,
        AccountModelHealth::upsert_binding_probe_if_current(
            db,
            &AccountModelHealthProbe {
                account_id: account.id,
                api_capability: "chat_completions".into(),
                model: model.clone(),
                status: status.into(),
                reason_code: reason.clone(),
                checked_at: checked,
                expires_at: expires,
                account_config_version: version,
                expected_generation: prior.generation,
            },
            &binding,
        ),
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Diagnostic persistence timed out".into()))?
    .map_err(map)?
    .ok_or_else(|| {
        ApiError::Conflict(
            "Binding, account or model health changed while probing; reload and retry".into(),
        )
    })?;
    Ok(Json(
        serde_json::json!({"binding_id":binding.id,"account_id":account.id,"model":model,"status":status,"reason_code":reason,"checked_at":checked.to_rfc3339(),"expires_at":expires.to_rfc3339(),"generation":saved.generation,"scope":"single_model_diagnostic"}),
    ))
}
