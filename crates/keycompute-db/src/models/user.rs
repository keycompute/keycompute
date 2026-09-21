//! Global identities. Tenant authority exists only in tenant_memberships.
use super::query::escape_like_pattern;
use super::tenant_audit_event::{AuditContext, TenantAuditEvent, lock_identity_admin};
use crate::DbError;
use chrono::{DateTime, Utc};
use keycompute_types::{AuditResult, AuditScopeType, CredentialKind, PlatformRole, UserStatus};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub platform_role: String,
    pub status: String,
    pub token_version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateUserRequest {
    pub email: String,
    pub name: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateUserRequest {
    pub name: Option<String>,
}

pub(crate) fn normalized_email(email: &str) -> Result<String, DbError> {
    let email = email.trim().to_ascii_lowercase();
    if email.len() > 255 || email.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(DbError::Other("invalid email".into()));
    }
    let Some((local, domain)) = email.split_once('@') else {
        return Err(DbError::Other("invalid email".into()));
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return Err(DbError::Other("invalid email".into()));
    }
    Ok(email)
}

impl User {
    pub fn platform_role(&self) -> Result<PlatformRole, DbError> {
        self.platform_role.parse().map_err(DbError::Other)
    }
    pub fn user_status(&self) -> Result<UserStatus, DbError> {
        self.status.parse().map_err(DbError::Other)
    }

    /// Registration cannot select a platform role or create membership implicitly.
    pub async fn create(
        db: &impl ConnectionTrait,
        req: &CreateUserRequest,
    ) -> Result<Self, DbError> {
        if req.name.as_ref().is_some_and(|name| name.len() > 255) {
            return Err(DbError::Other("name exceeds limit".into()));
        }
        Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO users(email,name,platform_role,status) VALUES($1,$2,'none','active') RETURNING *",
            [normalized_email(&req.email)?.into(), req.name.clone().into()],
        )).one(db).await?.ok_or_else(|| DbError::Other("user insert returned no row".into()))
    }

    /// Deployment-only bootstrap. No seeded/fictitious identity and no ability to
    /// acquire root after the first real user has been committed.
    pub async fn bootstrap_root(
        tx: &DatabaseTransaction,
        email: &str,
        name: Option<&str>,
    ) -> Result<Self, DbError> {
        lock_identity_admin(tx).await?;
        if name.is_some_and(|name| name.len() > 255) {
            return Err(DbError::Other("name exceeds limit".into()));
        }
        let root = Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO users(email,name,platform_role,status) SELECT $1,$2,'root','active' WHERE NOT EXISTS(SELECT 1 FROM users) RETURNING *",
            [normalized_email(email)?.into(), name.into()],
        )).one(tx).await?.ok_or_else(|| DbError::Other("identity bootstrap has already completed".into()))?;
        let audit = AuditContext {
            actor_user_id: root.id,
            credential_kind: CredentialKind::System,
            actor_platform_role: PlatformRole::Root,
            actor_tenant_role: None,
            request_id: None,
        };
        TenantAuditEvent::append(
            tx,
            AuditScopeType::Platform,
            None,
            &audit,
            "identity.bootstrap",
            "user",
            Some(&root.id.to_string()),
            AuditResult::Success,
            serde_json::json!({"platform_role":"root"}),
        )
        .await?;
        Ok(root)
    }

    /// Security changes are separate from profile edits and require a current
    /// root session. Invariants are checked on final transaction state.
    pub async fn set_security(
        tx: &DatabaseTransaction,
        id: Uuid,
        role: PlatformRole,
        status: UserStatus,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        lock_identity_admin(tx).await?;
        let actor = actor.require_root(tx).await?;
        tx.query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM tenants WHERE owner_user_id=$1 ORDER BY id FOR UPDATE",
            [id.into()],
        ))
        .await?;
        let before = Self::find_by_id_for_update(tx, id)
            .await?
            .ok_or_else(|| DbError::not_found("User", id))?;
        let user = Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role=$2,status=$3 WHERE id=$1 RETURNING *",
            [id.into(), role.as_str().into(), status.as_str().into()],
        ))
        .one(tx)
        .await?
        .ok_or_else(|| DbError::not_found("User", id))?;
        TenantAuditEvent::append(tx, AuditScopeType::Platform, None, &actor, "user.security", "user",
            Some(&id.to_string()), AuditResult::Success,
            serde_json::json!({"before":{"platform_role":before.platform_role,"status":before.status},"after":{"platform_role":user.platform_role,"status":user.status}})).await?;
        Ok(user)
    }

    pub async fn find_by_id(db: &impl ConnectionTrait, id: Uuid) -> Result<Option<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM users WHERE id=$1",
            [id.into()],
        ))
        .one(db)
        .await?)
    }
    pub async fn find_by_email(
        db: &impl ConnectionTrait,
        email: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM users WHERE lower(btrim(email))=$1",
            [normalized_email(email)?.into()],
        ))
        .one(db)
        .await?)
    }
    pub async fn find_by_id_for_update(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM users WHERE id=$1 FOR UPDATE",
            [id.into()],
        ))
        .one(db)
        .await?)
    }
    pub async fn find_by_id_for_no_key_update(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM users WHERE id=$1 FOR NO KEY UPDATE",
            [id.into()],
        ))
        .one(db)
        .await?)
    }
    /// Identity initialization only; management lists use checked platform scopes.
    pub async fn count_all(db: &impl ConnectionTrait) -> Result<i64, DbError> {
        let row = db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT COUNT(*) FROM users".to_string(),
            ))
            .await?
            .ok_or_else(|| DbError::Other("identity count returned no row".into()))?;
        Ok(row.try_get_by_index(0)?)
    }
    pub async fn update(
        &self,
        db: &impl ConnectionTrait,
        req: &UpdateUserRequest,
    ) -> Result<Self, DbError> {
        if let Some(name) = &req.name {
            if name.len() > 255 {
                return Err(DbError::Other("name exceeds limit".into()));
            }
            db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE users SET name=$1 WHERE id=$2",
                [name.clone().into(), self.id.into()],
            ))
            .await?;
        }
        Self::find_by_id(db, self.id)
            .await?
            .ok_or_else(|| DbError::not_found("User", self.id))
    }
    pub async fn update_in_tx(
        &self,
        tx: &DatabaseTransaction,
        req: &UpdateUserRequest,
    ) -> Result<Self, DbError> {
        self.update(tx, req).await
    }
    pub async fn increment_token_version(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<i32, DbError> {
        let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE users SET token_version=token_version+1 WHERE id=$1 RETURNING token_version",[id.into()])).await?.ok_or_else(||DbError::not_found("User",id))?;
        Ok(row.try_get_by_index(0)?)
    }
    pub async fn find_token_version(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<i32>, DbError> {
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT token_version FROM users WHERE id=$1",
                [id.into()],
            ))
            .await?;
        row.map(|row| row.try_get_by_index(0).map_err(DbError::from))
            .transpose()
    }
    pub async fn delete(&self, db: &impl ConnectionTrait) -> Result<(), DbError> {
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM users WHERE id=$1",
            [self.id.into()],
        ))
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn global_identity_requests_reject_legacy_privilege_fields() {
        assert!(
            serde_json::from_value::<CreateUserRequest>(
                serde_json::json!({"email":"a@b.invalid","role":"root"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<UpdateUserRequest>(
                serde_json::json!({"tenant_id":Uuid::new_v4()})
            )
            .is_err()
        );
    }
    #[test]
    fn email_is_canonical_bounded_and_has_no_controls() {
        assert_eq!(normalized_email(" A@B.invalid ").unwrap(), "a@b.invalid");
        for invalid in ["@x", "x@", "x@@y", "x\n@y", "x y@z"] {
            assert!(normalized_email(invalid).is_err());
        }
        assert!(normalized_email(&format!("{}@x", "a".repeat(256))).is_err());
    }
}

#[path = "user_scope.rs"]
mod scope;
pub use scope::TenantMemberRecord;
