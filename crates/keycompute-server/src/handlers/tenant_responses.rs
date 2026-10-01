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
    http::{
        HeaderMap,
        header::{CACHE_CONTROL, PRAGMA},
    },
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use keycompute_auth::AuthorizationAction;
use keycompute_db::{
    AuditContext,
    models::{account::Account, response_affinity::ResponseAffinity},
};
use keycompute_types::ModelAccessMode;
use llm_gateway::{JsonRequestMethod, PassthroughBody};
use llm_protocol_provider::ProtocolType;
use sea_orm::TransactionTrait;
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
    expected_revision: Option<i64>,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataBody {
    expected_revision: Option<i64>,
    metadata: Value,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppendItemsBody {
    expected_revision: Option<i64>,
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
        "account_pool" => Ok(ModelAccessMode::AccountPool),
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

fn native_response_index(row: store::NativeAdminSummary) -> ResponseSummary {
    ResponseSummary {
        id: row.id,
        tenant_id: row.tenant_id,
        owner_user_id: row.user_id,
        mode: ModelAccessMode::AccountPool.as_str().into(),
        provider: Some(row.provider),
        account_id: Some(row.account_id),
        model: row.model,
        status: "indexed".into(),
        background: false,
        store_response: true,
        stream: false,
        previous_response_id: None,
        conversation_id: None,
        revision: None,
        created_at: row.created_at,
        updated_at: row.updated_at,
        expires_at: row.expires_at,
        deleted: false,
        local_content_available: false,
        native_content_available: true,
    }
}

fn native_conversation_index(row: store::NativeAdminSummary) -> ConversationSummary {
    ConversationSummary {
        id: row.id,
        tenant_id: row.tenant_id,
        owner_user_id: row.user_id,
        mode: ModelAccessMode::AccountPool.as_str().into(),
        account_id: Some(row.account_id),
        model: row.model,
        metadata: Value::Null,
        active_response_id: None,
        revision: None,
        created_at: row.created_at,
        updated_at: row.updated_at,
        expires_at: row.expires_at,
        deleted: false,
    }
}

fn native_conversation_summary(
    affinity: &ResponseAffinity,
    body: &Value,
) -> Result<ConversationSummary> {
    if affinity.resource_kind.as_deref() != Some("conversation") {
        return Err(ApiError::NotFound("Native Conversation not found".into()));
    }
    Ok(ConversationSummary {
        id: affinity.response_id.clone(),
        tenant_id: affinity.tenant_id,
        owner_user_id: affinity.user_id.ok_or_else(store::missing)?,
        mode: ModelAccessMode::AccountPool.as_str().into(),
        account_id: Some(affinity.account_id.ok_or_else(store::missing)?),
        model: affinity.model.clone(),
        metadata: body.get("metadata").cloned().unwrap_or(Value::Null),
        active_response_id: None,
        revision: None,
        created_at: affinity.created_at,
        updated_at: affinity.updated_at,
        expires_at: affinity.expires_at,
        deleted: false,
    })
}

#[derive(Clone, Copy, Debug)]
enum NativeResponseOperation {
    Detail,
    Cancel,
    Delete,
    InputItems,
}
impl NativeResponseOperation {
    const fn method(self) -> JsonRequestMethod {
        match self {
            Self::Detail | Self::InputItems => JsonRequestMethod::Get,
            Self::Cancel => JsonRequestMethod::Post,
            Self::Delete => JsonRequestMethod::Delete,
        }
    }
    const fn suffix(self) -> &'static str {
        match self {
            Self::Detail | Self::Delete => "",
            Self::Cancel => "/cancel",
            Self::InputItems => "/input_items",
        }
    }
    const fn name(self) -> &'static str {
        match self {
            Self::Detail => "detail",
            Self::Cancel => "cancel",
            Self::Delete => "delete",
            Self::InputItems => "input_items",
        }
    }
}

#[derive(Debug)]
enum NativeOperation {
    Response(NativeResponseOperation),
    ConversationDetail,
    ConversationDelete,
    ConversationItems,
    ConversationMetadata,
    ConversationAppend,
    ConversationRemoveItem(String),
}
impl NativeOperation {
    fn kind(&self) -> &'static str {
        if matches!(self, Self::Response(_)) {
            "response"
        } else {
            "conversation"
        }
    }
    fn method(&self) -> JsonRequestMethod {
        match self {
            Self::Response(op) => op.method(),
            Self::ConversationDetail | Self::ConversationItems => JsonRequestMethod::Get,
            Self::ConversationDelete | Self::ConversationRemoveItem(_) => JsonRequestMethod::Delete,
            Self::ConversationMetadata | Self::ConversationAppend => JsonRequestMethod::Post,
        }
    }
    fn suffix(&self) -> String {
        match self {
            Self::Response(op) => op.suffix().into(),
            Self::ConversationItems | Self::ConversationAppend => "/items".into(),
            Self::ConversationRemoveItem(id) => format!(
                "/items/{}",
                url::form_urlencoded::byte_serialize(id.as_bytes()).collect::<String>()
            ),
            _ => String::new(),
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::Response(op) => op.name(),
            Self::ConversationDetail => "detail",
            Self::ConversationDelete => "delete",
            Self::ConversationItems => "items",
            Self::ConversationMetadata => "metadata",
            Self::ConversationAppend => "append_items",
            Self::ConversationRemoveItem(_) => "remove_item",
        }
    }
    fn deletes_resource(&self) -> bool {
        matches!(
            self,
            Self::Response(NativeResponseOperation::Delete) | Self::ConversationDelete
        )
    }
    fn detail(&self) -> bool {
        matches!(
            self,
            Self::Response(NativeResponseOperation::Detail) | Self::ConversationDetail
        )
    }
}
struct NativeRequest {
    operation: NativeOperation,
    query: Option<String>,
    body: Option<Value>,
    headers: HeaderMap,
}

fn validate_native_selector(owner: Uuid, id: &str) -> Result<()> {
    if owner.is_nil() || !llm_protocol_openai::responses_stream::valid_openai_resource_id(id) {
        Err(ApiError::BadRequest(
            "A real owner and bounded opaque Responses resource ID are required".into(),
        ))
    } else {
        Ok(())
    }
}

async fn native_audit_event(
    tx: &sea_orm::DatabaseTransaction,
    control: &store::ResponseControlScope,
    audit: &AuditContext,
    owner: Uuid,
    id: &str,
    action: &str,
    metadata: Value,
) -> Result<()> {
    store::append_control_audit(
        tx,
        control,
        audit,
        action,
        if action.starts_with("conversation.") {
            "conversation"
        } else {
            "response"
        },
        Some(id),
        json!({
            "owner_user_id": owner,
            "access_mode": "account_pool",
            "native": metadata,
        }),
    )
    .await
}

async fn native_control_event(
    state: &AppState,
    control: &store::ResponseControlScope,
    owner: Uuid,
    id: &str,
    account_id: Option<Uuid>,
    action: &str,
    metadata: Value,
) -> Result<()> {
    let tx = pool(state)?.begin().await.map_err(|error| {
        ApiError::Internal(format!(
            "Failed to begin native Responses control validation: {error}"
        ))
    })?;
    let audit = control.revalidate_for_admin(&tx).await?;
    if let Some(account_id) = account_id {
        crate::handlers::responses::authorize_non_pt_account(&tx, control.tenant_id(), account_id)
            .await?;
    }
    native_audit_event(&tx, control, &audit, owner, id, action, metadata).await?;
    tx.commit().await.map_err(|error| {
        ApiError::Internal(format!(
            "Failed to commit native Responses control audit: {error}"
        ))
    })?;
    Ok(())
}

fn native_item_query(raw: Option<&str>) -> Result<Option<String>> {
    parse_item_query(raw)?;
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        if k != "reason" {
            ser.append_pair(&k, &v);
        }
    }
    let q = ser.finish();
    Ok((!q.is_empty()).then_some(q))
}

fn native_conversation_id(body: &Value) -> Option<String> {
    body.get("conversation").and_then(|v| {
        v.as_str()
            .map(str::to_owned)
            .or_else(|| v.get("id").and_then(Value::as_str).map(str::to_owned))
    })
}

fn native_summary(affinity: &ResponseAffinity, body: &Value) -> Result<ResponseSummary> {
    let owner = affinity.user_id.ok_or_else(|| {
        ApiError::NotFound("Native Responses resource has no caller owner".into())
    })?;
    let account_id = affinity.account_id.ok_or_else(|| {
        ApiError::NotFound("Native Responses resource has no upstream account".into())
    })?;
    if affinity.resource_kind.as_deref() != Some("response") {
        return Err(ApiError::NotFound(
            "Native Responses resource not found".into(),
        ));
    }
    Ok(ResponseSummary {
        id: affinity.response_id.clone(),
        tenant_id: affinity.tenant_id,
        owner_user_id: owner,
        mode: ModelAccessMode::AccountPool.as_str().to_string(),
        provider: Some(affinity.provider.clone()),
        account_id: Some(account_id),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| affinity.model.clone()),
        status: body
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        background: body
            .get("background")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        store_response: body.get("store").and_then(Value::as_bool).unwrap_or(false),
        stream: false,
        previous_response_id: body
            .get("previous_response_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        conversation_id: native_conversation_id(body),
        revision: None,
        created_at: affinity.created_at,
        updated_at: affinity.updated_at,
        expires_at: affinity.expires_at,
        deleted: false,
        local_content_available: false,
        native_content_available: true,
    })
}

async fn native_response_operation(
    state: AppState,
    control: store::ResponseControlScope,
    path: ResourcePath,
    operation: NativeResponseOperation,
    query: Option<String>,
    client_headers: HeaderMap,
) -> Result<Response> {
    native_resource_operation(
        state,
        control,
        path,
        NativeRequest {
            operation: NativeOperation::Response(operation),
            query,
            body: None,
            headers: client_headers,
        },
    )
    .await
}

async fn native_resource_operation(
    state: AppState,
    control: store::ResponseControlScope,
    path: ResourcePath,
    request: NativeRequest,
) -> Result<Response> {
    validate_native_selector(path.owner_user_id, &path.id)?;
    let NativeRequest {
        operation,
        query,
        body,
        headers: client_headers,
    } = request;
    let kind = operation.kind();
    let request_action = format!("{kind}.native.request");
    let result_action = format!("{kind}.native.result");
    let db = pool(&state)?;

    // Authorization and its global identity fence are deliberately short-lived.
    // Never hold the identity fence or tenant/user locks across an upstream network call.
    native_control_event(
        &state,
        &control,
        path.owner_user_id,
        &path.id,
        None,
        &request_action,
        json!({"operation":operation.name()}),
    )
    .await?;

    // Account connection changes/deletion take account -> affinity locks. Keep the
    // same order here and retain only these resource locks while one upstream call
    // is in flight, so the exact encrypted credential snapshot cannot rotate under it.
    let tx = db.begin().await.map_err(|e| {
        ApiError::Internal(format!(
            "Failed to begin native Responses resource lock: {e}"
        ))
    })?;
    let snapshot = ResponseAffinity::find_active_snapshot_for_user(
        &tx,
        path.tenant_id,
        path.owner_user_id,
        &path.id,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Native Responses lookup failed: {e}")))?
    .filter(|r| r.resource_kind.as_deref() == Some(kind))
    .ok_or_else(|| ApiError::NotFound("Native Responses resource not found".into()))?;
    let account_id = snapshot.account_id.ok_or_else(|| {
        ApiError::NotFound("Native Responses resource has no upstream account".into())
    })?;
    let account = Account::find_by_id_for_share(&tx, account_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to load Responses account: {e}")))?
        .ok_or_else(|| ApiError::NotFound("Responses account not found".into()))?;
    let affinity = ResponseAffinity::find_active_for_share_for_user(
        &tx,
        path.tenant_id,
        path.owner_user_id,
        &path.id,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Native Responses lookup failed: {e}")))?
    .filter(|r| r.resource_kind.as_deref() == Some(kind) && r.account_id == Some(account_id))
    .ok_or_else(|| ApiError::Conflict("Native Responses ownership changed".into()))?;
    crate::handlers::responses::authorize_non_pt_account(&tx, path.tenant_id, account_id).await?;
    // The account SHARE lock may have waited behind a credential/configuration change.
    // Re-prove the original signed console session immediately before dispatch
    // without retaining identity locks across the upstream network call.
    control.verify_current(&tx).await?;
    if !account.enabled
        || !account.provider.eq_ignore_ascii_case("openai")
        || !affinity.provider.eq_ignore_ascii_case("openai")
    {
        return Err(ApiError::Conflict(
            "The account owning this Response is no longer OpenAI-compatible".into(),
        ));
    }
    if operation.deletes_resource() && affinity.settlement.is_some() {
        return Err(ApiError::Conflict(
            "This background response cannot be deleted until billing settlement completes".into(),
        ));
    }
    let protocol = ProtocolType::parse(&account.provider).ok_or_else(|| {
        ApiError::Conflict("The account owning this Response has an invalid protocol".into())
    })?;
    let endpoint = if account.endpoint.is_empty() {
        protocol.default_endpoint().to_string()
    } else {
        account.endpoint.clone()
    };
    let api_key = crate::handlers::admin_account::decrypt_account_api_key(
        &account.upstream_api_key_encrypted,
    )?;
    let mut headers = vec![
        ("Authorization".to_string(), format!("Bearer {api_key}")),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];
    headers.extend(crate::handlers::responses::forwarded_responses_headers(
        &client_headers,
        path.tenant_id,
        path.owner_user_id,
    )?);
    let mut url = crate::handlers::responses::native_admin_resource_url(
        &endpoint,
        kind,
        &path.id,
        &operation.suffix(),
    );
    if let Some(q) = query.as_deref().filter(|q| !q.is_empty()) {
        url.push('?');
        url.push_str(q);
    }
    let client = state
        .http_proxy
        .client_for_provider_and_account(&account.provider, Some(account_id));
    let upstream = match client
        .request_json_passthrough(
            operation.method(),
            &url,
            headers,
            body.map(|value| value.to_string()),
            false,
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.rollback().await;
            native_control_event(
                &state,
                &control,
                path.owner_user_id,
                &path.id,
                Some(account_id),
                &result_action,
                json!({"operation":operation.name(),"outcome":"transport_error","account_id":account_id}),
            )
            .await?;
            return Err(crate::error::map_execution_error(e));
        }
    };
    let status = upstream.meta.status;
    let confirmed_delete = operation.deletes_resource()
        && crate::handlers::responses::delete_response_is_confirmed(status);
    if confirmed_delete {
        let n = ResponseAffinity::tombstone_native_resource_for_user(
            &tx,
            path.tenant_id,
            path.owner_user_id,
            &path.id,
            account_id,
            kind,
        )
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to tombstone native Response: {e}")))?;
        if n != 1 {
            return Err(ApiError::Conflict(
                "Native Responses ownership changed before delete acknowledgement".into(),
            ));
        }
    }
    tx.commit().await.map_err(|e| {
        ApiError::Internal(format!(
            "Failed to commit native Responses resource state: {e}"
        ))
    })?;

    // The upstream operation may have taken arbitrarily long. Revalidate the
    // original console session after releasing account/affinity locks; a revoked
    // or expired session never receives the private response. Writes remain one-shot.
    native_control_event(
        &state,
        &control,
        path.owner_user_id,
        &path.id,
        Some(account_id),
        &result_action,
        json!({"operation":operation.name(),"upstream_status":status,"account_id":account_id}),
    )
    .await?;

    if confirmed_delete {
        return Ok(no_store(
            Json(json!({"id":path.id,"object":kind,"deleted":true})).into_response(),
        ));
    }
    if !(200..300).contains(&status) {
        return crate::handlers::responses::admin_passthrough_response(upstream).map(no_store);
    }
    if let NativeOperation::ConversationRemoveItem(item_id) = &operation {
        return Ok(no_store(
            Json(json!({"id":item_id,"object":"conversation.item","deleted":true})).into_response(),
        ));
    }
    if operation.detail() {
        let PassthroughBody::Full(body) = upstream.body else {
            return Err(ApiError::Provider(
                "Native Responses detail unexpectedly returned a stream".into(),
            ));
        };
        let (body, admission) = crate::handlers::responses::prepare_admin_responses_json(body)?;
        let body: Value = serde_json::from_str(&body).map_err(|_| {
            ApiError::Provider("Native Responses detail returned invalid JSON".into())
        })?;
        if body.get("id").and_then(Value::as_str) != Some(path.id.as_str())
            || body.get("object").and_then(Value::as_str) != Some(kind)
        {
            return Err(ApiError::Provider(
                "Native Responses detail returned a mismatched resource".into(),
            ));
        }
        let payload = if kind == "conversation" {
            json!({"summary":native_conversation_summary(&affinity, &body)?,"conversation":body})
        } else {
            json!({"summary":native_summary(&affinity, &body)?,"response":Value::Null,"native_body":body})
        };
        let mut response = no_store(Json(payload).into_response());
        if let Some(admission) = admission {
            crate::handlers::responses::retain_admin_response_body_guard(&mut response, admission);
        }
        return Ok(response);
    }
    crate::handlers::responses::admin_passthrough_response(upstream).map(no_store)
}

async fn list_response_page(
    state: AppState,
    control: store::ResponseControlScope,
    q: ListQuery,
) -> Result<Json<Page<ResponseSummary>>> {
    let mode = parse_mode(&q.mode)?;
    let (p, size, offset) = page(q.page, q.page_size)?;
    let (items, total) = if mode == ModelAccessMode::AccountPool {
        let items = store::admin_list_native_resources(
            pool(&state)?,
            &control,
            q.owner_user_id,
            "response",
            size,
            offset,
        )
        .await?
        .into_iter()
        .map(native_response_index)
        .collect();
        let total = store::admin_count_native_resources(
            pool(&state)?,
            &control,
            q.owner_user_id,
            "response",
        )
        .await?;
        (items, total)
    } else {
        let items = store::admin_list_responses(
            pool(&state)?,
            &control,
            q.owner_user_id,
            mode,
            size,
            offset,
        )
        .await?
        .into_iter()
        .map(local_response_summary)
        .collect();
        let total =
            store::admin_count_responses(pool(&state)?, &control, q.owner_user_id, mode).await?;
        (items, total)
    };
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
    let total = if mode == ModelAccessMode::AccountPool {
        store::admin_count_native_resources(pool(&state)?, &control, q.owner_user_id, "response")
            .await?
    } else {
        store::admin_count_responses(pool(&state)?, &control, q.owner_user_id, mode).await?
    };
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
    headers: HeaderMap,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    response_detail(state, control, path, headers).await
}

pub async fn platform_response_detail(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    Query(reason): Query<RootReason>,
    headers: HeaderMap,
) -> Result<Response> {
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason.reason)?;
    response_detail(state, control, path, headers).await
}

async fn response_detail(
    state: AppState,
    control: store::ResponseControlScope,
    path: ResourcePath,
    headers: HeaderMap,
) -> Result<Response> {
    let mode = parse_mode(&path.mode)?;
    if mode == ModelAccessMode::AccountPool {
        return native_response_operation(
            state,
            control,
            path,
            NativeResponseOperation::Detail,
            None,
            headers,
        )
        .await;
    }

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
    headers: HeaderMap,
) -> Result<Response> {
    let mode = parse_mode(&path.mode)?;
    if mode == ModelAccessMode::AccountPool {
        if body.expected_revision.is_some() {
            return Err(ApiError::BadRequest(
                "account_pool Responses mutations do not use local revisions".into(),
            ));
        }
        let operation = if delete {
            NativeResponseOperation::Delete
        } else {
            NativeResponseOperation::Cancel
        };
        return native_response_operation(state, control, path, operation, None, headers).await;
    }
    let expected_revision = body.expected_revision.ok_or_else(|| {
        ApiError::BadRequest("expected_revision is required for local Responses mutations".into())
    })?;

    let (row, active) = store::admin_cancel_or_delete_response(
        pool(&state)?,
        &control,
        path.owner_user_id,
        mode,
        &path.id,
        expected_revision,
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
    headers: HeaderMap,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    mutate_response(state, control, path, body, false, headers).await
}

pub async fn tenant_response_delete(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    mutate_response(state, control, path, body, true, headers).await
}

pub async fn platform_response_cancel(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    let reason = body.reason.clone().ok_or_else(|| {
        ApiError::BadRequest("root reason is required for platform Responses mutation".into())
    })?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    mutate_response(state, control, path, body, false, headers).await
}

pub async fn platform_response_delete(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    let reason = body.reason.clone().ok_or_else(|| {
        ApiError::BadRequest("root reason is required for platform Responses mutation".into())
    })?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    mutate_response(state, control, path, body, true, headers).await
}

pub async fn tenant_response_input_items(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    response_input_items(state, control, path, query, headers).await
}

pub async fn platform_response_input_items(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response> {
    let reason = reason_from_raw(query.as_deref())?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    response_input_items(state, control, path, query, headers).await
}

async fn response_input_items(
    state: AppState,
    control: store::ResponseControlScope,
    path: ResourcePath,
    query: Option<String>,
    headers: HeaderMap,
) -> Result<Response> {
    let mode = parse_mode(&path.mode)?;
    if mode == ModelAccessMode::AccountPool {
        let query = native_item_query(query.as_deref())?;
        return native_response_operation(
            state,
            control,
            path,
            NativeResponseOperation::InputItems,
            query,
            headers,
        )
        .await;
    }
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
    let (items, total) = if mode == ModelAccessMode::AccountPool {
        let items = store::admin_list_native_resources(
            pool(&state)?,
            &control,
            q.owner_user_id,
            "conversation",
            size,
            offset,
        )
        .await?
        .into_iter()
        .map(native_conversation_index)
        .collect();
        let total = store::admin_count_native_resources(
            pool(&state)?,
            &control,
            q.owner_user_id,
            "conversation",
        )
        .await?;
        (items, total)
    } else {
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
            store::admin_count_conversations(pool(&state)?, &control, q.owner_user_id, mode)
                .await?;
        (items, total)
    };
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
        total: if mode == ModelAccessMode::AccountPool {
            store::admin_count_native_resources(
                pool(&state)?,
                &control,
                q.owner_user_id,
                "conversation",
            )
            .await?
        } else {
            store::admin_count_conversations(pool(&state)?, &control, q.owner_user_id, mode).await?
        },
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
    headers: HeaderMap,
) -> Result<Response> {
    let mode = parse_mode(&path.mode)?;
    if mode == ModelAccessMode::AccountPool {
        return native_resource_operation(
            state,
            control,
            path,
            NativeRequest {
                operation: NativeOperation::ConversationDetail,
                query: None,
                body: None,
                headers,
            },
        )
        .await;
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
    headers: HeaderMap,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    conversation_detail(state, control, path, headers).await
}

pub async fn platform_conversation_detail(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    Query(reason): Query<RootReason>,
    headers: HeaderMap,
) -> Result<Response> {
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason.reason)?;
    conversation_detail(state, control, path, headers).await
}

async fn mutate_conversation(
    state: AppState,
    control: store::ResponseControlScope,
    path: ResourcePath,
    expected_revision: Option<i64>,
    change: ConversationMutation,
    headers: HeaderMap,
) -> Result<Response> {
    let mode = parse_mode(&path.mode)?;
    if mode == ModelAccessMode::AccountPool {
        if expected_revision.is_some() {
            return Err(ApiError::BadRequest(
                "Native Conversations do not use local revisions".into(),
            ));
        }
        let (operation, body) = match change {
            ConversationMutation::Metadata(metadata) => (
                NativeOperation::ConversationMetadata,
                Some(json!({"metadata":metadata})),
            ),
            ConversationMutation::Append(items) => {
                if items.is_empty() || items.len() > 20 || items.iter().any(|v| !v.is_object()) {
                    return Err(ApiError::BadRequest(
                        "Use one to 20 native conversation item objects".into(),
                    ));
                }
                (
                    NativeOperation::ConversationAppend,
                    Some(json!({"items":items})),
                )
            }
            ConversationMutation::RemoveItem(id) => {
                validate_native_selector(path.owner_user_id, &id)?;
                (NativeOperation::ConversationRemoveItem(id), None)
            }
            ConversationMutation::Delete => (NativeOperation::ConversationDelete, None),
        };
        return native_resource_operation(
            state,
            control,
            path,
            NativeRequest {
                operation,
                query: None,
                body,
                headers,
            },
        )
        .await;
    }
    let expected_revision = expected_revision.ok_or_else(|| {
        ApiError::BadRequest(
            "expected_revision is required for local Conversation mutations".into(),
        )
    })?;

    let delete = matches!(change, ConversationMutation::Delete);
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
    if delete {
        Ok(no_store(
            Json(json!({"id":row.id,"object":"conversation","deleted":true})).into_response(),
        ))
    } else {
        Ok(no_store(
            Json(store::conversation_view(&row)).into_response(),
        ))
    }
}

pub async fn tenant_conversation_update(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    headers: HeaderMap,
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
        headers,
    )
    .await
}

pub async fn tenant_conversation_delete(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    mutate_conversation(
        state,
        control,
        path,
        body.expected_revision,
        ConversationMutation::Delete,
        headers,
    )
    .await
}

pub async fn platform_conversation_update(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    headers: HeaderMap,
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
        headers,
    )
    .await
}

pub async fn platform_conversation_delete(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RevisionBody>,
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
        ConversationMutation::Delete,
        headers,
    )
    .await
}

pub async fn tenant_conversation_items(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    conversation_items(state, control, path, query, headers).await
}

pub async fn platform_conversation_items(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response> {
    let reason = reason_from_raw(query.as_deref())?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    conversation_items(state, control, path, query, headers).await
}

async fn conversation_items(
    state: AppState,
    control: store::ResponseControlScope,
    path: ResourcePath,
    query: Option<String>,
    headers: HeaderMap,
) -> Result<Response> {
    let mode = parse_mode(&path.mode)?;
    if mode == ModelAccessMode::AccountPool {
        let query = native_item_query(query.as_deref())?;
        return native_resource_operation(
            state,
            control,
            path,
            NativeRequest {
                operation: NativeOperation::ConversationItems,
                query,
                body: None,
                headers,
            },
        )
        .await;
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
    headers: HeaderMap,
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
        headers,
    )
    .await
}

pub async fn platform_conversation_append(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ResourcePath>,
    State(state): State<AppState>,
    headers: HeaderMap,
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
        headers,
    )
    .await
}

pub async fn tenant_conversation_remove_item(
    access: TenantAdmin,
    request_id: RequestId,
    Path(path): Path<ItemPath>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    access.require_path_tenant(path.tenant_id)?;
    let control = tenant_control_scope(&access, request_id)?;
    remove_item(state, control, path, body, headers).await
}

pub async fn platform_conversation_remove_item(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(path): Path<ItemPath>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RevisionBody>,
) -> Result<Response> {
    let reason = body.reason.clone().ok_or_else(|| {
        ApiError::BadRequest("root reason is required for platform Conversations mutation".into())
    })?;
    let control = root_control_scope(&auth, request_id, path.tenant_id, reason)?;
    remove_item(state, control, path, body, headers).await
}

async fn remove_item(
    state: AppState,
    control: store::ResponseControlScope,
    path: ItemPath,
    body: RevisionBody,
    headers: HeaderMap,
) -> Result<Response> {
    let item_id = path.item_id.clone();
    let native = parse_mode(&path.mode)? == ModelAccessMode::AccountPool;
    let response = mutate_conversation(
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
        headers,
    )
    .await?;
    if native {
        return Ok(response);
    }
    Ok(no_store(
        Json(json!({"id":item_id,"object":"conversation.item","deleted":true})).into_response(),
    ))
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
        assert_eq!(
            parse_mode("account_pool").unwrap(),
            ModelAccessMode::AccountPool
        );
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
