//! Console node control: explicit tenant/owner scope and primary live authority.
//! These capabilities never authorize a worker lease or expose session secrets.
use super::tenant_audit_event::lock_identity_admin;
use super::tenant_control::{self, TenantAuthzSnapshot};
use crate::{AuditContext, DbError, Tenant, TenantAuditEvent, User};
use chrono::{DateTime, Utc};
use keycompute_types::{
    AuditResult, AuditScopeType, CredentialKind, PlatformRole, PlatformScope, TenantRole,
    TenantScope,
};
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement, TransactionTrait,
    Value,
};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

#[path = "node_task_control.rs"]
mod task_control;
pub use task_control::{TaskAction, TaskChange, TaskMutation, change_task};

#[derive(Debug, Clone, Copy)]
enum Authority {
    Tenant(TenantScope, TenantAuthzSnapshot),
    Owned(TenantScope, TenantAuthzSnapshot),
    Platform(PlatformScope, i32),
}
#[derive(Debug, Clone, Copy)]
pub struct NodeControlScope {
    tenant: Uuid,
    authority: Authority,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeAction {
    Configure,
    Exclude,
    Recover,
    Revoke,
    Delete,
    ApproveToken,
    RejectToken,
    RevokeToken,
    CancelTask,
    ArchiveTask,
}
fn denied() -> DbError {
    DbError::Other("node control authorization denied".into())
}
fn invalid(message: &str) -> DbError {
    DbError::Other(format!("invalid node control input: {message}"))
}
fn conflict(kind: &str, id: Uuid) -> DbError {
    DbError::OptimisticConflict {
        entity: kind.into(),
        id: id.to_string(),
    }
}
impl NodeControlScope {
    pub fn tenant(
        scope: TenantScope,
        snapshot: TenantAuthzSnapshot,
        credential: CredentialKind,
    ) -> Result<Self, DbError> {
        if scope.tenant_role() != TenantRole::Admin {
            return Err(denied());
        }
        Self::selected(scope, snapshot, credential, false)
    }
    pub fn owned(
        scope: TenantScope,
        snapshot: TenantAuthzSnapshot,
        credential: CredentialKind,
    ) -> Result<Self, DbError> {
        Self::selected(scope, snapshot, credential, true)
    }
    fn selected(
        scope: TenantScope,
        snapshot: TenantAuthzSnapshot,
        credential: CredentialKind,
        owned: bool,
    ) -> Result<Self, DbError> {
        if credential != CredentialKind::Jwt
            || scope.tenant_id().is_nil()
            || scope.user_id().is_nil()
            || snapshot.token_version < 0
            || snapshot.tenant_authz_version <= 0
            || snapshot.membership_authz_version <= 0
        {
            return Err(denied());
        }
        Ok(Self {
            tenant: scope.tenant_id(),
            authority: if owned {
                Authority::Owned(scope, snapshot)
            } else {
                Authority::Tenant(scope, snapshot)
            },
        })
    }
    pub fn platform(
        scope: PlatformScope,
        tenant: Uuid,
        token_version: i32,
        credential: CredentialKind,
    ) -> Result<Self, DbError> {
        if credential != CredentialKind::Jwt
            || tenant.is_nil()
            || scope.user_id().is_nil()
            || token_version < 0
            || !matches!(
                scope.platform_role(),
                PlatformRole::Root | PlatformRole::Operator
            )
        {
            return Err(denied());
        }
        Ok(Self {
            tenant,
            authority: Authority::Platform(scope, token_version),
        })
    }
    pub const fn tenant_id(self) -> Uuid {
        self.tenant
    }
    pub fn actor_user_id(self) -> Uuid {
        match self.authority {
            Authority::Tenant(s, _) | Authority::Owned(s, _) => s.user_id(),
            Authority::Platform(s, _) => s.user_id(),
        }
    }
    pub fn require_action(self, action: NodeAction) -> Result<(), DbError> {
        match self.authority {
            Authority::Owned(..)
                if matches!(action, NodeAction::CancelTask | NodeAction::ArchiveTask) =>
            {
                Ok(())
            }
            Authority::Tenant(_, _) => Ok(()),
            Authority::Platform(s, _) if s.platform_role() == PlatformRole::Root => Ok(()),
            Authority::Platform(s, _)
                if s.platform_role() == PlatformRole::Operator
                    && matches!(action, NodeAction::Exclude | NodeAction::Recover) =>
            {
                Ok(())
            }
            _ => Err(denied()),
        }
    }
    fn parts(self) -> (String, Vec<Value>) {
        let (version, tv, mv, role) = match self.authority {
            Authority::Tenant(_, v) | Authority::Owned(_, v) => (
                v.token_version,
                Some(v.tenant_authz_version),
                Some(v.membership_authz_version),
                None,
            ),
            Authority::Platform(s, v) => (v, None, None, Some(s.platform_role())),
        };
        let values = vec![
            self.tenant.into(),
            self.actor_user_id().into(),
            version.into(),
            tv.into(),
            mv.into(),
        ];
        let predicate = if let Some(role) = role {
            format!(
                "EXISTS(SELECT 1 FROM users au WHERE au.id=$2 AND au.status='active' AND au.token_version=$3 AND au.platform_role='{}') AND $4::bigint IS NULL AND $5::bigint IS NULL",
                role.as_str()
            )
        } else {
            let admin = if matches!(self.authority, Authority::Tenant(..)) {
                " AND am.tenant_role='admin'"
            } else {
                ""
            };
            format!(
                "EXISTS(SELECT 1 FROM users au JOIN tenant_memberships am ON am.user_id=au.id JOIN tenants at ON at.id=am.tenant_id WHERE au.id=$2 AND am.tenant_id=$1 AND au.status='active' AND am.status='active' AND at.status='active' AND au.token_version=$3 AND at.authz_version=$4 AND am.authz_version=$5{admin})"
            )
        };
        (predicate, values)
    }
}
#[derive(Debug, Clone, Copy)]
pub enum NodeResource {
    Node,
    Task,
    Token,
}
impl NodeResource {
    fn table(self) -> &'static str {
        match self {
            Self::Node => "nodes",
            Self::Task => "node_tasks",
            Self::Token => "user_node_gateway_tokens",
        }
    }
    fn owner(self) -> &'static str {
        if matches!(self, Self::Node) {
            "owner_user_id"
        } else {
            "user_id"
        }
    }
    fn search(self) -> &'static str {
        match self {
            Self::Node => "display_name",
            Self::Task => "model",
            Self::Token => "token_preview",
        }
    }
    fn columns(self) -> &'static str {
        match self {
            Self::Node => {
                "r.id,r.tenant_id,r.owner_user_id,r.display_name,r.status,r.consecutive_failure_count,r.failure_threshold,r.last_heartbeat_at,r.created_at,r.updated_at"
            }
            Self::Task => {
                "r.id,r.request_id,r.tenant_id,r.user_id,r.model,r.status,r.assigned_node_id,r.failure_count,r.failure_threshold,r.queued_at,r.claimed_at,r.finished_at,r.deadline_at,r.created_at,r.updated_at,r.cancellation_requested_at,r.archived_at"
            }
            Self::Token => {
                "r.id,r.tenant_id,r.user_id,r.token_preview,r.status,r.is_revealed,r.approved_by,r.actioned_at,r.consumed_at,r.consumed_node_id,r.issued_at,r.updated_at"
            }
        }
    }
}
#[derive(Debug, Clone, Default)]
pub struct NodeFilter {
    pub owner_user_id: Option<Uuid>,
    pub status: Option<String>,
    pub search: Option<String>,
    pub archived: Option<bool>,
}
fn filters(
    scope: NodeControlScope,
    kind: NodeResource,
    filter: &NodeFilter,
    id: Option<Uuid>,
) -> Result<(String, Vec<Value>), DbError> {
    if matches!(kind, NodeResource::Token)
        && matches!(scope.authority,Authority::Platform(s,_) if s.platform_role()==PlatformRole::Operator)
    {
        return Err(denied());
    }
    if filter.archived.is_some() && !matches!(kind, NodeResource::Task) {
        return Err(invalid("archive filter applies only to tasks"));
    }
    let (auth, mut values) = scope.parts();
    let mut predicate = format!("r.tenant_id=$1 AND {auth}");
    if matches!(kind, NodeResource::Task) && id.is_none() {
        values.push(filter.archived.unwrap_or(false).into());
        predicate.push_str(&format!(
            " AND (r.archived_at IS NOT NULL)=${}",
            values.len()
        ));
    }
    if matches!(scope.authority, Authority::Owned(..)) {
        predicate.push_str(&format!(" AND r.{}=$2", kind.owner()));
    }
    if let Some(owner) = filter.owner_user_id {
        if owner.is_nil() {
            return Err(invalid("owner ID required"));
        }
        values.push(owner.into());
        predicate.push_str(&format!(" AND r.{}=${}", kind.owner(), values.len()));
    }
    if let Some(status) = &filter.status {
        let valid = match kind {
            NodeResource::Node => matches!(status.as_str(), "online" | "offline" | "excluded"),
            NodeResource::Task => matches!(
                status.as_str(),
                "queued" | "leased" | "succeeded" | "failed" | "expired"
            ),
            NodeResource::Token => matches!(
                status.as_str(),
                "pending" | "approved" | "rejected" | "consumed"
            ),
        };
        if !valid {
            return Err(invalid("unknown resource status"));
        }
        values.push(status.clone().into());
        predicate.push_str(&format!(" AND r.status=${}", values.len()));
    }
    if let Some(search) = &filter.search {
        if search.chars().count() > 200 || search.chars().any(char::is_control) {
            return Err(invalid("bounded search text required"));
        }
        let escaped = search
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        values.push(format!("%{escaped}%").into());
        predicate.push_str(&format!(" AND r.{} ILIKE ${}", kind.search(), values.len()));
    }
    if let Some(id) = id {
        if id.is_nil() {
            return Err(invalid("resource ID required"));
        }
        values.push(id.into());
        predicate.push_str(&format!(" AND r.id=${}", values.len()));
    }
    Ok((predicate, values))
}
#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct NodeInfo {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub owner_user_id: Uuid,
    pub display_name: String,
    pub status: String,
    pub consecutive_failure_count: i32,
    pub failure_threshold: i32,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct TaskInfo {
    pub id: Uuid,
    pub request_id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub model: String,
    pub status: String,
    pub assigned_node_id: Option<Uuid>,
    pub failure_count: i32,
    pub failure_threshold: i32,
    pub queued_at: DateTime<Utc>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub deadline_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub cancellation_requested_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
}
#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct TokenInfo {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub token_preview: String,
    pub status: String,
    pub is_revealed: bool,
    pub approved_by: Option<Uuid>,
    pub actioned_at: Option<DateTime<Utc>>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub consumed_node_id: Option<Uuid>,
    pub issued_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
fn query(
    scope: NodeControlScope,
    kind: NodeResource,
    filter: &NodeFilter,
    id: Option<Uuid>,
    page: Option<(i64, i64)>,
    count: bool,
) -> Result<Statement, DbError> {
    let (p, mut v) = filters(scope, kind, filter, id)?;
    let columns = if count {
        "COUNT(*)::bigint AS total"
    } else {
        kind.columns()
    };
    let mut sql = format!("SELECT {columns} FROM {} r WHERE {p}", kind.table());
    if !count {
        let order = if matches!(kind, NodeResource::Token) {
            "issued_at"
        } else {
            "created_at"
        };
        sql.push_str(&format!(" ORDER BY r.{order} DESC,r.id DESC"));
    }
    if let Some((limit, offset)) = page {
        if !(1..=100).contains(&limit) || !(0..=100_000_000).contains(&offset) {
            return Err(invalid("pagination outside bounds"));
        }
        v.push(limit.into());
        let l = v.len();
        v.push(offset.into());
        sql.push_str(&format!(" LIMIT ${l} OFFSET ${}", v.len()));
    }
    Ok(Statement::from_sql_and_values(DbBackend::Postgres, sql, v))
}
pub async fn count(
    db: &impl ConnectionTrait,
    scope: NodeControlScope,
    kind: NodeResource,
    f: &NodeFilter,
) -> Result<i64, DbError> {
    let row = db
        .query_one(query(scope, kind, f, None, None, true)?)
        .await?
        .ok_or_else(|| DbError::Other("node count unavailable".into()))?;
    Ok(row.try_get("", "total")?)
}
macro_rules! reads {
    ($list:ident,$one:ident,$ty:ty,$kind:ident) => {
        pub async fn $list(
            db: &impl ConnectionTrait,
            scope: NodeControlScope,
            f: &NodeFilter,
            limit: i64,
            offset: i64,
        ) -> Result<Vec<$ty>, DbError> {
            Ok(<$ty>::find_by_statement(query(
                scope,
                NodeResource::$kind,
                f,
                None,
                Some((limit, offset)),
                false,
            )?)
            .all(db)
            .await?)
        }
        pub async fn $one(
            db: &impl ConnectionTrait,
            scope: NodeControlScope,
            id: Uuid,
        ) -> Result<Option<$ty>, DbError> {
            Ok(<$ty>::find_by_statement(query(
                scope,
                NodeResource::$kind,
                &NodeFilter::default(),
                Some(id),
                None,
                false,
            )?)
            .one(db)
            .await?)
        }
    };
}
reads!(nodes, node, NodeInfo, Node);
reads!(tasks, task, TaskInfo, Task);
reads!(tokens, token, TokenInfo, Token);

/// Recheck a current console grant in the same transaction as the resource mutation.
/// Lock order is identity fence -> tenant -> actor; callers must never invert
/// task-before-node runtime completion ordering by locking a task after a node.
pub async fn lock_for_action(
    tx: &DatabaseTransaction,
    scope: NodeControlScope,
    action: NodeAction,
    audit: &AuditContext,
) -> Result<(Tenant, AuditContext), DbError> {
    scope.require_action(action)?;
    if audit.credential_kind != CredentialKind::Jwt
        || audit.actor_user_id != scope.actor_user_id()
        || audit.request_id.is_none_or(|v| v.is_nil())
    {
        return Err(denied());
    }
    match scope.authority {
        Authority::Tenant(s, snapshot) => {
            let current = tenant_control::revalidate_in_transaction(tx, s, snapshot, audit).await?;
            let t = Tenant::find_by_id(tx, scope.tenant)
                .await?
                .ok_or_else(denied)?;
            Ok((t, current.actor))
        }
        Authority::Platform(s, version) => {
            lock_identity_admin(tx).await?;
            let t = Tenant::find_by_id_for_update(tx, scope.tenant)
                .await?
                .ok_or_else(|| DbError::not_found("Tenant", scope.tenant))?;
            let u = User::find_by_id_for_update(tx, s.user_id())
                .await?
                .filter(|u| {
                    u.status == "active"
                        && u.platform_role == s.platform_role().as_str()
                        && u.token_version == version
                })
                .ok_or_else(denied)?;
            Ok((
                t,
                AuditContext {
                    actor_platform_role: u.platform_role()?,
                    actor_tenant_role: None,
                    ..*audit
                },
            ))
        }
        Authority::Owned(..) => {
            let actor = lock_owned_console(tx, scope, audit).await?;
            let tenant = Tenant::find_by_id(tx, scope.tenant)
                .await?
                .ok_or_else(denied)?;
            Ok((tenant, actor))
        }
    }
}
pub fn validate_reason(reason: &str) -> Result<&str, DbError> {
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > 1000 || reason.chars().any(char::is_control) {
        return Err(invalid("a bounded nonempty reason is required"));
    }
    Ok(reason)
}
async fn begin(
    db: &(impl ConnectionTrait + TransactionTrait),
) -> Result<DatabaseTransaction, DbError> {
    let tx = db.begin().await?;
    tx.execute_unprepared("SET LOCAL lock_timeout='3s'; SET LOCAL statement_timeout='8s'")
        .await?;
    Ok(tx)
}
async fn finish<T>(tx: DatabaseTransaction, result: Result<T, DbError>) -> Result<T, DbError> {
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
async fn owner_active(
    tx: &DatabaseTransaction,
    tenant: &Tenant,
    user: Uuid,
) -> Result<(), DbError> {
    if !tenant.is_active() {
        return Err(invalid("active tenant required for enabling work"));
    }
    let row=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT 1 FROM tenant_memberships m JOIN users u ON u.id=m.user_id WHERE m.tenant_id=$1 AND m.user_id=$2 AND m.status='active' AND u.status='active'",[tenant.id.into(),user.into()])).await?;
    if row.is_none() {
        return Err(invalid("active resource owner membership required"));
    }
    Ok(())
}
async fn locked_node(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    id: Uuid,
) -> Result<NodeInfo, DbError> {
    NodeInfo::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "SELECT {} FROM nodes r WHERE r.tenant_id=$1 AND r.id=$2 FOR UPDATE",
            NodeResource::Node.columns()
        ),
        [tenant.into(), id.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| DbError::not_found("Node", id))
}
#[derive(Debug, Clone, Default)]
pub struct NodePatch {
    pub display_name: Option<String>,
    pub failure_threshold: Option<i32>,
}
#[derive(Debug, Clone, Serialize)]
pub struct NodeChange {
    pub node: NodeInfo,
    pub changed: bool,
    pub deleted: bool,
}
async fn drain(
    tx: &DatabaseTransaction,
    node: &NodeInfo,
    actor: &AuditContext,
    reason: &str,
) -> Result<u64, DbError> {
    // A disabled session can finish only its already-issued leases. Do not set
    // revoked_at: that would strand accepted result/settlement submissions.
    let sessions=tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE node_sessions s SET accepting_tasks=FALSE,expires_at=GREATEST(s.expires_at,COALESCE((SELECT MAX(nt.complete_grace_until) FROM node_tasks nt WHERE nt.assigned_session_id=s.id AND nt.status='leased'),s.expires_at)) WHERE s.node_id=$1 AND s.tenant_id=$2 AND s.owner_user_id=$3 AND s.accepting_tasks",
        [node.id.into(),node.tenant_id.into(),node.owner_user_id.into()])).await?.rows_affected();
    let tokens=tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE user_node_gateway_tokens SET status='rejected',revoke_reason=$4,approved_by=$5,actioned_at=clock_timestamp(),updated_at=GREATEST(clock_timestamp(),updated_at+INTERVAL '1 microsecond') WHERE consumed_node_id=$1 AND tenant_id=$2 AND user_id=$3 AND status<>'rejected'",
        [node.id.into(),node.tenant_id.into(),node.owner_user_id.into(),reason.into(),actor.actor_user_id.into()])).await?.rows_affected();
    Ok(sessions + tokens)
}
/// The target, revision and intended action travel together, separately
/// from the current actor proof. Never deserialize this as authorization.
#[derive(Debug, Clone, Copy)]
pub struct NodeMutation<'a> {
    pub id: Uuid,
    pub expected_updated_at: DateTime<Utc>,
    pub action: NodeAction,
    pub patch: &'a NodePatch,
    pub reason: &'a str,
}
pub async fn change_node(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: NodeControlScope,
    audit: &AuditContext,
    mutation: NodeMutation<'_>,
) -> Result<NodeChange, DbError> {
    let NodeMutation {
        id,
        expected_updated_at: expected,
        action,
        patch,
        reason,
    } = mutation;
    let reason = validate_reason(reason)?;
    if !matches!(
        action,
        NodeAction::Configure
            | NodeAction::Exclude
            | NodeAction::Recover
            | NodeAction::Revoke
            | NodeAction::Delete
    ) {
        return Err(invalid("invalid node action"));
    }
    if patch.display_name.as_ref().is_some_and(|v| {
        v.trim().is_empty() || v.chars().count() > 200 || v.chars().any(char::is_control)
    }) || patch
        .failure_threshold
        .is_some_and(|v| !(1..=100).contains(&v))
    {
        return Err(invalid("invalid node name or failure threshold"));
    }
    if action != NodeAction::Configure
        && (patch.display_name.is_some() || patch.failure_threshold.is_some())
    {
        return Err(invalid("configuration fields only belong to configure"));
    }
    let tx = begin(db).await?;
    let result=async{
        let (tenant,actor)=lock_for_action(&tx,scope,action,audit).await?;
        let old=locked_node(&tx,scope.tenant,id).await?;
        if old.updated_at!=expected{return Err(conflict("Node",id));}
        let before=json!({"status":old.status,"failure_threshold":old.failure_threshold,"owner_user_id":old.owner_user_id});
        let mut status=old.status.clone();let mut failures=old.consecutive_failure_count;
        let mut changed=false;
        match action{
            NodeAction::Exclude=>status="excluded".into(),
            NodeAction::Recover=>{
                owner_active(&tx,&tenant,old.owner_user_id).await?;
                // Recovery clears operational exclusion, never resurrects a
                // revoked registration token or a draining/superseded session.
                let valid=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT 1 FROM node_sessions WHERE node_id=$1 AND tenant_id=$2 AND owner_user_id=$3 AND accepting_tasks AND revoked_at IS NULL AND expires_at>clock_timestamp() LIMIT 1",[id.into(),scope.tenant.into(),old.owner_user_id.into()])).await?.is_some();
                status=if valid {"online"} else {"offline"}.into();failures=0;
            }
            NodeAction::Revoke=>{status="excluded".into();changed=drain(&tx,&old,&actor,reason).await?>0;}
            NodeAction::Delete=>{
                if old.status=="online"{return Err(conflict("exclude node before deletion",id));}
                // Never unlink historical tasks, submissions or money to make
                // DELETE succeed. The tenant parent prevents a concurrent lease.
                let evidence=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                    "SELECT 1 WHERE EXISTS(SELECT 1 FROM node_tasks t WHERE t.assigned_node_id=$1 OR t.assigned_session_id IN(SELECT id FROM node_sessions WHERE node_id=$1)) OR EXISTS(SELECT 1 FROM node_task_submissions WHERE node_id=$1) OR EXISTS(SELECT 1 FROM node_tips WHERE node_id=$1) OR EXISTS(SELECT 1 FROM node_native_streams WHERE node_id=$1)",[id.into()])).await?;
                if evidence.is_some(){return Err(conflict("node has retained task or financial evidence",id));}
                let deleted=tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,"DELETE FROM nodes WHERE tenant_id=$1 AND owner_user_id=$2 AND id=$3 AND updated_at=$4",[scope.tenant.into(),old.owner_user_id.into(),id.into(),expected.into()])).await?.rows_affected();
                if deleted!=1{return Err(conflict("Node",id));}
                TenantAuditEvent::append(&tx,AuditScopeType::Tenant,Some(scope.tenant),&actor,"node.delete","node",Some(&id.to_string()),AuditResult::Success,json!({"reason":reason,"before":before,"owner_user_id":old.owner_user_id,"changed":true})).await?;
                return Ok(NodeChange{node:old,changed:true,deleted:true});
            }
            NodeAction::Configure=>{}, _=>unreachable!(),
        }
        let name=patch.display_name.as_deref().map(str::trim).unwrap_or(&old.display_name);
        let threshold=patch.failure_threshold.unwrap_or(old.failure_threshold);
        changed|=status!=old.status||failures!=old.consecutive_failure_count||name!=old.display_name||threshold!=old.failure_threshold;
        let row=if changed {
            NodeInfo::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,format!("UPDATE nodes r SET status=$5,consecutive_failure_count=$6,display_name=$7,failure_threshold=$8,updated_at=GREATEST(clock_timestamp(),updated_at+INTERVAL '1 microsecond') WHERE tenant_id=$1 AND owner_user_id=$2 AND id=$3 AND updated_at=$4 RETURNING {}",NodeResource::Node.columns()),[scope.tenant.into(),old.owner_user_id.into(),id.into(),expected.into(),status.into(),failures.into(),name.into(),threshold.into()])).one(&tx).await?.ok_or_else(||conflict("Node",id))?
        }else{old};
        let label=match action {NodeAction::Configure=>"node.configure",NodeAction::Exclude=>"node.exclude",NodeAction::Recover=>"node.recover",NodeAction::Revoke=>"node.revoke",_=>unreachable!()};
        TenantAuditEvent::append(&tx,AuditScopeType::Tenant,Some(scope.tenant),&actor,label,"node",Some(&id.to_string()),AuditResult::Success,json!({"reason":reason,"before":before,"after":{"status":row.status,"failure_threshold":row.failure_threshold},"owner_user_id":row.owner_user_id,"changed":changed})).await?;
        Ok(NodeChange{node:row,changed,deleted:false})
    }.await;
    finish(tx, result).await
}
#[derive(Debug, Clone, Serialize)]
pub struct TokenChange {
    pub token: TokenInfo,
    pub changed: bool,
}
pub async fn change_token(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: NodeControlScope,
    id: Uuid,
    expected: DateTime<Utc>,
    action: NodeAction,
    audit: &AuditContext,
    reason: &str,
) -> Result<TokenChange, DbError> {
    let reason = validate_reason(reason)?;
    if !matches!(
        action,
        NodeAction::ApproveToken | NodeAction::RejectToken | NodeAction::RevokeToken
    ) {
        return Err(invalid("invalid token action"));
    }
    let tx = begin(db).await?;
    let result=async{
        let (tenant,actor)=lock_for_action(&tx,scope,action,audit).await?;
        let old=TokenInfo::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,format!("SELECT {} FROM user_node_gateway_tokens r WHERE r.tenant_id=$1 AND r.id=$2 FOR UPDATE",NodeResource::Token.columns()),[scope.tenant.into(),id.into()])).one(&tx).await?.ok_or_else(||DbError::not_found("Node registration",id))?;
        if old.updated_at!=expected{return Err(conflict("Node registration",id));}
        if matches!(action,NodeAction::ApproveToken|NodeAction::RejectToken) && old.status!="pending"{return Err(conflict("only pending registrations can be decided",id));}
        if action==NodeAction::ApproveToken {owner_active(&tx,&tenant,old.user_id).await?;}
        let state=if action==NodeAction::ApproveToken {"approved"}else{"rejected"};
        let mut changed=old.status!=state;
        if action==NodeAction::RevokeToken && let Some(node_id)=old.consumed_node_id {
            let node=locked_node(&tx,scope.tenant,node_id).await?;
            if node.owner_user_id!=old.user_id{return Err(denied());}
            changed|=drain(&tx,&node,&actor,reason).await?>0;
            if node.status!="excluded"{
                tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE nodes SET status='excluded',updated_at=GREATEST(clock_timestamp(),updated_at+INTERVAL '1 microsecond') WHERE tenant_id=$1 AND owner_user_id=$2 AND id=$3",[scope.tenant.into(),old.user_id.into(),node_id.into()])).await?;
                changed=true;
            }
        }
        let row=if changed {
            TokenInfo::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,format!("UPDATE user_node_gateway_tokens r SET status=$4,approved_by=$5,actioned_at=clock_timestamp(),revoke_reason=$6,updated_at=GREATEST(clock_timestamp(),updated_at+INTERVAL '1 microsecond') WHERE tenant_id=$1 AND user_id=$2 AND id=$3 RETURNING {}",NodeResource::Token.columns()),[scope.tenant.into(),old.user_id.into(),id.into(),state.into(),actor.actor_user_id.into(),if action==NodeAction::RevokeToken{Some(reason.to_owned())}else{None}.into()])).one(&tx).await?.ok_or_else(||conflict("Node registration",id))?
        }else{old.clone()};
        let label=match action{NodeAction::ApproveToken=>"node.registration.approve",NodeAction::RejectToken=>"node.registration.reject",_=>"node.registration.revoke"};
        TenantAuditEvent::append(&tx,AuditScopeType::Tenant,Some(scope.tenant),&actor,label,"node_registration",Some(&id.to_string()),AuditResult::Success,json!({"reason":reason,"user_id":old.user_id,"before":{"status":old.status},"after":{"status":row.status},"changed":changed})).await?;
        Ok(TokenChange{token:row,changed})
    }.await;
    finish(tx, result).await
}

#[derive(Debug, Serialize, FromQueryResult)]
pub struct NodeStats {
    pub total: i64,
    pub online: i64,
    pub offline: i64,
    pub excluded: i64,
}
#[derive(Debug, Serialize, FromQueryResult)]
pub struct TaskStats {
    pub total: i64,
    pub queued: i64,
    pub leased: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub expired: i64,
}
pub async fn node_stats(
    db: &impl ConnectionTrait,
    scope: NodeControlScope,
) -> Result<NodeStats, DbError> {
    let (p, v) = filters(scope, NodeResource::Node, &NodeFilter::default(), None)?;
    NodeStats::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,format!("SELECT COUNT(*)::bigint AS total,COUNT(*) FILTER(WHERE r.status='online')::bigint AS online,COUNT(*) FILTER(WHERE r.status='offline')::bigint AS offline,COUNT(*) FILTER(WHERE r.status='excluded')::bigint AS excluded FROM nodes r WHERE {p}"),v)).one(db).await?.ok_or_else(||DbError::Other("node totals unavailable".into()))
}
pub async fn task_stats(
    db: &impl ConnectionTrait,
    scope: NodeControlScope,
) -> Result<TaskStats, DbError> {
    let (p, v) = filters(scope, NodeResource::Task, &NodeFilter::default(), None)?;
    TaskStats::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,format!("SELECT COUNT(*)::bigint AS total,COUNT(*) FILTER(WHERE r.status='queued')::bigint AS queued,COUNT(*) FILTER(WHERE r.status='leased')::bigint AS leased,COUNT(*) FILTER(WHERE r.status='succeeded')::bigint AS succeeded,COUNT(*) FILTER(WHERE r.status='failed')::bigint AS failed,COUNT(*) FILTER(WHERE r.status='expired')::bigint AS expired FROM node_tasks r WHERE {p}"),v)).one(db).await?.ok_or_else(||DbError::Other("task totals unavailable".into()))
}

async fn lock_owned_console(
    tx: &DatabaseTransaction,
    scope: NodeControlScope,
    audit: &AuditContext,
) -> Result<AuditContext, DbError> {
    let Authority::Owned(s, version) = scope.authority else {
        return Err(denied());
    };
    if audit.actor_user_id != s.user_id()
        || audit.credential_kind != CredentialKind::Jwt
        || audit.request_id.is_none_or(|v| v.is_nil())
    {
        return Err(denied());
    }
    // Same low-frequency administrative fence as approval and registration.
    // No inference request, heartbeat or task completion takes this fence here.
    lock_identity_admin(tx).await?;
    let tenant = Tenant::find_by_id_for_update(tx, s.tenant_id())
        .await?
        .filter(|t| t.status == "active" && t.authz_version == version.tenant_authz_version)
        .ok_or_else(denied)?;
    let user = User::find_by_id_for_update(tx, s.user_id())
        .await?
        .filter(|u| u.status == "active" && u.token_version == version.token_version)
        .ok_or_else(denied)?;
    let member=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT tenant_role FROM tenant_memberships WHERE tenant_id=$1 AND user_id=$2 AND status='active' AND authz_version=$3 FOR UPDATE",[tenant.id.into(),user.id.into(),version.membership_authz_version.into()])).await?.ok_or_else(denied)?;
    let role: String = member.try_get("", "tenant_role")?;
    Ok(AuditContext {
        actor_platform_role: user.platform_role()?,
        actor_tenant_role: Some(role.parse().map_err(DbError::Other)?),
        ..*audit
    })
}
/// The caller owns the transaction through secret reconstruction and commit.
/// This internal record is never a console metadata serialization type.
pub async fn owner_registration_for_reveal(
    tx: &DatabaseTransaction,
    scope: NodeControlScope,
    audit: &AuditContext,
) -> Result<super::user_node_gateway_token::UserNodeGatewayToken, DbError> {
    let actor = lock_owned_console(tx, scope, audit).await?;
    let mut record=super::user_node_gateway_token::UserNodeGatewayToken::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT * FROM user_node_gateway_tokens WHERE tenant_id=$1 AND user_id=$2 ORDER BY issued_at DESC,id DESC LIMIT 1 FOR UPDATE",[scope.tenant.into(),actor.actor_user_id.into()])).one(tx).await?.ok_or_else(||DbError::not_found("Node registration",scope.actor_user_id()))?;
    if record.status == "approved" {
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE user_node_gateway_tokens SET is_revealed=TRUE WHERE tenant_id=$1 AND user_id=$2 AND id=$3 AND status='approved'",[scope.tenant.into(),actor.actor_user_id.into(),record.id.into()])).await?;
        TenantAuditEvent::append(
            tx,
            AuditScopeType::Tenant,
            Some(scope.tenant),
            &actor,
            "node.registration.reveal",
            "node_registration",
            Some(&record.id.to_string()),
            AuditResult::Success,
            json!({"user_id":actor.actor_user_id,"status":"approved"}),
        )
        .await?;
    }
    if record.status == "approved" {
        record.is_revealed = true;
    }
    Ok(record)
}
pub async fn request_owner_registration(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: NodeControlScope,
    id: Uuid,
    hash: &str,
    preview: &str,
    audit: &AuditContext,
) -> Result<TokenInfo, DbError> {
    if id.is_nil()
        || hash.len() != 64
        || !hash.bytes().all(|v| v.is_ascii_hexdigit())
        || preview.len() > 32
        || preview.chars().any(char::is_control)
    {
        return Err(invalid("invalid generated credential metadata"));
    }
    let tx = begin(db).await?;
    let result=async{
        let actor=lock_owned_console(&tx,scope,audit).await?;
        let existing=TokenInfo::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,format!("SELECT {} FROM user_node_gateway_tokens r WHERE r.tenant_id=$1 AND r.user_id=$2 AND r.status IN ('pending','approved') ORDER BY r.issued_at DESC,r.id DESC LIMIT 1 FOR UPDATE",NodeResource::Token.columns()),[scope.tenant.into(),actor.actor_user_id.into()])).one(&tx).await?;
        if let Some(record)=existing{return Ok(record);}
        // Historical consumed/revoked tokens are not revived. A replacement is
        // a new pending request that needs a fresh administrative approval.
        let record=TokenInfo::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,format!("INSERT INTO user_node_gateway_tokens AS r(id,tenant_id,user_id,token_hash,token_preview) VALUES($1,$2,$3,$4,$5) RETURNING {}",NodeResource::Token.columns()),[id.into(),scope.tenant.into(),actor.actor_user_id.into(),hash.into(),preview.into()])).one(&tx).await?.ok_or_else(||DbError::Other("node registration was not created".into()))?;
        TenantAuditEvent::append(&tx,AuditScopeType::Tenant,Some(scope.tenant),&actor,"node.registration.request","node_registration",Some(&id.to_string()),AuditResult::Success,json!({"user_id":actor.actor_user_id,"status":"pending"})).await?;
        Ok(record)
    }.await;
    finish(tx, result).await
}
pub async fn delete_owner_rejected_registration(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: NodeControlScope,
    id: Uuid,
    audit: &AuditContext,
) -> Result<(), DbError> {
    let tx = begin(db).await?;
    let result=async{
        let actor=lock_owned_console(&tx,scope,audit).await?;
        let row=tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,"DELETE FROM user_node_gateway_tokens WHERE tenant_id=$1 AND user_id=$2 AND id=$3 AND status='rejected' AND revoke_reason IS NULL AND consumed_node_id IS NULL AND consumed_at IS NULL",[scope.tenant.into(),actor.actor_user_id.into(),id.into()])).await?;
        if row.rows_affected()!=1{return Err(DbError::not_found("Rejected node registration",id));}
        TenantAuditEvent::append(&tx,AuditScopeType::Tenant,Some(scope.tenant),&actor,"node.registration.delete","node_registration",Some(&id.to_string()),AuditResult::Success,json!({"user_id":actor.actor_user_id})).await?;Ok(())
    }.await;
    finish(tx, result).await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> NodeControlScope {
        let id = Uuid::new_v4();
        NodeControlScope::platform(
            PlatformScope::checked(id, PlatformRole::Root).unwrap(),
            Uuid::new_v4(),
            0,
            CredentialKind::Jwt,
        )
        .unwrap()
    }
    #[test]
    fn node_list_uses_an_immutable_unique_pagination_order() {
        let q = query(
            scope(),
            NodeResource::Node,
            &NodeFilter::default(),
            None,
            Some((10, 0)),
            false,
        )
        .unwrap();
        assert!(q.sql.contains("ORDER BY r.created_at DESC,r.id DESC"));
        assert!(!q.sql.contains("ORDER BY r.updated_at"));
        assert!(q.sql.contains("r.tenant_id=$1"));
    }
    #[test]
    fn task_list_uses_a_unique_pagination_order() {
        let q = query(
            scope(),
            NodeResource::Task,
            &NodeFilter::default(),
            None,
            Some((10, 0)),
            false,
        )
        .unwrap();
        assert!(q.sql.contains("ORDER BY r.created_at DESC,r.id DESC"));
        assert!(!q.sql.contains("payload_json"));
        assert!(!q.sql.contains("result_json"));
    }
    #[test]
    fn operator_actions_are_an_explicit_small_allowlist() {
        let s = NodeControlScope::platform(
            PlatformScope::checked(Uuid::new_v4(), PlatformRole::Operator).unwrap(),
            Uuid::new_v4(),
            0,
            CredentialKind::Jwt,
        )
        .unwrap();
        assert!(s.require_action(NodeAction::Exclude).is_ok());
        assert!(s.require_action(NodeAction::Recover).is_ok());
        for a in [
            NodeAction::Revoke,
            NodeAction::Delete,
            NodeAction::Configure,
            NodeAction::ApproveToken,
            NodeAction::RejectToken,
            NodeAction::RevokeToken,
            NodeAction::CancelTask,
            NodeAction::ArchiveTask,
        ] {
            assert!(s.require_action(a).is_err());
        }
        assert!(
            query(
                s,
                NodeResource::Token,
                &NodeFilter::default(),
                None,
                None,
                false
            )
            .is_err()
        );
    }
    #[test]
    fn node_filters_and_count_share_the_same_scope() {
        let s = scope();
        let f = NodeFilter {
            status: Some("online".into()),
            search: Some("节点_%".into()),
            ..Default::default()
        };
        let r = query(s, NodeResource::Node, &f, None, Some((1, 0)), false).unwrap();
        let c = query(s, NodeResource::Node, &f, None, None, true).unwrap();
        assert_eq!(
            r.sql
                .split(" WHERE ")
                .last()
                .unwrap()
                .split(" ORDER BY ")
                .next()
                .unwrap(),
            c.sql.split(" WHERE ").last().unwrap()
        );
        assert_eq!(&r.values.unwrap().0[..7], &c.values.unwrap().0);
    }
}
