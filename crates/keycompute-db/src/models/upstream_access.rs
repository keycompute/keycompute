//! Shared account exposure policy. Only SQL identifiers/placeholders supplied
//! by trusted code are accepted by the predicate builders; values stay bound.
use crate::DbError;
use sea_orm::{ConnectionTrait, DbBackend, Statement};

/// Serialize rare administrative grant/model edits, never generation reads.
/// Acquire before tenant/account row locks to keep writer ordering consistent.
pub async fn lock_configuration(db: &impl ConnectionTrait) -> Result<(), DbError> {
    db.execute(Statement::from_string(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(1262694450)".to_string(),
    ))
    .await?;
    Ok(())
}

/// Account-pool visibility is derived exclusively from ownership. Platform
/// accounts (`tenant_id IS NULL`) are shared; tenant accounts are private.
/// Passthrough bindings configure passthrough access and never widen this pool.
pub fn non_pt_predicate(account: &str, tenant: &str) -> String {
    format!(
        r#"(
        {account}.enabled
        AND {account}.pool_enabled
        AND EXISTS (SELECT 1 FROM tenants access_caller WHERE access_caller.id={tenant} AND access_caller.status='active')
        AND (
          {account}.tenant_id IS NULL
          OR ({account}.tenant_id={tenant} AND EXISTS (
            SELECT 1 FROM tenants access_owner
            WHERE access_owner.id={account}.tenant_id AND access_owner.status='active'
          ))
        )
    )"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pool_policy_is_derived_from_account_ownership() {
        let sql = non_pt_predicate("a", "$1");
        assert!(sql.contains("a.tenant_id IS NULL"));
        assert!(sql.contains("a.tenant_id=$1"));
        assert!(sql.contains("a.pool_enabled"));
        assert!(!sql.contains("passthrough_bindings"));
        assert!(!sql.contains("visibility"));
    }
}
