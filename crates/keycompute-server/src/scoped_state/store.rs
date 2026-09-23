//! Bounded, writer-authoritative platform state. Transactions never span inference.
use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
};
use chrono::{DateTime, Utc};
use keycompute_db::{AuditContext, DbRouter, TenantAuditEvent};
use keycompute_types::{
    AuditResult, AuditScopeType, CredentialKind, ModelAccessMode, PlatformRole, PlatformScope,
    TenantRole, TenantScope,
};
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, DbErr, FromQueryResult, Statement,
    TransactionTrait,
};
use serde_json::{Value, json};
use std::{future::Future, time::Duration};
use uuid::Uuid;
pub const MAX_HISTORY_ITEMS: usize = 512;
pub const MAX_HISTORY_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_STREAM_EVENTS: i64 = 4096;
pub const STORED_TTL: i64 = 30 * 24 * 60 * 60;
pub const TEMP_TTL: i64 = 600;
pub const MAX_RETAINED_RESPONSES: i64 = 1024;
pub const MAX_CONVERSATIONS: i64 = 256;
const ROOT_REASON_MAX: usize = 500;
#[derive(Debug, Clone, Copy)]
pub struct Scope {
    pub tenant: Uuid,
    pub user: Uuid,
    pub mode: ModelAccessMode,
}
impl Scope {
    pub fn new(auth: &AuthExtractor, mode: ModelAccessMode) -> Result<Self> {
        if mode == ModelAccessMode::AccountPool {
            return Err(ApiError::BadRequest("Use the scoped Responses URL".into()));
        }
        Ok(Self {
            tenant: auth.tenant_id,
            user: auth.user_id,
            mode,
        })
    }
    fn values(self) -> [sea_orm::Value; 3] {
        [
            self.tenant.into(),
            self.user.into(),
            self.mode.as_str().into(),
        ]
    }
}
fn storage(_: DbErr) -> ApiError {
    ApiError::ServiceUnavailable("Platform response state is temporarily unavailable".into())
}
pub fn missing() -> ApiError {
    ApiError::NotFound(
        "Response or conversation was not found in this user and execution mode".into(),
    )
}
pub fn conflict(message: &str) -> ApiError {
    ApiError::Conflict(message.into())
}
async fn timed<T>(future: impl Future<Output = std::result::Result<T, DbErr>>) -> Result<T> {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Platform state query timed out".into()))?
        .map_err(storage)
}
#[derive(Clone, FromQueryResult)]
pub struct ResponseRecord {
    pub id: String,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub access_mode: String,
    pub account_id: Option<Uuid>,
    pub model: String,
    pub request_id: Uuid,
    pub owner_id: Uuid,
    pub status: String,
    pub background: bool,
    pub store_response: bool,
    pub stream: bool,
    pub request_json: Value,
    pub input_json: Value,
    pub new_input_json: Value,
    pub output_json: Value,
    pub response_json: Option<Value>,
    pub execution_json: Option<Value>,
    pub previous_id: Option<String>,
    pub conversation_id: Option<String>,
    pub idempotency_hash: Option<String>,
    pub request_hash: String,
    pub next_seq: i64,
    pub event_bytes: i64,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub heartbeat_at: DateTime<Utc>,
    pub deadline_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}
#[derive(Clone, FromQueryResult)]
pub struct ConversationRecord {
    pub id: String,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub access_mode: String,
    pub account_id: Option<Uuid>,
    pub model: Option<String>,
    pub metadata_json: Value,
    pub items_json: Value,
    pub active_response_id: Option<String>,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}
#[derive(Clone, FromQueryResult)]
pub struct StoredEvent {
    pub seq: i64,
    pub frame: String,
}
impl ResponseRecord {
    pub fn active(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "in_progress")
    }
    pub fn retained(&self) -> bool {
        self.background || self.store_response
    }
    pub fn scope(&self) -> Scope {
        Scope {
            tenant: self.tenant_id,
            user: self.user_id,
            mode: if self.access_mode == "passthrough" {
                ModelAccessMode::Passthrough
            } else {
                ModelAccessMode::NodeDispatch
            },
        }
    }
}
async fn owner_transaction(pool: &DbRouter, scope: Scope) -> Result<DatabaseTransaction> {
    let tx = timed(pool.begin()).await?;
    timed(tx.execute_unprepared(
        "SET LOCAL statement_timeout='2500ms'; SET LOCAL lock_timeout='1000ms'",
    ))
    .await?;
    timed(tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1,7201))",
        [format!("{}:{}:{}", scope.tenant, scope.user, scope.mode.as_str()).into()],
    )))
    .await?;
    Ok(tx)
}
pub async fn transaction(pool: &DbRouter, scope: Scope) -> Result<DatabaseTransaction> {
    let tx = owner_transaction(pool, scope).await?;
    check_scope(&tx, scope).await?;
    Ok(tx)
}

#[derive(Debug, Clone, Copy)]
pub struct ResponseControlMembership {
    pub tenant_id: Uuid,
    pub tenant_role: TenantRole,
    pub tenant_authz_version: i64,
    pub membership_authz_version: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct ResponseControlSession {
    pub token_version: i32,
    pub jwt_expires_at: i64,
    pub selected: Option<ResponseControlMembership>,
}

#[derive(Debug, Clone, Copy)]
enum ResponseControlAuthority {
    TenantAdmin {
        scope: TenantScope,
        token_version: i32,
        tenant_authz_version: i64,
        membership_authz_version: i64,
        jwt_expires_at: i64,
    },
    Root {
        scope: PlatformScope,
        token_version: i32,
        jwt_expires_at: i64,
        selected: Option<ResponseControlMembership>,
    },
}

/// Typed, non-deserializable scope. Every use verifies current database authority.
#[derive(Debug, Clone)]
pub struct ResponseControlScope {
    tenant_id: Uuid,
    audit: AuditContext,
    authority: ResponseControlAuthority,
    root_reason: Option<String>,
}

impl ResponseControlScope {
    pub fn tenant_admin(
        scope: TenantScope,
        audit: AuditContext,
        token_version: i32,
        tenant_authz_version: i64,
        membership_authz_version: i64,
        jwt_expires_at: i64,
    ) -> Result<Self> {
        if scope.tenant_id().is_nil()
            || scope.user_id().is_nil()
            || scope.tenant_role() != TenantRole::Admin
            || audit.actor_user_id != scope.user_id()
            || audit.credential_kind != CredentialKind::Jwt
            || audit.request_id.is_none_or(|id| id.is_nil())
            || token_version < 0
            || tenant_authz_version <= 0
            || membership_authz_version <= 0
            || jwt_expires_at <= 0
        {
            return Err(ApiError::Forbidden(
                "tenant administrator console authority required".into(),
            ));
        }
        Ok(Self {
            tenant_id: scope.tenant_id(),
            audit,
            authority: ResponseControlAuthority::TenantAdmin {
                scope,
                token_version,
                tenant_authz_version,
                membership_authz_version,
                jwt_expires_at,
            },
            root_reason: None,
        })
    }

    pub fn root(
        scope: PlatformScope,
        target_tenant_id: Uuid,
        audit: AuditContext,
        session: ResponseControlSession,
        reason: impl Into<String>,
    ) -> Result<Self> {
        let reason = validate_root_reason(reason.into())?;
        let ResponseControlSession {
            token_version,
            jwt_expires_at,
            selected,
        } = session;
        if scope.platform_role() != PlatformRole::Root
            || scope.user_id().is_nil()
            || audit.actor_user_id != scope.user_id()
            || audit.credential_kind != CredentialKind::Jwt
            || audit.request_id.is_none_or(|id| id.is_nil())
            || token_version < 0
            || jwt_expires_at <= 0
            || target_tenant_id.is_nil()
        {
            return Err(ApiError::Forbidden(
                "root console authority required".into(),
            ));
        }
        if selected.is_some_and(|snapshot| {
            snapshot.tenant_id.is_nil()
                || snapshot.tenant_authz_version <= 0
                || snapshot.membership_authz_version <= 0
        }) {
            return Err(ApiError::Forbidden(
                "selected tenant membership snapshot required".into(),
            ));
        }
        Ok(Self {
            tenant_id: target_tenant_id,
            audit,
            authority: ResponseControlAuthority::Root {
                scope,
                token_version,
                jwt_expires_at,
                selected,
            },
            root_reason: Some(reason),
        })
    }

    pub const fn tenant_id(&self) -> Uuid {
        self.tenant_id
    }

    fn root_reason(&self) -> Option<&str> {
        self.root_reason.as_deref()
    }

    pub(crate) fn check_expiry(&self) -> Result<()> {
        let expires = match self.authority {
            ResponseControlAuthority::TenantAdmin { jwt_expires_at, .. }
            | ResponseControlAuthority::Root { jwt_expires_at, .. } => jwt_expires_at,
        };
        if expires <= Utc::now().timestamp() {
            return Err(ApiError::Auth("console session expired".into()));
        }
        Ok(())
    }

    pub(crate) async fn revalidate_for_admin(
        &self,
        tx: &DatabaseTransaction,
    ) -> Result<AuditContext> {
        let denied = || ApiError::Forbidden("Responses control authority changed".into());
        if self.audit.request_id.is_none_or(|id| id.is_nil())
            || self.audit.credential_kind != CredentialKind::Jwt
        {
            return Err(denied());
        }
        self.check_expiry()?;
        // Called only after the original-resource owner's advisory lock, when
        // one is needed. All parent rows precede users and memberships.
        timed(
            tx.execute_unprepared(
                "UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE",
            ),
        )
        .await?;
        let (actor, token_version, selected, require_root) = match self.authority {
            ResponseControlAuthority::TenantAdmin {
                scope,
                token_version,
                tenant_authz_version,
                membership_authz_version,
                ..
            } => (
                scope.user_id(),
                token_version,
                Some(ResponseControlMembership {
                    tenant_id: scope.tenant_id(),
                    tenant_role: TenantRole::Admin,
                    tenant_authz_version,
                    membership_authz_version,
                }),
                false,
            ),
            ResponseControlAuthority::Root {
                scope,
                token_version,
                selected,
                ..
            } => (scope.user_id(), token_version, selected, true),
        };
        let mut parents = vec![self.tenant_id];
        if let Some(origin) = selected {
            parents.push(origin.tenant_id);
        }
        parents.sort_unstable();
        parents.dedup();
        for tenant in parents {
            let row = timed(tx.query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,authz_version FROM tenants WHERE id=$1 FOR UPDATE",
                [tenant.into()],
            )))
            .await?
            .ok_or_else(denied)?;
            if let Some(origin) = selected.filter(|s| s.tenant_id == tenant)
                && (row.try_get::<String>("", "status").map_err(storage)? != "active"
                    || row.try_get::<i64>("", "authz_version").map_err(storage)?
                        != origin.tenant_authz_version)
            {
                return Err(denied());
            }
        }
        let row = timed(tx.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT platform_role FROM users WHERE id=$1 AND status='active' AND token_version=$2 FOR UPDATE",
            [actor.into(), token_version.into()],
        ))).await?.ok_or_else(denied)?;
        let platform_role: PlatformRole = row
            .try_get::<String>("", "platform_role")
            .map_err(storage)?
            .parse()
            .map_err(|_| denied())?;
        if require_root && platform_role != PlatformRole::Root {
            return Err(denied());
        }
        if let Some(origin) = selected {
            let row = timed(tx.query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT tenant_role FROM tenant_memberships WHERE tenant_id=$1 AND user_id=$2 AND status='active' AND authz_version=$3 FOR UPDATE",
                [origin.tenant_id.into(), actor.into(), origin.membership_authz_version.into()],
            ))).await?.ok_or_else(denied)?;
            if row.try_get::<String>("", "tenant_role").map_err(storage)?
                != origin.tenant_role.as_str()
            {
                return Err(denied());
            }
        }
        // A credential may expire while any parent/user/member lock is held
        // elsewhere. The check before waiting is not sufficient.
        self.check_expiry()?;
        Ok(AuditContext {
            actor_platform_role: platform_role,
            actor_tenant_role: if require_root {
                None
            } else {
                Some(TenantRole::Admin)
            },
            ..self.audit
        })
    }
}

fn validate_root_reason(reason: String) -> Result<String> {
    let trimmed = reason.trim();
    if trimmed.is_empty() || reason.len() > ROOT_REASON_MAX || reason.chars().any(char::is_control)
    {
        return Err(ApiError::BadRequest(
            "root reason must be nonempty, at most 500 bytes and contain no control characters"
                .into(),
        ));
    }
    Ok(trimmed.to_owned())
}
pub async fn check_scope(db: &impl ConnectionTrait, scope: Scope) -> Result<()> {
    let row=timed(db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT 1 FROM tenant_memberships m JOIN tenants t ON t.id=m.tenant_id JOIN users u ON u.id=m.user_id WHERE m.user_id=$1 AND m.tenant_id=$2 AND m.status='active' AND t.status='active' AND u.status='active'",
        [scope.user.into(),scope.tenant.into()]))).await?;
    if row.is_none() {
        return Err(missing());
    }
    Ok(())
}
pub async fn check_account(
    db: &impl ConnectionTrait,
    scope: Scope,
    account: Option<Uuid>,
) -> Result<()> {
    check_scope(db, scope).await?;
    if scope.mode != ModelAccessMode::Passthrough {
        return Ok(());
    }
    let Some(account) = account else {
        return Ok(());
    };
    let row=timed(db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT 1 FROM accounts a JOIN tenants owner ON owner.id=a.tenant_id AND owner.status='active' JOIN passthrough_bindings p ON p.account_id=a.id JOIN tenants anchor ON anchor.id=p.tenant_id AND anchor.status='active' WHERE a.id=$1 AND a.enabled AND a.provider='openai' AND a.api_capabilities @> ARRAY['responses']::TEXT[] AND (p.tenant_id=$2 OR p.is_global) LIMIT 1",
        [account.into(),scope.tenant.into()]))).await?;
    if row.is_none() {
        return Err(missing());
    }
    Ok(())
}
pub async fn response(
    db: &impl ConnectionTrait,
    scope: Scope,
    id: &str,
    deleted: bool,
) -> Result<ResponseRecord> {
    let row=timed(ResponseRecord::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT * FROM scoped_responses WHERE tenant_id=$1 AND user_id=$2 AND access_mode=$3 AND id=$4 AND expires_at>NOW() AND ($5 OR deleted_at IS NULL)",
        scope.values().into_iter().chain([id.into(),deleted.into()]))).one(db)).await?.ok_or_else(missing)?;
    check_account(db, scope, row.account_id).await?;
    Ok(row)
}
pub async fn conversation(
    db: &impl ConnectionTrait,
    scope: Scope,
    id: &str,
    deleted: bool,
) -> Result<ConversationRecord> {
    let row=timed(ConversationRecord::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT * FROM scoped_conversations WHERE tenant_id=$1 AND user_id=$2 AND access_mode=$3 AND id=$4 AND expires_at>NOW() AND ($5 OR deleted_at IS NULL)",
        scope.values().into_iter().chain([id.into(),deleted.into()]))).one(db)).await?.ok_or_else(missing)?;
    check_account(db, scope, row.account_id).await?;
    Ok(row)
}
pub fn metadata(value: Option<&Value>) -> Result<Value> {
    let value = value
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| json!({}));
    if value.as_object().is_none_or(|m| {
        m.len() > 16
            || m.iter().any(|(k, v)| {
                k.chars().count() > 64 || v.as_str().is_none_or(|s| s.chars().count() > 512)
            })
    }) {
        return Err(ApiError::BadRequest(
            "metadata must contain at most 16 string pairs (key 64, value 512 characters)".into(),
        ));
    }
    Ok(value)
}
pub fn normalize_input(value: Option<&Value>) -> Result<Vec<Value>> {
    let items = match value {
        None | Some(Value::Null) => vec![],
        Some(Value::String(text)) => vec![json!({"role":"user","content":text})],
        Some(Value::Array(items)) if items.iter().all(Value::is_object) => items.clone(),
        _ => {
            return Err(ApiError::BadRequest(
                "input must be a string or an array of native item objects".into(),
            ));
        }
    };
    check_history(&items)?;
    Ok(items)
}
pub fn check_history(items: &[Value]) -> Result<()> {
    if items.len() > MAX_HISTORY_ITEMS
        || serde_json::to_vec(items)
            .map_err(|_| ApiError::BadRequest("Invalid item data".into()))?
            .len()
            > MAX_HISTORY_BYTES
    {
        return Err(ApiError::BadRequest(
            "Resolved context exceeds 512 items or 2 MiB; no history was truncated".into(),
        ));
    }
    Ok(())
}
fn wrap_items(items: Vec<Value>) -> Vec<Value> {
    items
        .into_iter()
        .map(|body| json!({"id":format!("item_{}",Uuid::new_v4().simple()),"body":body}))
        .collect()
}
pub fn item_bodies(items: &Value) -> Vec<Value> {
    items
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("body").cloned())
        .collect()
}
pub async fn create_conversation(
    pool: &DbRouter,
    scope: Scope,
    meta: Value,
    items: Vec<Value>,
) -> Result<ConversationRecord> {
    if items.len() > 20 {
        return Err(ApiError::BadRequest(
            "Add at most 20 conversation items per request".into(),
        ));
    }
    check_history(&items)?;
    let items = json!(wrap_items(items));
    let id = format!("conv_{}", Uuid::new_v4().simple());
    let tx = transaction(pool, scope).await?;
    let count=timed(tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT AS n FROM scoped_conversations WHERE tenant_id=$1 AND user_id=$2 AND access_mode=$3 AND deleted_at IS NULL AND expires_at>NOW()",scope.values()))).await?
        .ok_or_else(missing)?.try_get::<i64>("","n").map_err(storage)?;
    if count >= MAX_CONVERSATIONS {
        return Err(ApiError::RateLimit(
            "Conversation storage quota reached; delete unused conversations".into(),
        ));
    }
    let row=timed(ConversationRecord::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO scoped_conversations(id,tenant_id,user_id,access_mode,metadata_json,items_json,expires_at) VALUES($1,$2,$3,$4,$5,$6,NOW()+($7::BIGINT*INTERVAL '1 second')) RETURNING *",
        [id.into(),scope.tenant.into(),scope.user.into(),scope.mode.as_str().into(),meta.into(),items.into(),STORED_TTL.into()])).one(&tx)).await?.ok_or_else(missing)?;
    timed(tx.commit()).await?;
    Ok(row)
}
pub enum ConversationMutation {
    Metadata(Value),
    Append(Vec<Value>),
    RemoveItem(String),
    Delete,
}
pub async fn mutate_conversation(
    pool: &DbRouter,
    scope: Scope,
    id: &str,
    change: ConversationMutation,
) -> Result<ConversationRecord> {
    let tx = transaction(pool, scope).await?;
    let row = conversation(
        &tx,
        scope,
        id,
        matches!(change, ConversationMutation::Delete),
    )
    .await?;
    let (updated, _) = mutate_conversation_record(&tx, row, change).await?;
    timed(tx.commit()).await?;
    Ok(updated)
}

pub struct NewResponse {
    pub request_id: Uuid,
    pub body: Value,
    pub model: String,
    pub input: Vec<Value>,
    pub previous: Option<String>,
    pub conversation: Option<String>,
    pub background: bool,
    pub store: bool,
    pub stream: bool,
    pub deadline_secs: i64,
    pub idempotency_hash: Option<String>,
    pub request_hash: String,
}
#[derive(FromQueryResult)]
struct Clock {
    now: DateTime<Utc>,
}
async fn clock(db: &impl ConnectionTrait) -> Result<DateTime<Utc>> {
    Ok(timed(
        Clock::find_by_statement(Statement::from_string(
            DbBackend::Postgres,
            "SELECT NOW() AS now".to_string(),
        ))
        .one(db),
    )
    .await?
    .ok_or_else(missing)?
    .now)
}
pub async fn create_response(
    pool: &DbRouter,
    scope: Scope,
    mut spec: NewResponse,
) -> Result<(ResponseRecord, bool)> {
    let tx = transaction(pool, scope).await?;
    if let Some(key) = &spec.idempotency_hash {
        let existing=timed(ResponseRecord::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT * FROM scoped_responses WHERE tenant_id=$1 AND user_id=$2 AND access_mode=$3 AND idempotency_hash=$4",
            scope.values().into_iter().chain([key.into()]))).one(&tx)).await?;
        if let Some(row) = existing {
            if row.request_hash != spec.request_hash {
                return Err(conflict(
                    "Idempotency-Key was already used with a different request",
                ));
            }
            check_account(&tx, scope, row.account_id).await?;
            if row.deleted_at.is_some() || row.expires_at <= clock(&tx).await? {
                return Err(missing());
            }
            timed(tx.commit()).await?;
            return Ok((row, false));
        }
    }
    let count=timed(tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT AS n FROM scoped_responses WHERE tenant_id=$1 AND user_id=$2 AND access_mode=$3 AND status IN ('queued','in_progress') AND deleted_at IS NULL",
        scope.values()))).await?.ok_or_else(missing)?.try_get::<i64>("","n").map_err(storage)?;
    let retained=timed(tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT AS n FROM scoped_responses WHERE tenant_id=$1 AND user_id=$2 AND access_mode=$3 AND deleted_at IS NULL AND expires_at>NOW()",scope.values()))).await?
        .ok_or_else(missing)?.try_get::<i64>("","n").map_err(storage)?;
    if retained >= MAX_RETAINED_RESPONSES {
        return Err(ApiError::RateLimit(
            "Response storage quota reached; delete unused responses".into(),
        ));
    }
    if count >= 32 {
        return Err(ApiError::RateLimit(
            "At most 32 managed responses may be active for this user and mode".into(),
        ));
    }
    if spec.previous.is_some() && spec.conversation.is_some() {
        return Err(ApiError::BadRequest(
            "previous_response_id and conversation are mutually exclusive".into(),
        ));
    }
    let mut input = Vec::new();
    let mut account = None;
    if let Some(id) = &spec.previous {
        let parent = response(&tx, scope, id, false).await?;
        if !parent.retained() {
            return Err(missing());
        }
        if !matches!(parent.status.as_str(), "completed" | "incomplete") {
            return Err(conflict("The previous response has not completed"));
        }
        input.extend(
            parent
                .input_json
                .as_array()
                .ok_or_else(missing)?
                .iter()
                .cloned(),
        );
        input.extend(
            parent
                .output_json
                .as_array()
                .ok_or_else(missing)?
                .iter()
                .cloned(),
        );
        account = parent.account_id;
        if spec.model.is_empty() {
            spec.model = parent.model;
        }
    }
    if let Some(id) = &spec.conversation {
        let conv = conversation(&tx, scope, id, false).await?;
        if conv.active_response_id.is_some() {
            return Err(conflict("Conversation has an active response"));
        }
        input.extend(item_bodies(&conv.items_json));
        account = conv.account_id;
        if spec.model.is_empty() {
            spec.model = conv.model.unwrap_or_default();
        }
    }
    if spec.model.trim().is_empty() {
        return Err(ApiError::BadRequest("model is required".into()));
    }
    let new_input = json!(spec.input.clone());
    input.extend(spec.input);
    check_history(&input)?;
    let id = format!("resp_{}", Uuid::new_v4().simple());
    let owner = Uuid::new_v4();
    let ttl = if spec.store {
        STORED_TTL
    } else {
        TEMP_TTL + spec.deadline_secs
    };
    let query = r#"
        INSERT INTO scoped_responses (
            id, tenant_id, user_id, access_mode, account_id, model,
            request_id, owner_id, status, background, store_response, stream,
            request_json, input_json, new_input_json, previous_id,
            conversation_id, idempotency_hash, request_hash, deadline_at, expires_at
        ) VALUES (
            $1,$2,$3,$4,$5,$6,$7,$8,'queued',$9,$10,$11,$12,$13,$14,
            $15,$16,$17,$18,
            NOW()+($19::BIGINT*INTERVAL '1 second'),
            NOW()+($20::BIGINT*INTERVAL '1 second')
        ) RETURNING *
    "#;
    let row = timed(
        ResponseRecord::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            query,
            [
                id.clone().into(),
                scope.tenant.into(),
                scope.user.into(),
                scope.mode.as_str().into(),
                account.into(),
                spec.model.into(),
                spec.request_id.into(),
                owner.into(),
                spec.background.into(),
                spec.store.into(),
                spec.stream.into(),
                spec.body.into(),
                json!(input).into(),
                new_input.into(),
                spec.previous.into(),
                spec.conversation.clone().into(),
                spec.idempotency_hash.into(),
                spec.request_hash.into(),
                spec.deadline_secs.into(),
                ttl.into(),
            ],
        ))
        .one(&tx),
    )
    .await?
    .ok_or_else(missing)?;
    if let Some(conv) = spec.conversation {
        let updated = timed(tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE scoped_conversations SET active_response_id=$2,
             revision=revision+1,updated_at=NOW()
             WHERE id=$1 AND tenant_id=$3 AND user_id=$4 AND access_mode=$5 AND active_response_id IS NULL",
            [conv.into(), id.into(), scope.tenant.into(), scope.user.into(), scope.mode.as_str().into()],
        )))
        .await?;
        if updated.rows_affected() != 1 {
            return Err(conflict("Conversation execution ownership changed"));
        }
    }
    timed(tx.commit()).await?;
    Ok((row, true))
}
pub async fn activate(
    pool: &DbRouter,
    record: &ResponseRecord,
    account: Option<Uuid>,
    execution: Value,
) -> Result<()> {
    let scope = record.scope();
    let tx = transaction(pool, scope).await?;
    let current = response(&tx, scope, &record.id, false).await?;
    if current.owner_id != record.owner_id
        || current.status != "queued"
        || current.deadline_at <= clock(&tx).await?
    {
        return Err(conflict("Response is no longer queued for this execution"));
    }
    if current.account_id.is_some() && current.account_id != account {
        return Err(conflict(
            "Continuation must use the originally authorized account",
        ));
    }
    if scope.mode == ModelAccessMode::Passthrough && account.is_none() {
        return Err(ApiError::Internal("Missing scoped account target".into()));
    }
    check_account(&tx, scope, account).await?;
    if let Some(id) = &record.conversation_id {
        let conv = conversation(&tx, scope, id, false).await?;
        if conv.active_response_id.as_deref() != Some(&record.id)
            || (conv.account_id.is_some() && conv.account_id != account)
        {
            return Err(conflict("Conversation execution target changed"));
        }
        let updated = timed(tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE scoped_conversations SET account_id=$2 WHERE id=$1 AND tenant_id=$3 AND user_id=$4 AND access_mode=$5",
            [id.into(), account.into(), scope.tenant.into(), scope.user.into(), scope.mode.as_str().into()],
        )))
        .await?;
        if updated.rows_affected() != 1 {
            return Err(conflict("Conversation execution ownership changed"));
        }
    }
    let updated = timed(tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE scoped_responses SET status='in_progress',account_id=$6,
         execution_json=$7,heartbeat_at=NOW(),updated_at=NOW(),revision=revision+1 WHERE id=$1 AND owner_id=$2 AND tenant_id=$3 AND user_id=$4 AND access_mode=$5",
        [record.id.as_str().into(), record.owner_id.into(), scope.tenant.into(), scope.user.into(), scope.mode.as_str().into(), account.into(), execution.into()],
    )))
    .await?;
    if updated.rows_affected() != 1 {
        return Err(conflict("Response execution ownership changed"));
    }
    timed(tx.commit()).await?;
    Ok(())
}
pub async fn running(pool: &DbRouter, record: &ResponseRecord) -> Result<bool> {
    let scope = record.scope();
    let result = timed(pool.write_conn().execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE scoped_responses SET heartbeat_at=NOW() WHERE id=$1 AND owner_id=$2
         AND tenant_id=$3 AND user_id=$4 AND access_mode=$5
         AND status IN ('queued','in_progress') AND deleted_at IS NULL AND deadline_at>NOW()",
        [
            record.id.as_str().into(),
            record.owner_id.into(),
            scope.tenant.into(),
            scope.user.into(),
            scope.mode.as_str().into(),
        ],
    )))
    .await?;
    Ok(result.rows_affected() == 1)
}
pub async fn record_execution(
    pool: &DbRouter,
    record: &ResponseRecord,
    result: Value,
) -> Result<bool> {
    if serde_json::to_vec(&result)
        .map_err(|_| ApiError::Provider("Invalid response data".into()))?
        .len()
        > MAX_RESPONSE_BYTES
    {
        return Err(ApiError::Provider(
            "Managed response exceeds the 8 MiB limit".into(),
        ));
    }
    let scope = record.scope();
    let result = timed(pool.write_conn().execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE scoped_responses SET execution_json=COALESCE(execution_json,'{}'::jsonb)||(CASE WHEN deleted_at IS NULL THEN $3 ELSE $3-'native_result' END),
         updated_at=NOW() WHERE id=$1 AND owner_id=$2 AND tenant_id=$4 AND user_id=$5 AND access_mode=$6
         AND status IN ('queued','in_progress','cancelled')",
        [
            record.id.as_str().into(),
            record.owner_id.into(),
            result.into(),
            scope.tenant.into(),
            scope.user.into(),
            scope.mode.as_str().into(),
        ],
    )))
    .await?;
    Ok(result.rows_affected() == 1)
}
pub async fn release_conversation(
    db: &impl ConnectionTrait,
    record: &ResponseRecord,
) -> Result<()> {
    if let Some(id) = &record.conversation_id {
        let scope = record.scope();
        timed(db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE scoped_conversations SET active_response_id=NULL,updated_at=NOW(),
             revision=revision+1 WHERE id=$1 AND active_response_id=$2 AND tenant_id=$3 AND user_id=$4 AND access_mode=$5",
            [id.into(), record.id.as_str().into(), scope.tenant.into(), scope.user.into(), scope.mode.as_str().into()],
        )))
        .await?;
    }
    Ok(())
}
pub fn public_response(record: &ResponseRecord) -> Value {
    if let Some(value) = &record.response_json {
        return value.clone();
    }
    json!({"id":record.id,"object":"response","created_at":record.created_at.timestamp(),
        "status":record.status,"model":record.model,"output":record.output_json,
        "error":null,"incomplete_details":null,"usage":null,
        "background":record.background,"store":record.store_response,
        "previous_response_id":record.previous_id,
        "conversation":record.conversation_id.as_ref().map(|id|json!({"id":id})),
        "instructions":record.request_json.get("instructions"),
        "metadata":record.request_json.get("metadata").cloned().unwrap_or_else(||json!({})),
        "tools":record.request_json.get("tools").cloned().unwrap_or_else(||json!([])),
        "tool_choice":record.request_json.get("tool_choice").cloned().unwrap_or_else(||json!("auto")),
        "parallel_tool_calls":record.request_json.get("parallel_tool_calls").cloned().unwrap_or(json!(true))})
}
pub fn final_response(record: &ResponseRecord, mut native: Value, status: &str) -> Value {
    if !native.is_object() {
        native = public_response(record);
    }
    // Resource controls are platform-owned; other requested fields only fill
    // omissions in the native response, never overwrite provider extensions.
    for key in [
        "instructions",
        "metadata",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "temperature",
        "top_p",
        "max_output_tokens",
        "reasoning",
        "text",
        "truncation",
    ] {
        if native.get(key).is_none()
            && let Some(value) = record.request_json.get(key)
        {
            native[key] = value.clone();
        }
    }
    // Caller metadata belongs to this platform resource even when a stateless
    // compatibility backend returns its own default null metadata.
    if let Some(metadata) = record.request_json.get("metadata") {
        native["metadata"] = metadata.clone();
    }
    native["id"] = record.id.clone().into();
    native["object"] = "response".into();
    native["created_at"] = record.created_at.timestamp().into();
    native["status"] = status.into();
    native["model"] = record.model.clone().into();
    native["background"] = record.background.into();
    native["store"] = record.store_response.into();
    native["previous_response_id"] = json!(record.previous_id);
    native["conversation"] = record
        .conversation_id
        .as_ref()
        .map(|id| json!({"id":id}))
        .unwrap_or(Value::Null);
    native
}
fn error_response(record: &ResponseRecord, code: &str, message: &str) -> Value {
    let mut response = public_response(record);
    response["error"] = json!({"code":code,"message":message});
    response["status"] = "failed".into();
    response
}
pub async fn finish_and_event(
    pool: &DbRouter,
    record: &ResponseRecord,
    status: &str,
    native: Value,
    terminal: Option<&str>,
) -> Result<(ResponseRecord, Option<String>)> {
    if !matches!(status, "completed" | "incomplete" | "failed" | "cancelled") {
        return Err(ApiError::Internal(
            "Invalid managed-response terminal state".into(),
        ));
    }
    let scope = record.scope();
    let tx = owner_transaction(pool, scope).await?;
    let mut current = timed(
        ResponseRecord::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM scoped_responses WHERE id=$1 AND owner_id=$2 AND tenant_id=$3 AND user_id=$4 AND access_mode=$5 FOR UPDATE",
            [record.id.as_str().into(), record.owner_id.into(), scope.tenant.into(), scope.user.into(), scope.mode.as_str().into()],
        ))
        .one(&tx),
    )
    .await?
    .ok_or_else(missing)?;
    if !current.active() || current.deleted_at.is_some() {
        timed(tx.commit()).await?;
        return Ok((current, None));
    }
    let mut status = status.to_owned();
    let mut response = final_response(&current, native, &status);
    let output = response
        .get("output")
        .filter(|v| v.is_array())
        .cloned()
        .unwrap_or_else(|| json!([]));
    if serde_json::to_vec(&response)
        .map_err(|_| ApiError::Provider("Invalid stored response".into()))?
        .len()
        > MAX_RESPONSE_BYTES
    {
        status = "failed".into();
        response = error_response(
            &current,
            "response_limit",
            "Response exceeded the platform storage limit",
        );
    }
    if let Some(id) = &current.conversation_id {
        let conv = timed(
            ConversationRecord::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT * FROM scoped_conversations WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND access_mode=$4 FOR UPDATE",
                [id.into(), scope.tenant.into(), scope.user.into(), scope.mode.as_str().into()],
            ))
            .one(&tx),
        )
        .await?;
        if let Some(mut conv) = conv.filter(|c| {
            c.deleted_at.is_none() && c.active_response_id.as_deref() == Some(&current.id)
        }) {
            if matches!(status.as_str(), "completed" | "incomplete") {
                let mut additions = current
                    .new_input_json
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                additions.extend(output.as_array().into_iter().flatten().cloned());
                let mut all = item_bodies(&conv.items_json);
                all.extend(additions.clone());
                if check_history(&all).is_err() {
                    status = "failed".into();
                    response = error_response(
                        &current,
                        "context_limit",
                        "Conversation history limit reached; no items were appended",
                    );
                } else {
                    conv.items_json
                        .as_array_mut()
                        .ok_or_else(missing)?
                        .extend(wrap_items(additions));
                }
            }
            timed(tx.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE scoped_conversations SET items_json=$2,active_response_id=NULL,model=$3,
                 revision=revision+1,updated_at=NOW() WHERE id=$1 AND active_response_id=$4 AND tenant_id=$5 AND user_id=$6 AND access_mode=$7",
                [
                    id.into(),
                    conv.items_json.into(),
                    current.model.as_str().into(),
                    current.id.as_str().into(),
                    scope.tenant.into(),
                    scope.user.into(),
                    scope.mode.as_str().into(),
                ],
            )))
            .await?;
        } else {
            status = "cancelled".into();
            response = final_response(&current, public_response(&current), &status);
        }
    }
    response["status"] = status.clone().into();
    if serde_json::to_vec(&response)
        .map_err(|_| ApiError::Provider("Invalid stored response".into()))?
        .len()
        > MAX_RESPONSE_BYTES
    {
        status = "failed".into();
        response = error_response(
            &current,
            "response_limit",
            "Response exceeded the platform storage limit",
        );
    }
    let retain = current.retained();
    let ttl = if current.store_response {
        STORED_TTL
    } else {
        TEMP_TTL
    };
    let output = if matches!(status.as_str(), "completed" | "incomplete") {
        output
    } else {
        json!([])
    };
    let returned = response.clone();
    current=timed(ResponseRecord::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE scoped_responses SET status=$2,output_json=CASE WHEN $3 THEN $4 ELSE '[]'::jsonb END,
         response_json=CASE WHEN $3 THEN $5 ELSE NULL END,
         request_json=CASE WHEN $3 THEN request_json ELSE '{}'::jsonb END,
         input_json=CASE WHEN $3 THEN input_json ELSE '[]'::jsonb END,
         new_input_json=CASE WHEN $3 THEN new_input_json ELSE '[]'::jsonb END,
         execution_json=CASE WHEN $3 THEN execution_json ELSE NULL END,
         updated_at=NOW(),revision=revision+1,expires_at=NOW()+($6::BIGINT*INTERVAL '1 second')
         WHERE id=$1 AND owner_id=$7 AND tenant_id=$8 AND user_id=$9 AND access_mode=$10 RETURNING *",
        [current.id.as_str().into(),status.into(),retain.into(),output.into(),response.into(),ttl.into(),current.owner_id.into(),scope.tenant.into(),scope.user.into(),scope.mode.as_str().into()])).one(&tx)).await?.ok_or_else(missing)?;
    if !retain {
        current.response_json = Some(returned);
        timed(tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM scoped_response_events e USING scoped_responses r
             WHERE e.response_id=$1 AND r.id=e.response_id AND r.tenant_id=$2 AND r.user_id=$3 AND r.access_mode=$4",
            [current.id.as_str().into(), scope.tenant.into(), scope.user.into(), scope.mode.as_str().into()],
        )))
        .await?;
    }
    // The terminal event and public status become visible atomically.
    let frame = terminal_event(&tx, &mut current, terminal).await?;
    timed(tx.commit()).await?;
    Ok((current, frame))
}
pub async fn finish(
    pool: &DbRouter,
    record: &ResponseRecord,
    status: &str,
    native: Value,
) -> Result<ResponseRecord> {
    finish_and_event(pool, record, status, native, None)
        .await
        .map(|(record, _)| record)
}
pub async fn fail(
    pool: &DbRouter,
    record: &ResponseRecord,
    code: &str,
    message: &str,
) -> Result<ResponseRecord> {
    finish(
        pool,
        record,
        "failed",
        error_response(record, code, message),
    )
    .await
}
pub async fn cancel_or_delete(
    pool: &DbRouter,
    scope: Scope,
    id: &str,
    delete: bool,
) -> Result<ResponseRecord> {
    let tx = transaction(pool, scope).await?;
    let row = response(&tx, scope, id, delete).await?;
    let (updated, _) = cancel_or_delete_record(&tx, row, delete).await?;
    timed(tx.commit()).await?;
    Ok(updated)
}

/// Internal runner metadata lookup; not a public authorization path.
pub async fn owned(pool: &DbRouter, record: &ResponseRecord) -> Result<ResponseRecord> {
    let scope = record.scope();
    timed(
        ResponseRecord::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM scoped_responses WHERE id=$1 AND owner_id=$2 AND tenant_id=$3 AND user_id=$4 AND access_mode=$5",
            [record.id.as_str().into(), record.owner_id.into(), scope.tenant.into(), scope.user.into(), scope.mode.as_str().into()],
        ))
        .one(pool.write_conn()),
    )
    .await?
    .ok_or_else(missing)
}
pub async fn append_event(
    pool: &DbRouter,
    record: &ResponseRecord,
    terminal: bool,
    make: impl FnOnce(i64) -> Result<String>,
) -> Result<String> {
    let scope = record.scope();
    // This is an internal execution write, not a public read or a new
    // dispatch. The original durable execution lease may finish after a user,
    // membership or account grant is disabled; replay still authorizes each
    // batch separately. A superseded lease or deleted/cancelled row cannot write.
    let tx = owner_transaction(pool, scope).await?;
    let current = timed(ResponseRecord::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM scoped_responses WHERE tenant_id=$1 AND user_id=$2 AND access_mode=$3 AND id=$4 AND owner_id=$5 AND deleted_at IS NULL FOR UPDATE",
        [scope.tenant.into(), scope.user.into(), scope.mode.as_str().into(), record.id.as_str().into(), record.owner_id.into()],
    )).one(&tx)).await?.ok_or_else(missing)?;
    if !current.active() {
        return Err(conflict("Response event stream is closed"));
    }
    let frame = make(current.next_seq)?;
    let length = i64::try_from(frame.len()).map_err(|_| conflict("Response event is too large"))?;
    if frame.len() > 512 * 1024
        || current.next_seq >= MAX_STREAM_EVENTS - i64::from(!terminal)
        || current.event_bytes + length
            > MAX_RESPONSE_BYTES as i64 - if terminal { 0 } else { 512 * 1024 }
    {
        return Err(ApiError::Provider(
            "Managed response event storage limit reached".into(),
        ));
    }
    let inserted = timed(tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO scoped_response_events(response_id,seq,frame)
         SELECT $1,$2,$3 WHERE EXISTS (
             SELECT 1 FROM scoped_responses
             WHERE id=$1 AND tenant_id=$4 AND user_id=$5 AND access_mode=$6 AND owner_id=$7
         )",
        [
            record.id.as_str().into(),
            current.next_seq.into(),
            frame.clone().into(),
            scope.tenant.into(),
            scope.user.into(),
            scope.mode.as_str().into(),
            record.owner_id.into(),
        ],
    )))
    .await?;
    if inserted.rows_affected() != 1 {
        return Err(conflict("Response event ownership changed"));
    }
    let updated = timed(tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE scoped_responses SET next_seq=next_seq+1,event_bytes=event_bytes+$2,updated_at=NOW() WHERE id=$1 AND owner_id=$3 AND tenant_id=$4 AND user_id=$5 AND access_mode=$6",
        [record.id.as_str().into(),length.into(),record.owner_id.into(),scope.tenant.into(),scope.user.into(),scope.mode.as_str().into()]))).await?;
    if updated.rows_affected() != 1 {
        return Err(conflict("Response event ownership changed"));
    }
    timed(tx.commit()).await?;
    Ok(frame)
}
#[derive(FromQueryResult)]
pub struct StreamStatus {
    pub account_id: Option<Uuid>,
    pub status: String,
    pub background: bool,
    pub store_response: bool,
    pub stream: bool,
    pub next_seq: i64,
}
impl StreamStatus {
    pub fn active(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "in_progress")
    }
}
pub(super) async fn read_events(
    db: &impl ConnectionTrait,
    scope: Scope,
    id: &str,
    after: i64,
    authority: super::access::ReplayAuthority,
) -> Result<(StreamStatus, Vec<StoredEvent>)> {
    let proof = authority.values(scope)?;
    let values: Vec<sea_orm::Value> = scope
        .values()
        .into_iter()
        .chain([id.into()])
        .chain(proof)
        .collect();
    let predicate = super::access::CURRENT_REPLAY_ACTOR;
    let record=timed(StreamStatus::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        format!("SELECT r.account_id,r.status,r.background,r.store_response,r.stream,r.next_seq FROM scoped_responses r WHERE r.tenant_id=$1 AND r.user_id=$2 AND r.access_mode=$3 AND r.id=$4 AND r.deleted_at IS NULL AND r.expires_at>statement_timestamp() AND {predicate}"),
        values.clone())).one(db)).await?.ok_or_else(missing)?;
    check_account(db, scope, record.account_id).await?;
    if !(record.background || record.store_response) || !record.stream {
        return Err(missing());
    }
    if after < -1 || after >= record.next_seq && after != -1 {
        return Err(ApiError::BadRequest(
            "starting_after is not a valid event cursor".into(),
        ));
    }
    let events=timed(StoredEvent::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        format!("SELECT e.seq,e.frame FROM scoped_response_events e JOIN scoped_responses r ON r.id=e.response_id WHERE r.tenant_id=$1 AND r.user_id=$2 AND r.access_mode=$3 AND r.id=$4 AND r.deleted_at IS NULL AND r.expires_at>statement_timestamp() AND e.seq>$10 AND {predicate} ORDER BY e.seq LIMIT 4"),
        values.into_iter().chain([after.into()]))).all(db)).await?;
    Ok((record, events))
}
#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct ListQuery {
    pub after: Option<String>,
    pub limit: Option<usize>,
    pub order: Option<String>,
}
pub fn list_items(items: &[Value], query: &ListQuery) -> Result<Value> {
    let limit = query.limit.unwrap_or(20);
    if !(1..=100).contains(&limit) {
        return Err(ApiError::BadRequest(
            "limit must be between 1 and 100".into(),
        ));
    }
    let descending = match query.order.as_deref() {
        None | Some("desc") => true,
        Some("asc") => false,
        _ => return Err(ApiError::BadRequest("order must be asc or desc".into())),
    };
    let mut all = items.to_vec();
    if descending {
        all.reverse();
    }
    let start = match &query.after {
        None => 0,
        Some(cursor) => all
            .iter()
            .position(|v| v["id"].as_str() == Some(cursor))
            .map(|n| n + 1)
            .ok_or_else(|| ApiError::BadRequest("after is not an item in this resource".into()))?,
    };
    let data: Vec<_> = all
        .iter()
        .skip(start)
        .take(limit)
        .map(|v| {
            let mut body = v["body"].clone();
            body["id"] = v["id"].clone();
            body
        })
        .collect();
    Ok(
        json!({"object":"list","first_id":data.first().and_then(|v|v.get("id")),"last_id":data.last().and_then(|v|v.get("id")),"has_more":start+limit<all.len(),"data":data}),
    )
}
pub fn response_input_items(record: &ResponseRecord) -> Vec<Value> {
    record
        .input_json
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, body)| {
            let id = format!("{}_input_{i}", record.id);
            json!({"id":id,"body":body})
        })
        .collect()
}
pub fn conversation_view(row: &ConversationRecord) -> Value {
    json!({"id":row.id,"object":"conversation","created_at":row.created_at.timestamp(),"metadata":row.metadata_json})
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct ResponseAdminSummary {
    pub id: String,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub access_mode: String,
    pub account_id: Option<Uuid>,
    pub model: String,
    pub status: String,
    pub background: bool,
    pub store_response: bool,
    pub stream: bool,
    pub previous_id: Option<String>,
    pub conversation_id: Option<String>,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct ConversationAdminSummary {
    pub id: String,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub access_mode: String,
    pub account_id: Option<Uuid>,
    pub model: Option<String>,
    pub metadata_json: Value,
    pub active_response_id: Option<String>,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

fn admin_values(
    tenant: Uuid,
    owner: Option<Uuid>,
    mode: ModelAccessMode,
    limit: i64,
    offset: i64,
) -> Vec<sea_orm::Value> {
    vec![
        tenant.into(),
        owner.into(),
        mode.as_str().into(),
        limit.into(),
        offset.into(),
    ]
}

async fn admin_read_transaction(pool: &DbRouter) -> Result<DatabaseTransaction> {
    let tx = timed(pool.begin()).await?;
    timed(tx.execute_unprepared(
        "SET LOCAL statement_timeout='2500ms'; SET LOCAL lock_timeout='1000ms'",
    ))
    .await?;
    Ok(tx)
}

pub(crate) async fn append_control_audit(
    tx: &DatabaseTransaction,
    control: &ResponseControlScope,
    audit: &AuditContext,
    action: &str,
    resource_type: &str,
    resource_id: Option<&str>,
    mut metadata: Value,
) -> Result<()> {
    control.check_expiry()?;
    let (scope, tenant) = if let Some(reason) = control.root_reason() {
        if let Some(object) = metadata.as_object_mut() {
            object.insert("reason".into(), Value::String(reason.to_owned()));
            object.insert(
                "tenant_id".into(),
                Value::String(control.tenant_id().to_string()),
            );
        }
        (AuditScopeType::Platform, None)
    } else {
        (AuditScopeType::Tenant, Some(control.tenant_id()))
    };
    TenantAuditEvent::append(
        tx,
        scope,
        tenant,
        audit,
        action,
        resource_type,
        resource_id,
        AuditResult::Success,
        metadata,
    )
    .await
    .map_err(ApiError::from)?;
    Ok(())
}

pub async fn admin_list_responses(
    pool: &DbRouter,
    control: &ResponseControlScope,
    owner: Option<Uuid>,
    mode: ModelAccessMode,
    limit: i64,
    offset: i64,
) -> Result<Vec<ResponseAdminSummary>> {
    if !(1..=100).contains(&limit)
        || !(0..=100_000_000).contains(&offset)
        || owner.is_some_and(|id| id.is_nil())
        || mode == ModelAccessMode::AccountPool
    {
        return Err(ApiError::BadRequest("Invalid local resource page".into()));
    }
    let tx = admin_read_transaction(pool).await?;
    let audit = control.revalidate_for_admin(&tx).await?;
    let rows = timed(ResponseAdminSummary::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id,tenant_id,user_id,access_mode,account_id,model,status,background,store_response,stream,previous_id,conversation_id,revision,created_at,updated_at,expires_at,deleted_at \
         FROM scoped_responses \
         WHERE tenant_id=$1 AND ($2::uuid IS NULL OR user_id=$2) AND access_mode=$3 AND (background OR store_response) AND deleted_at IS NULL AND expires_at>NOW() \
         ORDER BY created_at DESC,id DESC LIMIT $4 OFFSET $5",
        admin_values(control.tenant_id(), owner, mode, limit, offset),
    ))
    .all(&tx))
    .await?;
    append_control_audit(
        &tx,
        control,
        &audit,
        "response.list",
        "response",
        None,
        json!({"owner_user_id":owner,"access_mode":mode.as_str(),"count":rows.len()}),
    )
    .await?;
    timed(tx.commit()).await?;
    Ok(rows)
}

pub async fn admin_count_responses(
    pool: &DbRouter,
    control: &ResponseControlScope,
    owner: Option<Uuid>,
    mode: ModelAccessMode,
) -> Result<i64> {
    if owner.is_some_and(|id| id.is_nil()) || mode == ModelAccessMode::AccountPool {
        return Err(ApiError::BadRequest(
            "Invalid local resource selector".into(),
        ));
    }
    let tx = admin_read_transaction(pool).await?;
    let audit = control.revalidate_for_admin(&tx).await?;
    let row = timed(tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT AS n FROM scoped_responses WHERE tenant_id=$1 AND ($2::uuid IS NULL OR user_id=$2) AND access_mode=$3 AND (background OR store_response) AND deleted_at IS NULL AND expires_at>NOW()",
        [control.tenant_id().into(), owner.into(), mode.as_str().into()],
    )))
    .await?
    .ok_or_else(missing)?;
    let total = row.try_get::<i64>("", "n").map_err(storage)?;
    append_control_audit(
        &tx,
        control,
        &audit,
        "response.count",
        "response",
        None,
        json!({"owner_user_id":owner,"access_mode":mode.as_str(),"count":total}),
    )
    .await?;
    timed(tx.commit()).await?;
    Ok(total)
}

fn validate_local_resource(owner: Uuid, mode: ModelAccessMode, id: &str) -> Result<()> {
    if owner.is_nil()
        || mode == ModelAccessMode::AccountPool
        || id.is_empty()
        || id.len() > 200
        || id.chars().any(char::is_control)
    {
        return Err(ApiError::BadRequest(
            "Invalid local response resource selector".into(),
        ));
    }
    Ok(())
}

async fn admin_response_record(
    db: &impl ConnectionTrait,
    tenant: Uuid,
    owner: Uuid,
    mode: ModelAccessMode,
    id: &str,
    deleted: bool,
) -> Result<ResponseRecord> {
    timed(ResponseRecord::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM scoped_responses WHERE tenant_id=$1 AND user_id=$2 AND access_mode=$3 AND id=$4 AND (background OR store_response) AND expires_at>NOW() AND ($5 OR deleted_at IS NULL) FOR UPDATE",
        [tenant.into(), owner.into(), mode.as_str().into(), id.into(), deleted.into()],
    ))
    .one(db))
    .await?
    .ok_or_else(missing)
}

pub async fn admin_response(
    pool: &DbRouter,
    control: &ResponseControlScope,
    owner: Uuid,
    mode: ModelAccessMode,
    id: &str,
    deleted: bool,
) -> Result<ResponseRecord> {
    validate_local_resource(owner, mode, id)?;
    let owner_scope = Scope {
        tenant: control.tenant_id(),
        user: owner,
        mode,
    };
    let tx = owner_transaction(pool, owner_scope).await?;
    let audit = control.revalidate_for_admin(&tx).await?;
    let row = admin_response_record(&tx, control.tenant_id(), owner, mode, id, deleted).await?;
    append_control_audit(
        &tx,
        control,
        &audit,
        "response.detail",
        "response",
        Some(id),
        json!({"owner_user_id":owner,"access_mode":mode.as_str(),"revision":row.revision}),
    )
    .await?;
    timed(tx.commit()).await?;
    Ok(row)
}

pub async fn admin_list_conversations(
    pool: &DbRouter,
    control: &ResponseControlScope,
    owner: Option<Uuid>,
    mode: ModelAccessMode,
    limit: i64,
    offset: i64,
) -> Result<Vec<ConversationAdminSummary>> {
    if !(1..=100).contains(&limit)
        || !(0..=100_000_000).contains(&offset)
        || owner.is_some_and(|id| id.is_nil())
        || mode == ModelAccessMode::AccountPool
    {
        return Err(ApiError::BadRequest("Invalid local resource page".into()));
    }
    let tx = admin_read_transaction(pool).await?;
    let audit = control.revalidate_for_admin(&tx).await?;
    let rows = timed(ConversationAdminSummary::find_by_statement(
        Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id,tenant_id,user_id,access_mode,account_id,model,metadata_json,active_response_id,revision,created_at,updated_at,expires_at,deleted_at \
         FROM scoped_conversations \
         WHERE tenant_id=$1 AND ($2::uuid IS NULL OR user_id=$2) AND access_mode=$3 AND deleted_at IS NULL AND expires_at>NOW() \
         ORDER BY created_at DESC,id DESC LIMIT $4 OFFSET $5",
        admin_values(control.tenant_id(), owner, mode, limit, offset),
    ))
    .all(&tx))
    .await?;
    append_control_audit(
        &tx,
        control,
        &audit,
        "conversation.list",
        "conversation",
        None,
        json!({"owner_user_id":owner,"access_mode":mode.as_str(),"count":rows.len()}),
    )
    .await?;
    timed(tx.commit()).await?;
    Ok(rows)
}

pub async fn admin_count_conversations(
    pool: &DbRouter,
    control: &ResponseControlScope,
    owner: Option<Uuid>,
    mode: ModelAccessMode,
) -> Result<i64> {
    if owner.is_some_and(|id| id.is_nil()) || mode == ModelAccessMode::AccountPool {
        return Err(ApiError::BadRequest(
            "Invalid local resource selector".into(),
        ));
    }
    let tx = admin_read_transaction(pool).await?;
    let audit = control.revalidate_for_admin(&tx).await?;
    let row = timed(tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT AS n FROM scoped_conversations WHERE tenant_id=$1 AND ($2::uuid IS NULL OR user_id=$2) AND access_mode=$3 AND deleted_at IS NULL AND expires_at>NOW()",
        [control.tenant_id().into(), owner.into(), mode.as_str().into()],
    )))
    .await?
    .ok_or_else(missing)?;
    let total = row.try_get::<i64>("", "n").map_err(storage)?;
    append_control_audit(
        &tx,
        control,
        &audit,
        "conversation.count",
        "conversation",
        None,
        json!({"owner_user_id":owner,"access_mode":mode.as_str(),"count":total}),
    )
    .await?;
    timed(tx.commit()).await?;
    Ok(total)
}

async fn admin_conversation_record(
    db: &impl ConnectionTrait,
    tenant: Uuid,
    owner: Uuid,
    mode: ModelAccessMode,
    id: &str,
    deleted: bool,
) -> Result<ConversationRecord> {
    timed(ConversationRecord::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM scoped_conversations WHERE tenant_id=$1 AND user_id=$2 AND access_mode=$3 AND id=$4 AND expires_at>NOW() AND ($5 OR deleted_at IS NULL) FOR UPDATE",
        [tenant.into(), owner.into(), mode.as_str().into(), id.into(), deleted.into()],
    ))
    .one(db))
    .await?
    .ok_or_else(missing)
}

pub async fn admin_conversation(
    pool: &DbRouter,
    control: &ResponseControlScope,
    owner: Uuid,
    mode: ModelAccessMode,
    id: &str,
    deleted: bool,
) -> Result<ConversationRecord> {
    validate_local_resource(owner, mode, id)?;
    let owner_scope = Scope {
        tenant: control.tenant_id(),
        user: owner,
        mode,
    };
    let tx = owner_transaction(pool, owner_scope).await?;
    let audit = control.revalidate_for_admin(&tx).await?;
    let row = admin_conversation_record(&tx, control.tenant_id(), owner, mode, id, deleted).await?;
    append_control_audit(
        &tx,
        control,
        &audit,
        "conversation.detail",
        "conversation",
        Some(id),
        json!({"owner_user_id":owner,"access_mode":mode.as_str(),"revision":row.revision}),
    )
    .await?;
    timed(tx.commit()).await?;
    Ok(row)
}

async fn cancel_or_delete_record(
    tx: &DatabaseTransaction,
    mut row: ResponseRecord,
    delete: bool,
) -> Result<(ResponseRecord, bool)> {
    let scope = row.scope();
    let id = row.id.clone();
    let expected_revision = row.revision;
    let was_active = row.active();
    if row.deleted_at.is_some() {
        return Ok((row, false));
    }
    if !row.retained() {
        return Err(missing());
    }
    if !delete && !row.background {
        return Err(ApiError::BadRequest(
            "Only background Responses can be cancelled through this endpoint".into(),
        ));
    }
    let transitioned = row.active();
    if !delete && !transitioned {
        return Ok((row, false));
    }
    if transitioned {
        row.status = "cancelled".into();
        row.response_json = Some(final_response(&row, public_response(&row), "cancelled"));
    }
    release_conversation(tx, &row).await?;
    row=timed(ResponseRecord::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE scoped_responses SET status=$2,response_json=CASE WHEN $3 THEN NULL ELSE $4 END,
         deleted_at=CASE WHEN $3 THEN NOW() ELSE deleted_at END,
         request_json=CASE WHEN $3 THEN '{}'::jsonb ELSE request_json END,
         input_json=CASE WHEN $3 THEN '[]'::jsonb ELSE input_json END,
         new_input_json=CASE WHEN $3 THEN '[]'::jsonb ELSE new_input_json END,
         output_json=CASE WHEN $3 THEN '[]'::jsonb ELSE output_json END,
         execution_json=CASE WHEN $3 THEN execution_json-'native_result' ELSE execution_json END,
         updated_at=NOW(),revision=revision+1 WHERE id=$1 AND tenant_id=$5 AND user_id=$6 AND access_mode=$7 AND revision=$8 RETURNING *",
        [id.as_str().into(),row.status.into(),delete.into(),row.response_json.into(),scope.tenant.into(),scope.user.into(),scope.mode.as_str().into(),expected_revision.into()])).one(tx)).await?.ok_or_else(||conflict("Response revision changed; reload before retrying"))?;
    if delete {
        timed(tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM scoped_response_events e USING scoped_responses r
             WHERE e.response_id=$1 AND r.id=e.response_id AND r.tenant_id=$2 AND r.user_id=$3 AND r.access_mode=$4",
            [id.as_str().into(), scope.tenant.into(), scope.user.into(), scope.mode.as_str().into()],
        )))
        .await?;
    }
    if !delete && transitioned {
        let _ = terminal_event(tx, &mut row, None).await?;
    }
    Ok((row, was_active))
}

pub async fn admin_cancel_or_delete_response(
    pool: &DbRouter,
    control: &ResponseControlScope,
    owner: Uuid,
    mode: ModelAccessMode,
    id: &str,
    expected_revision: i64,
    delete: bool,
) -> Result<(ResponseRecord, bool)> {
    validate_local_resource(owner, mode, id)?;
    let scope = Scope {
        tenant: control.tenant_id(),
        user: owner,
        mode,
    };
    let tx = owner_transaction(pool, scope).await?;
    let audit = control.revalidate_for_admin(&tx).await?;
    let row = admin_response_record(&tx, scope.tenant, owner, mode, id, delete).await?;
    if expected_revision != row.revision {
        return Err(conflict(
            "Response revision changed; reload before retrying",
        ));
    }
    let (updated, was_active) = cancel_or_delete_record(&tx, row, delete).await?;
    append_control_audit(&tx,control,&audit,if delete {"response.delete"} else {"response.cancel"},"response",Some(id),
        json!({"owner_user_id":owner,"revision":updated.revision,"status":updated.status,"operation":if delete {"delete"} else {"cancel"},"changed":updated.revision!=expected_revision})).await?;
    control.check_expiry()?;
    timed(tx.commit()).await?;
    Ok((updated, was_active))
}

async fn mutate_conversation_record(
    tx: &DatabaseTransaction,
    mut row: ConversationRecord,
    change: ConversationMutation,
) -> Result<(ConversationRecord, Option<ResponseRecord>)> {
    let scope = Scope {
        tenant: row.tenant_id,
        user: row.user_id,
        mode: match row.access_mode.as_str() {
            "passthrough" => ModelAccessMode::Passthrough,
            "node_dispatch" => ModelAccessMode::NodeDispatch,
            _ => return Err(missing()),
        },
    };
    let id = row.id.clone();
    let expected_revision = row.revision;
    if row.deleted_at.is_some() {
        return Ok((row, None));
    }
    if row.active_response_id.is_some() && !matches!(change, ConversationMutation::Delete) {
        return Err(conflict(
            "Conversation has an active response; retry after it finishes",
        ));
    }
    let deleting = matches!(change, ConversationMutation::Delete);
    let mut active_cancelled = None;
    match change {
        ConversationMutation::Metadata(value) => row.metadata_json = metadata(Some(&value))?,
        ConversationMutation::Append(items) => {
            if items.len() > 20 {
                return Err(ApiError::BadRequest("Add at most 20 items".into()));
            }
            row.items_json
                .as_array_mut()
                .ok_or_else(missing)?
                .extend(wrap_items(items));
            check_history(&item_bodies(&row.items_json))?;
        }
        ConversationMutation::RemoveItem(item_id) => row
            .items_json
            .as_array_mut()
            .ok_or_else(missing)?
            .retain(|item| item["id"] != item_id),
        ConversationMutation::Delete => {
            if let Some(active) = &row.active_response_id {
                let active_row=timed(ResponseRecord::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
                    "SELECT * FROM scoped_responses WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND access_mode=$4 AND status IN ('queued','in_progress') FOR UPDATE",
                    [active.into(),scope.tenant.into(),scope.user.into(),scope.mode.as_str().into()])).one(tx)).await?;
                if let Some(mut active_row) = active_row {
                    let public =
                        final_response(&active_row, public_response(&active_row), "cancelled");
                    active_row=timed(ResponseRecord::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
                        "UPDATE scoped_responses SET status='cancelled',response_json=$5,updated_at=NOW(),revision=revision+1 WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND access_mode=$4 RETURNING *",
                        [active.into(),scope.tenant.into(),scope.user.into(),scope.mode.as_str().into(),public.into()])).one(tx)).await?.ok_or_else(missing)?;
                    let _ = terminal_event(tx, &mut active_row, None).await?;
                    active_cancelled = Some(active_row);
                }
            }
            row.items_json = json!([]);
            row.metadata_json = json!({});
        }
    }
    let updated=timed(ConversationRecord::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE scoped_conversations SET metadata_json=$5,items_json=$6,deleted_at=CASE WHEN $7 THEN NOW() ELSE deleted_at END,active_response_id=CASE WHEN $7 THEN NULL ELSE active_response_id END,revision=revision+1,updated_at=NOW(),expires_at=NOW()+($8::BIGINT*INTERVAL '1 second') WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND access_mode=$4 AND revision=$9 RETURNING *",
        [id.into(),scope.tenant.into(),scope.user.into(),scope.mode.as_str().into(),row.metadata_json.into(),row.items_json.into(),deleting.into(),STORED_TTL.into(),expected_revision.into()])).one(tx)).await?.ok_or_else(||conflict("Conversation revision changed; reload before retrying"))?;
    Ok((updated, active_cancelled))
}

pub async fn admin_mutate_conversation(
    pool: &DbRouter,
    control: &ResponseControlScope,
    owner: Uuid,
    mode: ModelAccessMode,
    id: &str,
    expected_revision: i64,
    change: ConversationMutation,
) -> Result<(ConversationRecord, Option<ResponseRecord>)> {
    validate_local_resource(owner, mode, id)?;
    let scope = Scope {
        tenant: control.tenant_id(),
        user: owner,
        mode,
    };
    let tx = owner_transaction(pool, scope).await?;
    let audit = control.revalidate_for_admin(&tx).await?;
    let row = admin_conversation_record(
        &tx,
        scope.tenant,
        owner,
        mode,
        id,
        matches!(change, ConversationMutation::Delete),
    )
    .await?;
    if expected_revision != row.revision {
        return Err(conflict(
            "Conversation revision changed; reload before retrying",
        ));
    }
    let operation = match &change {
        ConversationMutation::Metadata(_) => "metadata",
        ConversationMutation::Append(_) => "append",
        ConversationMutation::RemoveItem(_) => "remove_item",
        ConversationMutation::Delete => "delete",
    };
    let (updated, cancelled) = mutate_conversation_record(&tx, row, change).await?;
    append_control_audit(&tx,control,&audit,"conversation.mutate","conversation",Some(id),
        json!({"owner_user_id":owner,"revision":updated.revision,"operation":operation,"changed":updated.revision!=expected_revision})).await?;
    control.check_expiry()?;
    timed(tx.commit()).await?;
    Ok((updated, cancelled))
}

pub async fn cleanup(pool: &DbRouter) -> Result<()> {
    timed(pool.write_conn().execute_unprepared(
        "DELETE FROM scoped_responses WHERE id IN
         (SELECT id FROM scoped_responses WHERE expires_at<=NOW()
          AND status NOT IN ('queued','in_progress') AND COALESCE(execution_json->>'accounting_pending','false')<>'true' ORDER BY expires_at LIMIT 64)",
    ))
    .await?;
    timed(pool.write_conn().execute_unprepared(
        "DELETE FROM scoped_conversations WHERE id IN
         (SELECT id FROM scoped_conversations WHERE expires_at<=NOW()
          AND active_response_id IS NULL ORDER BY expires_at LIMIT 64)",
    ))
    .await?;
    Ok(())
}
impl std::fmt::Debug for ResponseRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseRecord")
            .field("id", &self.id)
            .field("status", &self.status)
            .field("revision", &self.revision)
            .field("request_hash", &self.request_hash)
            .field("has_idempotency_key", &self.idempotency_hash.is_some())
            .field("heartbeat_at", &self.heartbeat_at)
            .field("updated_at", &self.updated_at)
            .field("body", &"[REDACTED]")
            .finish()
    }
}
impl std::fmt::Debug for ConversationRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConversationRecord")
            .field("id", &self.id)
            .field("revision", &self.revision)
            .field("tenant_id", &self.tenant_id)
            .field("user_id", &self.user_id)
            .field("access_mode", &self.access_mode)
            .field("updated_at", &self.updated_at)
            .field("expires_at", &self.expires_at)
            .field("items", &"[REDACTED]")
            .finish()
    }
}
async fn terminal_event(
    db: &impl ConnectionTrait,
    row: &mut ResponseRecord,
    raw: Option<&str>,
) -> Result<Option<String>> {
    if !row.stream || row.deleted_at.is_some() {
        return Ok(None);
    }
    let body = public_response(row);
    let seq = row.next_seq;
    let frame = if row.status == "cancelled" {
        let event = json!({"type":"error","sequence_number":seq,
            "code":"response_cancelled","message":"Response was cancelled",
            "response_id":row.id});
        format!("event: error\ndata: {event}\n\n")
    } else if let Some(raw) = raw {
        super::execution::rewrite_frame(raw, &row.id, seq, Some(&body), false)?
    } else {
        let kind = match row.status.as_str() {
            "completed" => "response.completed",
            "incomplete" => "response.incomplete",
            _ => "response.failed",
        };
        let event = json!({"type":kind,"sequence_number":seq,"response":body});
        format!("event: {kind}\ndata: {event}\n\n")
    };
    if !row.retained() {
        return Ok(Some(frame));
    }
    let bytes = frame.len() as i64;
    if frame.len() > 512 * 1024
        || row.next_seq >= MAX_STREAM_EVENTS
        || row.event_bytes + bytes > MAX_RESPONSE_BYTES as i64
    {
        return Err(ApiError::Provider(
            "Managed terminal event exceeds its reserved storage budget".into(),
        ));
    }
    let scope = row.scope();
    let inserted = timed(db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO scoped_response_events(response_id,seq,frame)
         SELECT $1,$2,$3 WHERE EXISTS (
             SELECT 1 FROM scoped_responses
             WHERE id=$1 AND owner_id=$4 AND tenant_id=$5 AND user_id=$6 AND access_mode=$7
         )",
        [
            row.id.as_str().into(),
            seq.into(),
            frame.clone().into(),
            row.owner_id.into(),
            scope.tenant.into(),
            scope.user.into(),
            scope.mode.as_str().into(),
        ],
    )))
    .await?;
    if inserted.rows_affected() != 1 {
        return Err(conflict("Response event ownership changed"));
    }
    let updated = timed(db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE scoped_responses SET next_seq=next_seq+1,event_bytes=event_bytes+$2 WHERE id=$1 AND owner_id=$3 AND tenant_id=$4 AND user_id=$5 AND access_mode=$6",
        [row.id.as_str().into(), bytes.into(), row.owner_id.into(), scope.tenant.into(), scope.user.into(), scope.mode.as_str().into()],
    )))
    .await?;
    if updated.rows_affected() != 1 {
        return Err(conflict("Response event ownership changed"));
    }
    row.next_seq += 1;
    row.event_bytes += bytes;
    Ok(Some(frame))
}

/// Accounting completion is separate from resource delivery/cancellation.
pub async fn accounting_secured(pool: &DbRouter, record: &ResponseRecord) -> Result<()> {
    let scope = record.scope();
    let result=timed(pool.write_conn().execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE scoped_responses SET execution_json=COALESCE(execution_json,'{}'::jsonb)||'{\"accounting_pending\":false}'::jsonb WHERE id=$1 AND owner_id=$2 AND tenant_id=$3 AND user_id=$4 AND access_mode=$5",
        [record.id.as_str().into(),record.owner_id.into(),scope.tenant.into(),scope.user.into(),scope.mode.as_str().into()]))).await?;
    if result.rows_affected() != 1 {
        return Err(conflict(
            "Execution ownership changed during reconciliation",
        ));
    }
    Ok(())
}

/// Read a bounded batch of stale owners, including cancelled resources whose
/// detached accounting did not finish before the process was interrupted.
pub async fn recovery_candidates(pool: &DbRouter) -> Result<Vec<ResponseRecord>> {
    timed(ResponseRecord::find_by_statement(Statement::from_string(DbBackend::Postgres,
        "SELECT * FROM scoped_responses WHERE heartbeat_at<NOW()-INTERVAL '60 seconds' AND (status IN ('queued','in_progress') OR execution_json->>'accounting_pending'='true') ORDER BY heartbeat_at LIMIT 8".to_string()))
        .all(pool.write_conn())).await
}

/// Rotate ownership only if the original runner is still stale at the writer.
/// A late old process may reconcile the same billing request, but can no longer
/// publish resource data or append conversation history.
pub async fn claim_recovery(
    pool: &DbRouter,
    record: &ResponseRecord,
) -> Result<Option<ResponseRecord>> {
    let tx = owner_transaction(pool, record.scope()).await?;
    let next=timed(ResponseRecord::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE scoped_responses SET owner_id=$6,heartbeat_at=NOW(),revision=revision+1 WHERE id=$1 AND owner_id=$2 AND tenant_id=$3 AND user_id=$4 AND access_mode=$5 AND heartbeat_at<NOW()-INTERVAL '60 seconds' AND (status IN ('queued','in_progress') OR execution_json->>'accounting_pending'='true') RETURNING *",
        [record.id.as_str().into(),record.owner_id.into(),record.tenant_id.into(),record.user_id.into(),record.access_mode.as_str().into(),Uuid::new_v4().into()])).one(&tx)).await?;
    timed(tx.commit()).await?;
    Ok(next)
}

#[cfg(test)]
mod response_control_scope_tests {
    use super::*;
    fn audit(actor: Uuid) -> AuditContext {
        AuditContext {
            actor_user_id: actor,
            credential_kind: CredentialKind::Jwt,
            actor_platform_role: PlatformRole::None,
            actor_tenant_role: Some(TenantRole::Admin),
            request_id: Some(Uuid::new_v4()),
        }
    }
    #[test]
    fn constructor_checks_credential_actor_and_request_identity() {
        let tenant = Uuid::new_v4();
        let actor = Uuid::new_v4();
        let scope = TenantScope::checked(tenant, actor, TenantRole::Admin).unwrap();
        let a = audit(actor);
        let expiry = Utc::now().timestamp() + 60;
        assert!(ResponseControlScope::tenant_admin(scope, a, 0, 1, 1, expiry).is_ok());
        for kind in [
            CredentialKind::ApiKey,
            CredentialKind::Node,
            CredentialKind::System,
        ] {
            assert!(
                ResponseControlScope::tenant_admin(
                    scope,
                    AuditContext {
                        credential_kind: kind,
                        ..a
                    },
                    0,
                    1,
                    1,
                    expiry
                )
                .is_err()
            );
        }
        for changed in [
            AuditContext {
                actor_user_id: Uuid::new_v4(),
                ..a
            },
            AuditContext {
                request_id: None,
                ..a
            },
            AuditContext {
                request_id: Some(Uuid::nil()),
                ..a
            },
        ] {
            assert!(ResponseControlScope::tenant_admin(scope, changed, 0, 1, 1, expiry).is_err());
        }
        let member = TenantScope::checked(tenant, actor, TenantRole::Member).unwrap();
        assert!(ResponseControlScope::tenant_admin(member, a, 0, 1, 1, expiry).is_err());
        let old = ResponseControlScope::tenant_admin(scope, a, 0, 1, 1, 1).unwrap();
        assert!(old.check_expiry().is_err());
    }
    #[test]
    fn root_support_requires_explicit_target_reason_and_valid_origin_snapshot() {
        let actor = Uuid::new_v4();
        let target = Uuid::new_v4();
        let root = PlatformScope::checked(actor, PlatformRole::Root).unwrap();
        let audit = audit(actor);
        let session = ResponseControlSession {
            token_version: 0,
            jwt_expires_at: Utc::now().timestamp() + 60,
            selected: None,
        };
        assert!(ResponseControlScope::root(root, target, audit, session, "incident").is_ok());
        for reason in ["", "\t", "incident\nprivate"] {
            assert!(ResponseControlScope::root(root, target, audit, session, reason).is_err());
        }
        let operator = PlatformScope::checked(actor, PlatformRole::Operator).unwrap();
        assert!(ResponseControlScope::root(operator, target, audit, session, "incident").is_err());
        assert!(ResponseControlScope::root(root, Uuid::nil(), audit, session, "incident").is_err());
        let invalid = ResponseControlSession {
            selected: Some(ResponseControlMembership {
                tenant_id: target,
                tenant_role: TenantRole::Member,
                tenant_authz_version: 0,
                membership_authz_version: 1,
            }),
            ..session
        };
        assert!(ResponseControlScope::root(root, target, audit, invalid, "incident").is_err());
    }
}
