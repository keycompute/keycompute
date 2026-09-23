//! Original console-session proof for tenant and personal key-control transactions.
//! This grants no resource permission: the caller's scoped DAO remains authoritative.
use crate::{
    error::{ApiError, Result},
    extractors::ConsoleAuth,
    state::AppState,
};
use chrono::Utc;
use keycompute_types::CredentialKind;
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, DbErr, Statement, TransactionTrait,
};

fn denied() -> ApiError {
    ApiError::Auth("Key control requires the original current console authorization".into())
}
fn storage(_: DbErr) -> ApiError {
    ApiError::ServiceUnavailable("Key control authorization storage is unavailable".into())
}
fn require_credential(auth: &ConsoleAuth) -> Result<()> {
    if auth.credential_kind != CredentialKind::Jwt
        || auth
            .credential_expires_at
            .is_none_or(|expires| expires <= Utc::now().timestamp())
        || auth.user_id.is_nil()
        || auth.tenant_id.is_nil()
        || auth.token_version < 0
        || auth.authz_version <= 0
        || auth.membership_authz_version <= 0
        || auth.tenant_role.is_none()
    {
        return Err(denied());
    }
    Ok(())
}

/// Compare the original version tuple, not merely today's active status/role.
/// Scoped write DAOs retain their shared/exclusive authorization locks in the
/// outer transaction; this check does not acquire a global exclusive fence.
pub(crate) async fn finish_read(db: &impl ConnectionTrait, auth: &ConsoleAuth) -> Result<()> {
    require_credential(auth)?;
    let role = auth.tenant_role.ok_or_else(denied)?;
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT EXISTS(SELECT 1 FROM users u
         JOIN tenant_memberships m ON m.user_id=u.id
         JOIN tenants t ON t.id=m.tenant_id
         WHERE u.id=$1 AND t.id=$2 AND u.status='active' AND t.status='active'
           AND m.status='active' AND u.token_version=$3 AND t.authz_version=$4
           AND m.authz_version=$5 AND m.tenant_role=$6 AND u.platform_role=$7) AS authorized",
            [
                auth.user_id.into(),
                auth.tenant_id.into(),
                auth.token_version.into(),
                auth.authz_version.into(),
                auth.membership_authz_version.into(),
                role.as_str().into(),
                auth.platform_role.as_str().into(),
            ],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(denied)?;
    if !row.try_get::<bool>("", "authorized").map_err(storage)? {
        return Err(denied());
    }
    require_credential(auth)
}

pub(crate) async fn begin(state: &AppState, auth: &ConsoleAuth) -> Result<DatabaseTransaction> {
    require_credential(auth)?;
    let tx = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Key storage unavailable".into()))?
        .begin()
        .await
        .map_err(storage)?;
    require_credential(auth)?;
    Ok(tx)
}

#[derive(Clone, Copy)]
pub(crate) enum KeyChange {
    Credential,
    Intent,
}

pub(crate) async fn commit(
    state: &AppState,
    tx: DatabaseTransaction,
    auth: &ConsoleAuth,
    change: KeyChange,
) -> Result<()> {
    // DAO calls are savepoints. Expiry, revocation or a regrant while waiting
    // must roll back all key, issuance and audit effects at this final boundary.
    if let Err(error) = finish_read(&tx, auth).await {
        tx.rollback().await.map_err(storage)?;
        return Err(error);
    }
    // Inert issuance requests/cancellations do not invalidate inference keys.
    let _fence =
        matches!(change, KeyChange::Credential).then(|| state.display_cache.mutation_guard());
    tx.commit().await.map_err(storage)?;
    // Commit acknowledgement may be delayed. Never release a one-time secret
    // after expiry; inspect durable records rather than retrying an uncertain claim.
    require_credential(auth)
}
