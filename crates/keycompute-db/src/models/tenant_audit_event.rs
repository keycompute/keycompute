//! Immutable audit snapshots and transaction-bound administrative authority.
use crate::DbError;
use chrono::{DateTime, Utc};
use keycompute_types::{
    AuditResult, AuditScopeType, CredentialKind, PlatformRole, PlatformScope, TenantRole,
    TenantScope,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct TenantAuditEvent {
    pub id: Uuid,
    pub scope_type: String,
    pub tenant_id: Option<Uuid>,
    pub actor_user_id: Uuid,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub request_id: Option<Uuid>,
    pub credential_kind: String,
    pub actor_platform_role: String,
    pub actor_tenant_role: Option<String>,
    pub details: Value,
    pub result: String,
    pub created_at: DateTime<Utc>,
}

/// Constructed by trusted server code, never deserialized from a request.
#[derive(Debug, Clone, Copy)]
pub struct AuditContext {
    pub actor_user_id: Uuid,
    pub credential_kind: CredentialKind,
    pub actor_platform_role: PlatformRole,
    pub actor_tenant_role: Option<TenantRole>,
    pub request_id: Option<Uuid>,
}

/// Administrative writes take this fence before parent/child locks. Normal
/// inference, settlement and display reads must never take this write fence.
pub(crate) async fn lock_identity_admin(tx: &DatabaseTransaction) -> Result<(), DbError> {
    let result = tx
        .execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await?;
    if result.rows_affected() != 1 {
        return Err(DbError::Other(
            "identity administration fence is missing".into(),
        ));
    }
    Ok(())
}

impl AuditContext {
    /// Revalidate current platform authority while the security fence is held.
    /// A cached role string in the audit metadata is not a privilege grant.
    pub(crate) async fn require_root(&self, tx: &DatabaseTransaction) -> Result<Self, DbError> {
        if self.credential_kind != CredentialKind::Jwt || self.actor_user_id.is_nil() {
            return Err(DbError::Other("root console authority required".into()));
        }
        let user = super::user::User::find_by_id(tx, self.actor_user_id)
            .await?
            .filter(|user| user.status == "active" && user.platform_role == "root")
            .ok_or_else(|| DbError::Other("root console authority required".into()))?;
        Ok(Self {
            actor_platform_role: user.platform_role()?,
            ..*self
        })
    }

    /// Parent-first authorization under the caller's transaction. Every tenant
    /// operation requires membership, independently of platform privilege.
    pub(crate) async fn require_tenant_admin(
        &self,
        tx: &DatabaseTransaction,
        tenant: Uuid,
    ) -> Result<Self, DbError> {
        if self.credential_kind != CredentialKind::Jwt
            || tenant.is_nil()
            || self.actor_user_id.is_nil()
        {
            return Err(DbError::Other(
                "tenant administrator console authority required".into(),
            ));
        }
        let current = super::tenant::Tenant::find_by_id_for_update(tx, tenant)
            .await?
            .filter(|tenant| tenant.is_active())
            .ok_or_else(|| DbError::Other("active tenant required".into()))?;
        let user = super::user::User::find_by_id(tx, self.actor_user_id)
            .await?
            .filter(|user| user.status == "active")
            .ok_or_else(|| DbError::Other("active actor required".into()))?;
        let member = super::tenant_membership::TenantMembership::find_any(tx, current.id, user.id)
            .await?
            .filter(|member| member.status == "active" && member.role == "admin")
            .ok_or_else(|| DbError::Other("tenant administrator membership required".into()))?;
        Ok(Self {
            actor_platform_role: user.platform_role()?,
            actor_tenant_role: Some(member.tenant_role()?),
            ..*self
        })
    }
}

fn audit_details(mut details: Value) -> Result<Value, DbError> {
    if !details.is_object() || details.to_string().len() > 16384 {
        return Err(DbError::Other(
            "audit details must be a bounded object".into(),
        ));
    }
    fn redact(value: &mut Value, depth: usize) {
        if depth > 8 {
            *value = Value::String("[redacted]".into());
            return;
        }
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    let key = key.to_ascii_lowercase();
                    let allowed = matches!(
                        key.as_str(),
                        "role"
                            | "previous_role"
                            | "status"
                            | "previous_status"
                            | "platform_role"
                            | "tenant_role"
                            | "email"
                            | "count"
                            | "version"
                            | "authz_version"
                            | "membership_version"
                            | "owner_user_id"
                            | "previous_owner_user_id"
                            | "user_id"
                            | "tenant_id"
                            | "account_id"
                            | "invitation_id"
                            | "key_id"
                            | "reason"
                            | "scope_type"
                            | "changed"
                            | "enabled"
                            | "default_rpm_limit"
                            | "default_tpm_limit"
                            | "operation"
                            | "amount"
                            | "currency"
                            | "before"
                            | "after"
                            | "result"
                    );
                    if allowed {
                        redact(value, depth + 1);
                    } else {
                        *value = Value::String("[redacted]".into());
                    }
                }
            }
            Value::Array(items) => {
                if items.len() > 64 {
                    *value = Value::String("[redacted]".into());
                } else {
                    for item in items {
                        redact(item, depth + 1);
                    }
                }
            }
            Value::String(text) if text.len() > 1000 => {
                *value = Value::String("[redacted]".into());
            }
            _ => {}
        }
    }
    redact(&mut details, 0);
    Ok(details)
}

impl TenantAuditEvent {
    #[allow(clippy::too_many_arguments)]
    pub async fn append(
        tx: &DatabaseTransaction,
        scope: AuditScopeType,
        tenant_id: Option<Uuid>,
        ctx: &AuditContext,
        action: &str,
        resource_type: &str,
        resource_id: Option<&str>,
        result: AuditResult,
        details: Value,
    ) -> Result<Self, DbError> {
        let valid_label = |value: &str| {
            !value.is_empty()
                && value.len() <= 100
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        };
        if !valid_label(action)
            || !valid_label(resource_type)
            || ctx.actor_user_id.is_nil()
            || resource_id.is_some_and(|id| {
                id.is_empty() || id.len() > 2048 || id.chars().any(char::is_control)
            })
            || tenant_id.is_some_and(|id| id.is_nil())
        {
            return Err(DbError::Other("invalid audit identity or action".into()));
        }
        if (scope == AuditScopeType::Tenant) != tenant_id.is_some() {
            return Err(DbError::Other("audit scope and tenant must agree".into()));
        }
        let details = audit_details(details)?;
        Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO tenant_audit_events(scope_type,tenant_id,actor_user_id,action,resource_type,resource_id,request_id,credential_kind,actor_platform_role,actor_tenant_role,details,result) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING *",
            [scope.as_str().into(), tenant_id.into(), ctx.actor_user_id.into(), action.into(),
                resource_type.into(), resource_id.map(str::to_owned).into(), ctx.request_id.into(),
                ctx.credential_kind.as_str().into(), ctx.actor_platform_role.as_str().into(),
                ctx.actor_tenant_role.map(|role| role.as_str()).into(), details.into(), result.as_str().into()],
        )).one(tx).await?.ok_or_else(|| DbError::Other("audit insert returned no row".into()))
    }

    pub async fn list_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        if scope.tenant_role() != TenantRole::Admin {
            return Err(DbError::Other("tenant admin required".into()));
        }
        Ok(Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT * FROM tenant_audit_events WHERE tenant_id=$1 ORDER BY created_at DESC,id DESC LIMIT $2 OFFSET $3",
            [scope.tenant_id().into(), limit.clamp(1,100).into(), offset.max(0).into()],
        )).all(db).await?)
    }

    pub async fn list_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        if scope.platform_role() != PlatformRole::Root {
            return Err(DbError::Other("root audit access required".into()));
        }
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM tenant_audit_events ORDER BY created_at DESC,id DESC LIMIT $1 OFFSET $2",
            [limit.clamp(1, 100).into(), offset.max(0).into()],
        ))
        .all(db)
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audit_redacts_nested_secrets_and_rejects_unbounded_input() {
        let value = audit_details(serde_json::json!({"role":"member","before":{"password":"secret","token":"secret"},"authorization":"Bearer secret"})).unwrap();
        assert_eq!(value["role"], "member");
        assert!(!value.to_string().contains("secret"));
        assert!(audit_details(serde_json::json!({"reason":"x".repeat(17000)})).is_err());
        assert!(audit_details(serde_json::json!([])).is_err());
    }
}
