//! Hash-only invitations with serialized, one-time consumption.
use super::tenant_audit_event::{AuditContext, TenantAuditEvent, lock_identity_admin};
use super::user::normalized_email;
use crate::DbError;
use chrono::{DateTime, Utc};
use keycompute_types::{
    AuditResult, AuditScopeType, CredentialKind, TenantInvitationStatus, TenantRole, TenantScope,
};
use rand::{RngCore, rngs::OsRng};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, FromQueryResult, Serialize)]
pub struct TenantInvitation {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub invited_by_user_id: Uuid,
    pub email: String,
    pub role: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub accepted_by_user_id: Option<Uuid>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
impl std::fmt::Debug for TenantInvitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TenantInvitation")
            .field("id", &self.id)
            .field("tenant_id", &self.tenant_id)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTenantInvitationRequest {
    pub tenant_id: Uuid,
    pub invited_by_user_id: Uuid,
    pub email: String,
    pub role: TenantRole,
    pub expires_at: DateTime<Utc>,
}
#[derive(Clone)]
pub struct CreatedTenantInvitation {
    pub invitation: TenantInvitation,
    /// Returned on creation only, never in audit or Debug output.
    pub token: Option<String>,
}
impl std::fmt::Debug for CreatedTenantInvitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedTenantInvitation")
            .field("invitation", &self.invitation)
            .field("token", &self.token.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}
fn invalid() -> DbError {
    DbError::Other("invitation is unavailable, expired or already used".into())
}
fn token_hash(token: &str) -> Option<String> {
    (token.len() == 64 && token.bytes().all(|c| c.is_ascii_hexdigit()))
        .then(|| hex::encode(Sha256::digest(token.as_bytes())))
}
impl TenantInvitation {
    pub fn status(&self) -> Result<TenantInvitationStatus, DbError> {
        self.status.parse().map_err(DbError::Other)
    }
    pub fn role(&self) -> Result<TenantRole, DbError> {
        self.role.parse().map_err(DbError::Other)
    }
    pub async fn create(
        tx: &DatabaseTransaction,
        req: &CreateTenantInvitationRequest,
        actor: &AuditContext,
    ) -> Result<CreatedTenantInvitation, DbError> {
        lock_identity_admin(tx).await?;
        let actor = actor.require_tenant_admin(tx, req.tenant_id).await?;
        if actor.actor_user_id != req.invited_by_user_id {
            return Err(DbError::Other("invitation actor mismatch".into()));
        }
        let email = normalized_email(&req.email)?;
        let valid=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT $1::timestamptz > clock_timestamp() AND $1 <= clock_timestamp()+INTERVAL '7 days' AS valid",
            [req.expires_at.into()],
        )).await?.ok_or_else(invalid)?.try_get::<bool>("","valid")?;
        if !valid {
            return Err(DbError::Other(
                "invitation expiry must be within seven days".into(),
            ));
        }
        Self::expire_locked(tx, req.tenant_id, &actor).await?;
        if let Some(existing)=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT * FROM tenant_invitations WHERE tenant_id=$1 AND email=$2 AND status='pending' FOR UPDATE",
            [req.tenant_id.into(),email.clone().into()],
        )).one(tx).await? {
            if existing.role!=req.role.as_str() {
                return Err(DbError::Other("a pending invitation already exists with a different role".into()));
            }
            return Ok(CreatedTenantInvitation { invitation:existing, token:None });
        }
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let token = hex::encode(bytes);
        let hash = hex::encode(Sha256::digest(token.as_bytes()));
        let invitation=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO tenant_invitations(tenant_id,invited_by_user_id,email,role,token_hash,expires_at) VALUES($1,$2,$3,$4,$5,$6) RETURNING *",
            [req.tenant_id.into(),actor.actor_user_id.into(),email.into(),req.role.as_str().into(),hash.into(),req.expires_at.into()],
        )).one(tx).await?.ok_or_else(invalid)?;
        TenantAuditEvent::append(
            tx,
            AuditScopeType::Tenant,
            Some(req.tenant_id),
            &actor,
            "invitation.create",
            "tenant_invitation",
            Some(&invitation.id.to_string()),
            AuditResult::Success,
            serde_json::json!({"email":invitation.email,"role":invitation.role}),
        )
        .await?;
        Ok(CreatedTenantInvitation {
            invitation,
            token: Some(token),
        })
    }
    pub async fn find_by_token_hash(
        db: &impl ConnectionTrait,
        tenant: Uuid,
        hash: &str,
    ) -> Result<Option<Self>, DbError> {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(None);
        }
        Ok(Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT * FROM tenant_invitations WHERE tenant_id=$1 AND token_hash=$2 AND status='pending' AND expires_at>clock_timestamp()",
            [tenant.into(),hash.into()],
        )).one(db).await?)
    }
    /// A global session may accept without already belonging to the tenant.
    /// The supplied email must also match the database's verified credential.
    pub async fn accept(
        tx: &DatabaseTransaction,
        tenant: Uuid,
        token: &str,
        user: Uuid,
        _verified_email: &str,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        if actor.credential_kind != CredentialKind::Jwt
            || actor.actor_user_id != user
            || user.is_nil()
        {
            return Err(invalid());
        }
        let hash = token_hash(token).ok_or_else(invalid)?;
        lock_identity_admin(tx).await?;
        super::tenant::Tenant::find_by_id_for_update(tx, tenant)
            .await?
            .filter(|tenant| tenant.is_active())
            .ok_or_else(invalid)?;
        let invitation = Self::find_by_token_hash(tx, tenant, &hash)
            .await?
            .ok_or_else(invalid)?;
        let mut users = vec![user, invitation.invited_by_user_id];
        users.sort_unstable();
        users.dedup();
        tx.query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM users WHERE id=ANY($1) ORDER BY id FOR SHARE",
            [users.into()],
        ))
        .await?;
        let current = super::user::User::find_by_id(tx, user)
            .await?
            .filter(|user| user.status == "active")
            .ok_or_else(invalid)?;
        let verified = tx
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 FROM user_credentials WHERE user_id=$1 AND email_verified=TRUE FOR SHARE",
                [user.into()],
            ))
            .await?
            .is_some();
        // The authenticated request may carry an email claim for display, but
        // it is never an authority input.  Compare the invitation with the
        // current canonical address read from the locked global user row.
        let current_email = normalized_email(&current.email)?;
        if !verified || current_email != invitation.email {
            return Err(invalid());
        }
        super::tenant_membership::TenantMembership::find(tx, tenant, invitation.invited_by_user_id)
            .await?
            .filter(|member| member.status == "active" && member.role == "admin")
            .ok_or_else(invalid)?;
        let existing =
            super::tenant_membership::TenantMembership::find_any(tx, tenant, user).await?;
        if let Some(existing) = &existing
            && existing.status == "active"
            && existing.role != invitation.role
        {
            return Err(DbError::Other(
                "existing member role must be changed through member administration".into(),
            ));
        }
        // Use the wall clock again after lock waits, not transaction-start NOW().
        let accepted=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE tenant_invitations SET status='accepted',accepted_by_user_id=$3,accepted_at=clock_timestamp(),updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 AND token_hash=$4 AND status='pending' AND expires_at>clock_timestamp() RETURNING *",
            [tenant.into(),invitation.id.into(),user.into(),hash.into()],
        )).one(tx).await?.ok_or_else(invalid)?;
        if existing
            .as_ref()
            .is_none_or(|member| member.status != "active")
        {
            tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "INSERT INTO tenant_memberships(tenant_id,user_id,role,status) VALUES($1,$2,$3,'active') ON CONFLICT(tenant_id,user_id) DO UPDATE SET role=EXCLUDED.role,status='active' WHERE tenant_memberships.status<>'active'",
                [tenant.into(),user.into(),invitation.role.clone().into()],
            )).await?;
        }
        let actor = AuditContext {
            actor_platform_role: current.platform_role()?,
            actor_tenant_role: Some(invitation.role()?),
            ..*actor
        };
        TenantAuditEvent::append(
            tx,
            AuditScopeType::Tenant,
            Some(tenant),
            &actor,
            "invitation.accept",
            "tenant_invitation",
            Some(&invitation.id.to_string()),
            AuditResult::Success,
            serde_json::json!({"user_id":user,"role":invitation.role}),
        )
        .await?;
        TenantAuditEvent::append(
            tx,
            AuditScopeType::Tenant,
            Some(tenant),
            &actor,
            "membership.join",
            "tenant_membership",
            Some(&user.to_string()),
            AuditResult::Success,
            serde_json::json!({"role":accepted.role}),
        )
        .await?;
        Ok(accepted)
    }
    pub async fn revoke(
        tx: &DatabaseTransaction,
        tenant: Uuid,
        id: Uuid,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        lock_identity_admin(tx).await?;
        let actor = actor.require_tenant_admin(tx, tenant).await?;
        let existing = Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM tenant_invitations WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
            [tenant.into(), id.into()],
        ))
        .one(tx)
        .await?
        .ok_or_else(invalid)?;
        if existing.status == "revoked" {
            return Ok(existing);
        }
        if existing.status != "pending" {
            return Err(invalid());
        }
        let revoked=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE tenant_invitations SET status='revoked',revoked_at=clock_timestamp(),updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 AND status='pending' RETURNING *",
            [tenant.into(),id.into()],
        )).one(tx).await?.ok_or_else(invalid)?;
        TenantAuditEvent::append(
            tx,
            AuditScopeType::Tenant,
            Some(tenant),
            &actor,
            "invitation.revoke",
            "tenant_invitation",
            Some(&id.to_string()),
            AuditResult::Success,
            serde_json::json!({}),
        )
        .await?;
        Ok(revoked)
    }
    async fn expire_locked(
        tx: &DatabaseTransaction,
        tenant: Uuid,
        actor: &AuditContext,
    ) -> Result<u64, DbError> {
        let expired=tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE tenant_invitations SET status='expired',updated_at=clock_timestamp() WHERE tenant_id=$1 AND status='pending' AND expires_at<=clock_timestamp()",[tenant.into()],
        )).await?.rows_affected();
        if expired > 0 {
            TenantAuditEvent::append(
                tx,
                AuditScopeType::Tenant,
                Some(tenant),
                actor,
                "invitation.expire",
                "tenant_invitation",
                None,
                AuditResult::Success,
                serde_json::json!({"count":expired}),
            )
            .await?;
        }
        Ok(expired)
    }
    pub async fn expire_pending(
        tx: &DatabaseTransaction,
        tenant: Uuid,
        actor: &AuditContext,
    ) -> Result<u64, DbError> {
        lock_identity_admin(tx).await?;
        let actor = actor.require_tenant_admin(tx, tenant).await?;
        Self::expire_locked(tx, tenant, &actor).await
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
            "SELECT * FROM tenant_invitations WHERE tenant_id=$1 ORDER BY created_at DESC,id DESC LIMIT $2 OFFSET $3",
            [scope.tenant_id().into(),limit.clamp(1,100).into(),offset.max(0).into()],
        )).all(db).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens_have_fixed_format_and_hash() {
        assert!(token_hash("short").is_none());
        assert!(token_hash(&"x".repeat(64)).is_none());
        assert_eq!(token_hash(&"a".repeat(64)).unwrap().len(), 64);
    }
    #[test]
    fn debug_never_exposes_the_raw_token() {
        let now = Utc::now();
        let invitation = TenantInvitation {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            invited_by_user_id: Uuid::new_v4(),
            email: "a@b.invalid".into(),
            role: "member".into(),
            status: "pending".into(),
            expires_at: now,
            accepted_by_user_id: None,
            accepted_at: None,
            revoked_at: None,
            created_at: now,
            updated_at: now,
        };
        let created = CreatedTenantInvitation {
            invitation,
            token: Some("usable-secret-token".into()),
        };
        assert!(!format!("{created:?}").contains("secret"));
        assert!(
            !serde_json::to_string(&created.invitation)
                .unwrap()
                .contains("secret")
        );
    }
}
