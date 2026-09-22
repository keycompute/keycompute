//! Read-only wallet projections. No initialization, lock, reclaim or financial mutation.
//! Authorization-bearing reads must use the primary; DbRouter callers pass write_conn().
use super::{DisplayBalanceRow, UserBalance, UserBalanceDisplaySnapshot};
use crate::DbError;
use keycompute_types::{PlatformRole, PlatformScope, TenantRole, TenantScope};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Clone, Copy)]
enum Scope {
    Owned(TenantScope),
    Tenant(TenantScope),
    Platform(PlatformScope, Uuid),
}
fn denied() -> DbError {
    DbError::Other("balance snapshot authorization denied".into())
}
impl Scope {
    fn identity(self) -> Result<(Uuid, Uuid), DbError> {
        match self {
            Self::Owned(scope) => Ok((scope.tenant_id(), scope.user_id())),
            Self::Tenant(scope) if scope.tenant_role() == TenantRole::Admin => {
                Ok((scope.tenant_id(), scope.user_id()))
            }
            Self::Platform(scope, t)
                if scope.platform_role() == PlatformRole::Root && !t.is_nil() =>
            {
                Ok((t, scope.user_id()))
            }
            _ => Err(denied()),
        }
    }
}
fn statement(scope: Scope, owners: Vec<Uuid>) -> Result<Statement, DbError> {
    let (tenant, actor) = scope.identity()?;
    let authority = match scope {
        Scope::Owned(_) => {
            "m.user_id=$2 AND m.status='active' AND u.status='active' AND EXISTS (SELECT 1 FROM tenants t WHERE t.id=$1 AND t.status='active')"
        }
        Scope::Tenant(_) => {
            "EXISTS (SELECT 1 FROM tenant_memberships am JOIN users au ON au.id=am.user_id JOIN tenants at ON at.id=am.tenant_id WHERE am.tenant_id=$1 AND am.user_id=$2 AND am.tenant_role='admin' AND am.status='active' AND au.status='active' AND at.status='active')"
        }
        Scope::Platform(_, _) => {
            "EXISTS (SELECT 1 FROM users au WHERE au.id=$2 AND au.status='active' AND au.platform_role='root')"
        }
    };
    Ok(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "SELECT m.user_id, m.tenant_id AS owner_tenant_id, ub.id AS balance_id,
            ub.tenant_id AS balance_tenant_id, ub.available_balance, ub.frozen_balance,
            ub.total_recharged, ub.total_consumed, statement_timestamp() AS as_of
         FROM tenant_memberships m JOIN users u ON u.id=m.user_id
         LEFT JOIN user_balances ub ON ub.tenant_id=m.tenant_id AND ub.user_id=m.user_id
         WHERE m.tenant_id=$1 AND m.user_id=ANY($3) AND {authority}"
        ),
        [tenant.into(), actor.into(), owners.into()],
    ))
}
async fn snapshots(
    db: &impl ConnectionTrait,
    scope: Scope,
    owners: &[Uuid],
) -> Result<HashMap<Uuid, UserBalanceDisplaySnapshot>, DbError> {
    if owners.len() > 1000 {
        return Err(DbError::Other(
            "display balance batch exceeds 1000 users".into(),
        ));
    }
    let (tenant, _) = scope.identity()?;
    if owners.iter().any(Uuid::is_nil) {
        return Err(denied());
    }
    if owners.is_empty() {
        return Ok(HashMap::new());
    }
    let mut requested = owners.to_vec();
    requested.sort_unstable();
    requested.dedup();
    let rows = DisplayBalanceRow::find_by_statement(statement(scope, requested.clone())?)
        .all(db)
        .await?;
    let mut results = HashMap::with_capacity(rows.len());
    for row in rows {
        let snapshot = row.into_snapshot(tenant)?;
        results.insert(snapshot.user_id, snapshot);
    }
    for owner in requested {
        if !results.contains_key(&owner) {
            return Err(DbError::not_found("wallet owner", owner));
        }
    }
    Ok(results)
}
impl UserBalance {
    /// Administrators do not widen a personal balance query.
    pub async fn find_owned_display_snapshot(
        db: &impl ConnectionTrait,
        scope: TenantScope,
    ) -> Result<UserBalanceDisplaySnapshot, DbError> {
        snapshots(db, Scope::Owned(scope), &[scope.user_id()])
            .await?
            .remove(&scope.user_id())
            .ok_or_else(|| DbError::not_found("wallet owner", scope.user_id()))
    }
    /// Active tenant admins can inspect retained balances of suspended/revoked members.
    /// A missing wallet is zero only for a real member; foreign users never become zero rows.
    pub async fn find_display_snapshots_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        owners: &[Uuid],
    ) -> Result<HashMap<Uuid, UserBalanceDisplaySnapshot>, DbError> {
        snapshots(db, Scope::Tenant(scope), owners).await
    }
    /// Explicit root reporting may inspect an inactive tenant without pretending to be its member.
    pub async fn find_display_snapshots_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        tenant: Uuid,
        owners: &[Uuid],
    ) -> Result<HashMap<Uuid, UserBalanceDisplaySnapshot>, DbError> {
        snapshots(db, Scope::Platform(scope, tenant), owners).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn personal_and_tenant_queries_keep_the_same_wallet_ownership_join() {
        let t = Uuid::new_v4();
        let u = Uuid::new_v4();
        let tenant = TenantScope::checked(t, u, TenantRole::Admin).unwrap();
        for s in [Scope::Owned(tenant), Scope::Tenant(tenant)] {
            let query = statement(s, vec![u]).unwrap();
            assert!(
                query
                    .sql
                    .contains("ub.tenant_id=m.tenant_id AND ub.user_id=m.user_id")
            );
            assert!(query.sql.contains("m.tenant_id=$1 AND m.user_id=ANY($3)"));
            assert!(!query.sql.contains("FOR UPDATE"));
            assert!(!query.sql.contains("FOR SHARE"));
            assert!(!query.sql.contains("INSERT"));
        }
    }
    #[test]
    fn member_operator_and_nil_tenant_do_not_form_management_scopes() {
        let t = Uuid::new_v4();
        let u = Uuid::new_v4();
        let member = TenantScope::checked(t, u, TenantRole::Member).unwrap();
        assert!(statement(Scope::Tenant(member), vec![u]).is_err());
        let operator = PlatformScope::checked(u, PlatformRole::Operator).unwrap();
        assert!(statement(Scope::Platform(operator, t), vec![u]).is_err());
        let root = PlatformScope::checked(u, PlatformRole::Root).unwrap();
        assert!(statement(Scope::Platform(root, Uuid::nil()), vec![u]).is_err());
    }
}
