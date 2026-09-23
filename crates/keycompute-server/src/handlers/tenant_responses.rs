//! Tenant and root administration for managed Responses and Conversations.
use crate::{
    error::{ApiError, Result},
    extractors::{GlobalConsoleAuth, RequestId},
    handlers::pagination::total_pages,
    scoped_state::store::{self, ConversationMutation},
    state::AppState,
    tenant_access::TenantAdmin,
};
use axum::{
    Json, Router,
    extract::{Path, Query, RawQuery, State},
    http::header::{CACHE_CONTROL, PRAGMA},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use keycompute_auth::AuthorizationAction;
use keycompute_db::AuditContext;
use keycompute_types::ModelAccessMode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct TenantPath {
    tenant_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct ResourcePath {
    tenant_id: Uuid,
    mode: String,
    owner_user_id: Uuid,
    id: String,
}

#[derive(Debug, Deserialize)]
pub struct ItemPath {
    tenant_id: Uuid,
    mode: String,
    owner_user_id: Uuid,
    id: String,
    item_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    mode: String,
    owner_user_id: Option<Uuid>,
    page: Option<i64>,
    page_size: Option<i64>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootReason {
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionBody {
    expected_revision: i64,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataBody {
    expected_revision: i64,
    metadata: Value,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppendItemsBody {
    expected_revision: i64,
    items: Vec<Value>,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Page<T> {
    items: Vec<T>,
    total: i64,
    page: i64,
    page_size: i64,
    total_pages: i64,
}

#[derive(Debug, Serialize)]
pub struct Count {
    total: i64,
}

#[derive(Debug, Serialize)]
pub struct ResponseSummary {
    id: String,
    tenant_id: Uuid,
    owner_user_id: Uuid,
    mode: String,
    provider: Option<String>,
    account_id: Option<Uuid>,
    model: Option<String>,
    status: String,
    background: bool,
    store_response: bool,
    stream: bool,
    previous_response_id: Option<String>,
    conversation_id: Option<String>,
    revision: Option<i64>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    expires_at: chrono::DateTime<chrono::Utc>,
    deleted: bool,
    local_content_available: bool,
    native_content_available: bool,
}

#[derive(Debug, Serialize)]
pub struct ConversationSummary {
    id: String,
    tenant_id: Uuid,
    owner_user_id: Uuid,
    mode: String,
    account_id: Option<Uuid>,
    model: Option<String>,
    metadata: Value,
    active_response_id: Option<String>,
    revision: Option<i64>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    expires_at: chrono::DateTime<chrono::Utc>,
    deleted: bool,
}

fn pool(state: &AppState) -> Result<&keycompute_db::DbRouter> {
    state.pool.as_deref().ok_or_else(|| {
        ApiError::ServiceUnavailable("Responses administration storage unavailable".into())
    })
}

fn parse_mode(value: &str) -> Result<ModelAccessMode> {
    match value {
        "passthrough" => Ok(ModelAccessMode::Passthrough),
        "node_dispatch" => Ok(ModelAccessMode::NodeDispatch),
        _ => Err(ApiError::BadRequest("unknown Responses mode".into())),
    }
}

fn page(page: Option<i64>, size: Option<i64>) -> Result<(i64, i64, i64)> {
    let page = page.unwrap_or(1);
    let size = size.unwrap_or(20);
    if !(1..=1_000_000).contains(&page) || !(1..=100).contains(&size) {
        return Err(ApiError::BadRequest(
            "page or page_size outside supported range".into(),
        ));
    }
    Ok((page, size, (page - 1) * size))
}

fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        CACHE_CONTROL,
        "private, no-store".parse().expect("static cache header"),
    );
    response
        .headers_mut()
        .insert(PRAGMA, "no-cache".parse().expect("static pragma header"));
    response
}

fn tenant_audit(access: &TenantAdmin, request_id: RequestId) -> AuditContext {
    access.audit(request_id)
}

fn root_audit(auth: &GlobalConsoleAuth, request_id: RequestId) -> AuditContext {
    AuditContext {
        actor_user_id: auth.user_id,
        credential_kind: auth.credential_kind,
        actor_platform_role: auth.platform_role,
        actor_tenant_role: auth.tenant_role,
        request_id: Some(request_id.0),
    }
}

fn tenant_control_scope(
    access: &TenantAdmin,
    request_id: RequestId,
) -> Result<store::ResponseControlScope> {
    let scope = access.require(AuthorizationAction::ManageTenantResource)?;
    store::ResponseControlScope::tenant_admin(
        scope,
        tenant_audit(access, request_id),
        access.auth().token_version,
        access.auth().authz_version,
        access.auth().membership_authz_version,
        access.auth().credential_expires_at.unwrap_or_default(),
    )
}

fn root_control_scope(
    auth: &GlobalConsoleAuth,
    request_id: RequestId,
    tenant_id: Uuid,
    reason: impl Into<String>,
) -> Result<store::ResponseControlScope> {
    let platform = auth.require_platform(AuthorizationAction::ManagePlatform)?;
    store::ResponseControlScope::root(
        platform,
        tenant_id,
        root_audit(auth, request_id),
        store::ResponseControlSession {
            token_version: auth.token_version,
            jwt_expires_at: auth.credential_expires_at.unwrap_or_default(),
            selected: auth
                .selected_tenant_id
                .map(|tenant_id| {
                    Ok::<_, ApiError>(store::ResponseControlMembership {
                        tenant_id,
                        tenant_role: auth
                            .tenant_role
                            .ok_or_else(|| ApiError::Auth("selected membership required".into()))?,
                        tenant_authz_version: auth.authz_version.ok_or_else(|| {
                            ApiError::Auth("selected tenant version required".into())
                        })?,
                        membership_authz_version: auth.membership_authz_version.ok_or_else(
                            || ApiError::Auth("selected membership version required".into()),
                        )?,
                    })
                })
                .transpose()?,
        },
        reason,
    )
}

fn local_response_summary(row: store::ResponseAdminSummary) -> ResponseSummary {
    ResponseSummary {
        id: row.id,
        tenant_id: row.tenant_id,
        owner_user_id: row.user_id,
        mode: row.access_mode,
        provider: None,
        account_id: row.account_id,
        model: Some(row.model),
        status: row.status,
        background: row.background,
        store_response: row.store_response,
        stream: row.stream,
        previous_response_id: row.previous_id,
        conversation_id: row.conversation_id,
        revision: Some(row.revision),
        created_at: row.created_at,
        updated_at: row.updated_at,
        expires_at: row.expires_at,
        deleted: row.deleted_at.is_some(),
        local_content_available: true,
        native_content_available: false,
    }
}

fn local_conversation_summary(row: store::ConversationAdminSummary) -> ConversationSummary {
    ConversationSummary {
        id: row.id,
        tenant_id: row.tenant_id,
        owner_user_id: row.user_id,
        mode: row.access_mode,
        account_id: row.account_id,
        model: row.model,
        metadata: row.metadata_json,
        active_response_id: row.active_response_id,
        revision: Some(row.revision),
        created_at: row.created_at,
        updated_at: row.updated_at,
        expires_at: row.expires_at,
        deleted: row.deleted_at.is_some(),
    }
}

async fn list_response_page(
    state: AppState,
    control: store::ResponseControlScope,
    q: ListQuery,
) -> Result<Json<Page<ResponseSummary>>> {
    let mode = parse_mode(&q.mode)?;
    let (p, size, offset) = page(q.page, q.page_size)?;
    let items =
        store::admin_list_responses(pool(&state)?, &control, q.owner_user_id, mode, size, offset)
            .await?
            .into_iter()
            .map(local_response_summary)
            .collect();
    let total =
        store::admin_count_responses(pool(&state)?, &control, q.owner_user_id, mode).await?;
    Ok(Json(Page {
        items,
        total,
        page: p,
        page_size: size,
        total_pages: total_pages(total, size),
    }))
}

pub async fn tenant_responses(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Page<ResponseSummary>>> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    list_response_page(state, control, q).await
}

pub async fn platform_responses(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Page<ResponseSummary>>> {
    let reason = q.reason.clone().ok_or_else(|| {
        ApiError::BadRequest("root reason is required for platform Responses access".into())
    })?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    list_response_page(state, control, q).await
}

async fn response_count(
    state: AppState,
    control: store::ResponseControlScope,
    q: ListQuery,
) -> Result<Json<Count>> {
    let mode = parse_mode(&q.mode)?;
    let total =
        store::admin_count_responses(pool(&state)?, &control, q.owner_user_id, mode).await?;
    Ok(Json(Count { total }))
}

pub async fn tenant_response_count(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Count>> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    response_count(state, control, q).await
}

pub async fn platform_response_count(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Count>> {
    let reason = q.reason.clone().ok_or_else(|| {
        ApiError::BadRequest("root reason is required for platform Responses access".into())
    })?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    response_count(state, control, q).await
}

pub async fn tenant_response_detail(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    response_detail(state, control, path).await
}

pub async fn platform_response_detail(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    Query(reason): Query<RootReason>,
) -> Result<Response> {
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason.reason)?;
    response_detail(state, control, path).await
}

async fn response_detail(
    state: AppState,
    control: store::ResponseControlScope,
    path: ResourcePath,
) -> Result<Response> {
    let mode = parse_mode(&path.mode)?;

    let row = store::admin_response(
        pool(&state)?,
        &control,
        path.owner_user_id,
        mode,
        &path.id,
        false,
    )
    .await?;
    Ok(no_store(
        Json(json!({
            "summary": local_response_summary(store::ResponseAdminSummary {
                id: row.id.clone(),
                tenant_id: row.tenant_id,
                user_id: row.user_id,
                access_mode: row.access_mode.clone(),
                account_id: row.account_id,
                model: row.model.clone(),
                status: row.status.clone(),
                background: row.background,
                store_response: row.store_response,
                stream: row.stream,
                previous_id: row.previous_id.clone(),
                conversation_id: row.conversation_id.clone(),
                revision: row.revision,
                created_at: row.created_at,
                updated_at: row.updated_at,
                expires_at: row.expires_at,
                deleted_at: row.deleted_at,
            }),
            "response": store::public_response(&row)
        }))
        .into_response(),
    ))
}

async fn mutate_response(
    state: AppState,
    control: store::ResponseControlScope,
    path: ResourcePath,
    body: RevisionBody,
    delete: bool,
) -> Result<Response> {
    let mode = parse_mode(&path.mode)?;

    let (row, active) = store::admin_cancel_or_delete_response(
        pool(&state)?,
        &control,
        path.owner_user_id,
        mode,
        &path.id,
        body.expected_revision,
        delete,
    )
    .await?;
    if active {
        crate::scoped_state::cancel_execution(&state, &row).await?;
    }
    if delete {
        return Ok(Json(json!({"id":path.id,"object":"response","deleted":true})).into_response());
    }
    Ok(no_store(Json(store::public_response(&row)).into_response()))
}

pub async fn tenant_response_cancel(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    mutate_response(state, control, path, body, false).await
}

pub async fn tenant_response_delete(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    mutate_response(state, control, path, body, true).await
}

pub async fn platform_response_cancel(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    let reason = body.reason.clone().ok_or_else(|| {
        ApiError::BadRequest("root reason is required for platform Responses mutation".into())
    })?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    mutate_response(state, control, path, body, false).await
}

pub async fn platform_response_delete(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    let reason = body.reason.clone().ok_or_else(|| {
        ApiError::BadRequest("root reason is required for platform Responses mutation".into())
    })?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    mutate_response(state, control, path, body, true).await
}

pub async fn tenant_response_input_items(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    response_input_items(state, control, path, query).await
}

pub async fn platform_response_input_items(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Response> {
    let reason = reason_from_raw(query.as_deref())?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    response_input_items(state, control, path, query).await
}

async fn response_input_items(
    state: AppState,
    control: store::ResponseControlScope,
    path: ResourcePath,
    query: Option<String>,
) -> Result<Response> {
    let mode = parse_mode(&path.mode)?;
    let q = parse_item_query(query.as_deref())?;
    let row = store::admin_response(
        pool(&state)?,
        &control,
        path.owner_user_id,
        mode,
        &path.id,
        false,
    )
    .await?;
    let items = store::response_input_items(&row);
    Ok(no_store(
        Json(store::list_items(&items, &q)?).into_response(),
    ))
}

fn parse_item_query(raw: Option<&str>) -> Result<store::ListQuery> {
    let mut query = store::ListQuery::default();
    let mut seen = std::collections::HashSet::new();
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        if !seen.insert(key.to_string()) {
            return Err(ApiError::BadRequest(
                "Duplicate resource query parameter".into(),
            ));
        }
        match key.as_ref() {
            "after" => query.after = Some(value.into_owned()),
            "limit" => {
                query.limit = Some(
                    value
                        .parse()
                        .map_err(|_| ApiError::BadRequest("limit must be an integer".into()))?,
                );
            }
            "order" => query.order = Some(value.into_owned()),
            "reason" => {}
            other => {
                return Err(ApiError::BadRequest(format!(
                    "Unknown query parameter: {other}"
                )));
            }
        }
    }
    Ok(query)
}

fn reason_from_raw(raw: Option<&str>) -> Result<String> {
    let mut reason = None;
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        if key == "reason" && reason.replace(value.into_owned()).is_some() {
            return Err(ApiError::BadRequest(
                "reason may be supplied only once".into(),
            ));
        }
    }
    reason.ok_or_else(|| {
        ApiError::BadRequest("root reason is required for platform Responses access".into())
    })
}

async fn list_conversation_page(
    state: AppState,
    control: store::ResponseControlScope,
    q: ListQuery,
) -> Result<Json<Page<ConversationSummary>>> {
    let mode = parse_mode(&q.mode)?;

    let (p, size, offset) = page(q.page, q.page_size)?;
    let items = store::admin_list_conversations(
        pool(&state)?,
        &control,
        q.owner_user_id,
        mode,
        size,
        offset,
    )
    .await?
    .into_iter()
    .map(local_conversation_summary)
    .collect();
    let total =
        store::admin_count_conversations(pool(&state)?, &control, q.owner_user_id, mode).await?;
    Ok(Json(Page {
        items,
        total,
        page: p,
        page_size: size,
        total_pages: total_pages(total, size),
    }))
}

pub async fn tenant_conversations(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Page<ConversationSummary>>> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    list_conversation_page(state, control, q).await
}

pub async fn platform_conversations(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Page<ConversationSummary>>> {
    let reason = q.reason.clone().ok_or_else(|| {
        ApiError::BadRequest("root reason is required for platform Conversations access".into())
    })?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    list_conversation_page(state, control, q).await
}

async fn conversation_count(
    state: AppState,
    control: store::ResponseControlScope,
    q: ListQuery,
) -> Result<Json<Count>> {
    let mode = parse_mode(&q.mode)?;

    Ok(Json(Count {
        total: store::admin_count_conversations(pool(&state)?, &control, q.owner_user_id, mode)
            .await?,
    }))
}

pub async fn tenant_conversation_count(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Count>> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    conversation_count(state, control, q).await
}

pub async fn platform_conversation_count(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Count>> {
    let reason = q.reason.clone().ok_or_else(|| {
        ApiError::BadRequest("root reason is required for platform Conversations access".into())
    })?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    conversation_count(state, control, q).await
}

async fn conversation_detail(
    state: AppState,
    control: store::ResponseControlScope,
    path: ResourcePath,
) -> Result<Response> {
    let mode = parse_mode(&path.mode)?;

    let row = store::admin_conversation(
        pool(&state)?,
        &control,
        path.owner_user_id,
        mode,
        &path.id,
        false,
    )
    .await?;
    Ok(no_store(
        Json(json!({
            "summary": ConversationSummary {
                id: row.id.clone(),
                tenant_id: row.tenant_id,
                owner_user_id: row.user_id,
                mode: row.access_mode.clone(),
                account_id: row.account_id,
                model: row.model.clone(),
                metadata: row.metadata_json.clone(),
                active_response_id: row.active_response_id.clone(),
                revision: Some(row.revision),
                created_at: row.created_at,
                updated_at: row.updated_at,
                expires_at: row.expires_at,
                deleted: row.deleted_at.is_some(),
            },
            "conversation": store::conversation_view(&row)
        }))
        .into_response(),
    ))
}

pub async fn tenant_conversation_detail(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    conversation_detail(state, control, path).await
}

pub async fn platform_conversation_detail(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    Query(reason): Query<RootReason>,
) -> Result<Response> {
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason.reason)?;
    conversation_detail(state, control, path).await
}

async fn mutate_conversation(
    state: AppState,
    control: store::ResponseControlScope,
    path: ResourcePath,
    expected_revision: i64,
    change: ConversationMutation,
) -> Result<Response> {
    let mode = parse_mode(&path.mode)?;

    let (row, active) = store::admin_mutate_conversation(
        pool(&state)?,
        &control,
        path.owner_user_id,
        mode,
        &path.id,
        expected_revision,
        change,
    )
    .await?;
    if let Some(active) = active {
        crate::scoped_state::cancel_execution(&state, &active).await?;
    }
    Ok(no_store(
        Json(store::conversation_view(&row)).into_response(),
    ))
}

pub async fn tenant_conversation_update(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    Json(body): Json<MetadataBody>,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    let metadata = store::metadata(Some(&body.metadata))?;
    mutate_conversation(
        state,
        control,
        path,
        body.expected_revision,
        ConversationMutation::Metadata(metadata),
    )
    .await
}

pub async fn tenant_conversation_delete(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    let id = path.id.clone();
    mutate_conversation(
        state,
        control,
        path,
        body.expected_revision,
        ConversationMutation::Delete,
    )
    .await?;
    Ok(Json(json!({"id":id,"object":"conversation","deleted":true})).into_response())
}

pub async fn platform_conversation_update(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    Json(body): Json<MetadataBody>,
) -> Result<Response> {
    let reason = body.reason.clone().ok_or_else(|| {
        ApiError::BadRequest("root reason is required for platform Conversations mutation".into())
    })?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    let metadata = store::metadata(Some(&body.metadata))?;
    mutate_conversation(
        state,
        control,
        path,
        body.expected_revision,
        ConversationMutation::Metadata(metadata),
    )
    .await
}

pub async fn platform_conversation_delete(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    let reason = body.reason.clone().ok_or_else(|| {
        ApiError::BadRequest("root reason is required for platform Conversations mutation".into())
    })?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    let id = path.id.clone();
    mutate_conversation(
        state,
        control,
        path,
        body.expected_revision,
        ConversationMutation::Delete,
    )
    .await?;
    Ok(Json(json!({"id":id,"object":"conversation","deleted":true})).into_response())
}

pub async fn tenant_conversation_items(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    conversation_items(state, control, path, query).await
}

pub async fn platform_conversation_items(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Response> {
    let reason = reason_from_raw(query.as_deref())?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    conversation_items(state, control, path, query).await
}

async fn conversation_items(
    state: AppState,
    control: store::ResponseControlScope,
    path: ResourcePath,
    query: Option<String>,
) -> Result<Response> {
    let mode = parse_mode(&path.mode)?;
    if mode == ModelAccessMode::AccountPool {
        return Err(ApiError::Conflict(
            "account_pool Conversations administration is unsupported until native conversation affinity is persisted".into(),
        ));
    }
    let row = store::admin_conversation(
        pool(&state)?,
        &control,
        path.owner_user_id,
        mode,
        &path.id,
        false,
    )
    .await?;
    Ok(no_store(
        Json(store::list_items(
            row.items_json.as_array().ok_or_else(store::missing)?,
            &parse_item_query(query.as_deref())?,
        )?)
        .into_response(),
    ))
}

pub async fn tenant_conversation_append(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    Json(body): Json<AppendItemsBody>,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    mutate_conversation(
        state,
        control,
        path,
        body.expected_revision,
        ConversationMutation::Append(body.items),
    )
    .await
}

pub async fn platform_conversation_append(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    Json(body): Json<AppendItemsBody>,
) -> Result<Response> {
    let reason = body.reason.clone().ok_or_else(|| {
        ApiError::BadRequest("root reason is required for platform Conversations mutation".into())
    })?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    mutate_conversation(
        state,
        control,
        path,
        body.expected_revision,
        ConversationMutation::Append(body.items),
    )
    .await
}

pub async fn tenant_conversation_remove_item(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ItemPath>,
    State(state): State<AppState>,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    remove_item(state, control, path, body).await
}

pub async fn platform_conversation_remove_item(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ItemPath>,
    State(state): State<AppState>,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    let reason = body.reason.clone().ok_or_else(|| {
        ApiError::BadRequest("root reason is required for platform Conversations mutation".into())
    })?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    remove_item(state, control, path, body).await
}

async fn remove_item(
    state: AppState,
    control: store::ResponseControlScope,
    path: ItemPath,
    body: RevisionBody,
) -> Result<Response> {
    let item_id = path.item_id.clone();
    mutate_conversation(
        state,
        control,
        ResourcePath {
            tenant_id: path.tenant_id,
            mode: path.mode,
            owner_user_id: path.owner_user_id,
            id: path.id,
        },
        body.expected_revision,
        ConversationMutation::RemoveItem(item_id.clone()),
    )
    .await?;
    Ok(Json(json!({"id":item_id,"object":"conversation.item","deleted":true})).into_response())
}

async fn private_control_response(response: Response) -> Response {
    no_store(response)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/tenants/{tenant_id}/responses", get(tenant_responses))
        .route(
            "/api/v1/platform/tenants/{tenant_id}/responses",
            get(platform_responses),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/responses/count",
            get(tenant_response_count),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/responses/count",
            get(platform_response_count),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/responses/{mode}/{owner_user_id}/{id}",
            get(tenant_response_detail).delete(tenant_response_delete),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/responses/{mode}/{owner_user_id}/{id}",
            get(platform_response_detail).delete(platform_response_delete),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/responses/{mode}/{owner_user_id}/{id}/cancel",
            post(tenant_response_cancel),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/responses/{mode}/{owner_user_id}/{id}/cancel",
            post(platform_response_cancel),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/responses/{mode}/{owner_user_id}/{id}/input_items",
            get(tenant_response_input_items),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/responses/{mode}/{owner_user_id}/{id}/input_items",
            get(platform_response_input_items),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/conversations",
            get(tenant_conversations),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/conversations",
            get(platform_conversations),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/conversations/count",
            get(tenant_conversation_count),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/conversations/count",
            get(platform_conversation_count),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/conversations/{mode}/{owner_user_id}/{id}",
            get(tenant_conversation_detail)
                .patch(tenant_conversation_update)
                .delete(tenant_conversation_delete),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/conversations/{mode}/{owner_user_id}/{id}",
            get(platform_conversation_detail)
                .patch(platform_conversation_update)
                .delete(platform_conversation_delete),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/conversations/{mode}/{owner_user_id}/{id}/items",
            get(tenant_conversation_items).post(tenant_conversation_append),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/conversations/{mode}/{owner_user_id}/{id}/items",
            get(platform_conversation_items).post(platform_conversation_append),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/conversations/{mode}/{owner_user_id}/{id}/items/{item_id}",
            delete(tenant_conversation_remove_item),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/conversations/{mode}/{owner_user_id}/{id}/items/{item_id}",
            delete(platform_conversation_remove_item),
        )
        .layer(axum::middleware::map_response(private_control_response))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parser_is_fixed() {
        assert_eq!(
            parse_mode("passthrough").unwrap(),
            ModelAccessMode::Passthrough
        );
        assert_eq!(
            parse_mode("node_dispatch").unwrap(),
            ModelAccessMode::NodeDispatch
        );
        // Native account-pool management is a separate adapter, not local
        // storage. Do not silently route it through node_dispatch.
        assert!(parse_mode("account_pool").is_err());
        assert!(parse_mode("pt").is_err());
    }

    #[test]
    fn query_contract_rejects_scope_spoofing() {
        assert!(
            serde_json::from_value::<ListQuery>(json!({
                "mode":"passthrough",
                "tenant_id":Uuid::new_v4()
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<RevisionBody>(json!({
                "expected_revision":1,
                "owner_user_id":Uuid::new_v4()
            }))
            .is_err()
        );
        assert!(page(Some(0), Some(20)).is_err());
        assert!(page(Some(1), Some(101)).is_err());
    }
}
