//! Current signed console provenance, independent of resource/action grants.
//! Callers must still authorize the action and use scoped DAOs. No bearer material
//! is retained, no platform permission is inferred from a selected membership.
use crate::{
    error::{ApiError, Result},
    extractors::ConsoleAuth,
    state::AppState,
};
use chrono::Utc;
use keycompute_auth::AuthContext;
use keycompute_types::{CredentialKind, PlatformRole, TenantRole};
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, DbErr, Statement, TransactionTrait,
};
use uuid::Uuid;

#[derive(Clone, Copy)]
struct SelectedMembership {
    tenant: Uuid,
    role: TenantRole,
    tenant_version: i64,
    member_version: i64,
}
#[derive(Clone, Copy)]
pub(crate) struct ConsoleSessionProof {
    user: Uuid,
    platform_role: PlatformRole,
    token_version: i32,
    expires: i64,
    selected: Option<SelectedMembership>,
}
fn denied() -> ApiError {
    ApiError::Auth("The original console session is no longer authorized".into())
}
fn storage(_: DbErr) -> ApiError {
    ApiError::ServiceUnavailable("Console authorization storage is unavailable".into())
}
impl ConsoleSessionProof {
    pub(crate) fn from_context(ctx: &AuthContext) -> Result<Self> {
        if ctx.credential_kind != CredentialKind::Jwt
            || ctx.user_id.is_nil()
            || ctx.token_version < 0
        {
            return Err(denied());
        }
        let selected = match (
            ctx.selected_tenant_id,
            ctx.tenant_role,
            ctx.authz_version,
            ctx.membership_authz_version,
        ) {
            (None, None, None, None) => None,
            (Some(tenant), Some(role), Some(tenant_version), Some(member_version))
                if !tenant.is_nil() && tenant_version > 0 && member_version > 0 =>
            {
                Some(SelectedMembership {
                    tenant,
                    role,
                    tenant_version,
                    member_version,
                })
            }
            _ => return Err(denied()),
        };
        let proof = Self {
            user: ctx.user_id,
            platform_role: ctx.platform_role,
            token_version: ctx.token_version,
            expires: ctx.credential_expires_at.ok_or_else(denied)?,
            selected,
        };
        proof.check_deadline()?;
        Ok(proof)
    }
    pub(crate) fn from_console(auth: &ConsoleAuth) -> Result<Self> {
        Self::from_context(&auth.authorization_context())
    }
    pub(crate) fn check_deadline(&self) -> Result<()> {
        if self.expires <= Utc::now().timestamp() {
            return Err(denied());
        }
        Ok(())
    }
    /// Final bounded writer lookup. Existing mutation DAOs retain their own
    /// deterministic authorization locks; this adds no global exclusive lock.
    pub(crate) async fn verify_current(&self, db: &impl ConnectionTrait) -> Result<()> {
        self.check_deadline()?;
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT EXISTS(SELECT 1 FROM users u WHERE u.id=$1 AND u.status='active'
             AND u.token_version=$2 AND u.platform_role=$3
             AND ($4::uuid IS NULL OR EXISTS(
                SELECT 1 FROM tenants t JOIN tenant_memberships m ON m.tenant_id=t.id
                WHERE t.id=$4 AND t.status='active' AND t.authz_version=$5
                  AND m.user_id=u.id AND m.status='active' AND m.authz_version=$6
                  AND m.tenant_role=$7
             ))) AS authorized",
                [
                    self.user.into(),
                    self.token_version.into(),
                    self.platform_role.as_str().into(),
                    self.selected.map(|m| m.tenant).into(),
                    self.selected.map(|m| m.tenant_version).into(),
                    self.selected.map(|m| m.member_version).into(),
                    self.selected.map(|m| m.role.as_str()).into(),
                ],
            ))
            .await
            .map_err(storage)?
            .ok_or_else(denied)?;
        if !row.try_get::<bool>("", "authorized").map_err(storage)? {
            return Err(denied());
        }
        self.check_deadline()
    }
    pub(crate) async fn begin(&self, state: &AppState) -> Result<DatabaseTransaction> {
        self.check_deadline()?;
        let tx = state
            .pool
            .as_deref()
            .ok_or_else(|| ApiError::ServiceUnavailable("Console storage unavailable".into()))?
            .begin()
            .await
            .map_err(storage)?;
        self.check_deadline()?;
        Ok(tx)
    }
    /// Return only a currently authorized transaction. On rejection all nested
    /// DAO savepoints, state changes and audits are rolled back together.
    pub(crate) async fn prepare_commit(
        &self,
        tx: DatabaseTransaction,
    ) -> Result<DatabaseTransaction> {
        if let Err(error) = self.verify_current(&tx).await {
            tx.rollback().await.map_err(storage)?;
            return Err(error);
        }
        Ok(tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn global() -> AuthContext {
        let mut ctx = AuthContext::global(Uuid::new_v4());
        ctx.credential_expires_at = Some(Utc::now().timestamp() + 600);
        ctx
    }
    #[test]
    fn proof_requires_an_expiring_console_credential_and_a_complete_selected_tuple() {
        let ctx = global();
        assert!(ConsoleSessionProof::from_context(&ctx).is_ok());
        let mut selected = ctx.clone();
        selected.selected_tenant_id = Some(Uuid::new_v4());
        assert!(ConsoleSessionProof::from_context(&selected).is_err());
        selected.tenant_role = Some(TenantRole::Admin);
        selected.authz_version = Some(1);
        selected.membership_authz_version = Some(2);
        assert!(ConsoleSessionProof::from_context(&selected).is_ok());
        for field in [
            "expiry",
            "expired",
            "kind",
            "user",
            "version",
            "tenant",
            "membership",
        ] {
            let mut bad = selected.clone();
            match field {
                "expiry" => bad.credential_expires_at = None,
                "expired" => bad.credential_expires_at = Some(Utc::now().timestamp()),
                "kind" => bad.credential_kind = CredentialKind::ApiKey,
                "user" => bad.user_id = Uuid::nil(),
                "version" => bad.token_version = -1,
                "tenant" => bad.selected_tenant_id = Some(Uuid::nil()),
                "membership" => bad.membership_authz_version = Some(0),
                _ => unreachable!(),
            }
            assert!(ConsoleSessionProof::from_context(&bad).is_err(), "{field}");
        }
    }
    #[test]
    fn session_provenance_never_grants_platform_or_tenant_actions() {
        let ctx = global();
        let proof = ConsoleSessionProof::from_context(&ctx).unwrap();
        assert_eq!(proof.platform_role, PlatformRole::None);
        assert!(proof.selected.is_none());
        assert!(
            ctx.require_platform(keycompute_auth::AuthorizationAction::ManagePlatform)
                .is_err()
        );
        let mut malformed = ctx;
        malformed.tenant_role = Some(TenantRole::Admin);
        assert!(ConsoleSessionProof::from_context(&malformed).is_err());
    }
}
