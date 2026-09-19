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

/// Binding grants replace legacy visibility whenever ANY binding exists.
/// Even an inactive grant suppresses fallback, so revocation cannot reopen it.
pub fn non_pt_predicate(account: &str, tenant: &str) -> String {
    format!(
        r#"(
        {account}.enabled
        AND EXISTS (SELECT 1 FROM tenants access_owner WHERE access_owner.id={account}.tenant_id AND access_owner.status='active')
        AND EXISTS (SELECT 1 FROM tenants access_caller WHERE access_caller.id={tenant} AND access_caller.status='active')
        AND (
          (NOT EXISTS (SELECT 1 FROM passthrough_bindings access_any WHERE access_any.account_id={account}.id)
            AND {account}.pool_enabled AND ({account}.tenant_id={tenant} OR {account}.visibility='global'))
          OR EXISTS (
            SELECT 1 FROM passthrough_bindings access_grant
            JOIN tenants access_anchor ON access_anchor.id=access_grant.tenant_id AND access_anchor.status='active'
            WHERE access_grant.account_id={account}.id AND access_grant.pool_enabled
              AND (access_grant.tenant_id={tenant} OR access_grant.is_global)
          )
        )
    )"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grant_policy_cannot_fall_back_to_account_visibility() {
        let sql = non_pt_predicate("a", "$1");
        assert!(sql.contains("NOT EXISTS"));
        assert!(sql.contains("access_grant.pool_enabled"));
        assert!(sql.contains("access_anchor.status='active'"));
        assert!(sql.contains("a.tenant_id=$1 OR a.visibility='global'"));
    }
}
