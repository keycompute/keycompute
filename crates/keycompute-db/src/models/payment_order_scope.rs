//! Console payment reads always carry an explicit personal, tenant or platform scope.
use super::*;
use keycompute_types::{PlatformRole, PlatformScope, TenantRole, TenantScope};

#[derive(Clone, Copy)]
enum ReadScope {
    Personal(TenantScope),
    Tenant(TenantScope),
    Platform(PlatformScope),
}

/// One predicate builder for detail, list, count and aggregate queries.
/// Table/column expressions below are internal constants, never request data.
fn predicate(
    scope: ReadScope,
    status: Option<&str>,
    user: Option<Uuid>,
) -> Result<(String, Vec<sea_orm::Value>), DbError> {
    let mut predicates = Vec::new();
    let mut values = Vec::new();
    match scope {
        ReadScope::Personal(scope) => {
            predicates.push("tenant_id=$1".to_string());
            predicates.push("user_id=$2".to_string());
            values.extend([scope.tenant_id().into(), scope.user_id().into()]);
        }
        ReadScope::Tenant(scope) => {
            if scope.tenant_role() != TenantRole::Admin {
                return Err(DbError::Other("tenant administrator scope required".into()));
            }
            predicates.push("tenant_id=$1".to_string());
            values.push(scope.tenant_id().into());
        }
        ReadScope::Platform(scope) => {
            // Operator may consume separate aggregate diagnostics, not raw
            // individual orders, provider payloads or payment credentials.
            if scope.platform_role() != PlatformRole::Root {
                return Err(DbError::Other("platform billing scope required".into()));
            }
        }
    }
    if let Some(status) = status {
        values.push(status.into());
        predicates.push(format!("status=${}", values.len()));
    }
    if let Some(user) = user {
        values.push(user.into());
        predicates.push(format!("user_id=${}", values.len()));
    }
    let sql = if predicates.is_empty() {
        "TRUE".into()
    } else {
        predicates.join(" AND ")
    };
    Ok((sql, values))
}

async fn find(
    db: &impl ConnectionTrait,
    scope: ReadScope,
    column: &'static str,
    value: sea_orm::Value,
) -> Result<Option<PaymentOrder>, DbError> {
    let (predicate, mut values) = predicate(scope, None, None)?;
    values.push(value);
    let sql = format!(
        "SELECT * FROM payment_orders WHERE {predicate} AND {column}=${}",
        values.len()
    );
    Ok(
        PaymentOrder::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .one(db)
        .await?,
    )
}

async fn list(
    db: &impl ConnectionTrait,
    scope: ReadScope,
    status: Option<&str>,
    user: Option<Uuid>,
    limit: i64,
    offset: i64,
) -> Result<Vec<PaymentOrder>, DbError> {
    let (predicate, mut values) = predicate(scope, status, user)?;
    let limit_index = values.len() + 1;
    let offset_index = limit_index + 1;
    values.extend([limit.clamp(1, 1000).into(), offset.max(0).into()]);
    let sql = format!(
        "SELECT * FROM payment_orders WHERE {predicate} ORDER BY created_at DESC,id DESC LIMIT ${limit_index} OFFSET ${offset_index}"
    );
    Ok(
        PaymentOrder::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .all(db)
        .await?,
    )
}

async fn count(
    db: &impl ConnectionTrait,
    scope: ReadScope,
    status: Option<&str>,
    user: Option<Uuid>,
) -> Result<i64, DbError> {
    let (predicate, values) = predicate(scope, status, user)?;
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT COUNT(*)::bigint AS total FROM payment_orders WHERE {predicate}"),
            values,
        ))
        .await?
        .ok_or_else(|| DbError::Other("payment count returned no row".into()))?;
    Ok(row.try_get("", "total")?)
}

impl PaymentOrder {
    /// Personal endpoints never widen, including for a tenant administrator.
    pub async fn find_owned(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        find(db, ReadScope::Personal(scope), "id", id.into()).await
    }
    pub async fn find_owned_by_out_trade_no(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        reference: &str,
    ) -> Result<Option<Self>, DbError> {
        find(
            db,
            ReadScope::Personal(scope),
            "out_trade_no",
            reference.into(),
        )
        .await
    }
    pub async fn list_owned(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        status: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        list(db, ReadScope::Personal(scope), status, None, limit, offset).await
    }
    pub async fn count_owned(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        status: Option<&str>,
    ) -> Result<i64, DbError> {
        count(db, ReadScope::Personal(scope), status, None).await
    }
    pub async fn find_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        find(db, ReadScope::Tenant(scope), "id", id.into()).await
    }
    pub async fn list_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        status: Option<&str>,
        user: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        list(db, ReadScope::Tenant(scope), status, user, limit, offset).await
    }
    pub async fn count_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        status: Option<&str>,
        user: Option<Uuid>,
    ) -> Result<i64, DbError> {
        count(db, ReadScope::Tenant(scope), status, user).await
    }
    pub async fn find_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        find(db, ReadScope::Platform(scope), "id", id.into()).await
    }
    pub async fn list_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        status: Option<&str>,
        user: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        list(db, ReadScope::Platform(scope), status, user, limit, offset).await
    }
    pub async fn count_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        status: Option<&str>,
        user: Option<Uuid>,
    ) -> Result<i64, DbError> {
        count(db, ReadScope::Platform(scope), status, user).await
    }
    pub async fn stats_owned(
        db: &impl ConnectionTrait,
        scope: TenantScope,
    ) -> Result<PaymentOrderStats, DbError> {
        let (predicate, values) = predicate(ReadScope::Personal(scope), None, None)?;
        let sql = format!(
            "SELECT COUNT(*) AS total_orders, COALESCE(SUM(amount),0) AS total_amount, COUNT(*) FILTER (WHERE status='paid') AS paid_orders, COALESCE(SUM(amount) FILTER (WHERE status='paid'),0) AS paid_amount, COUNT(*) FILTER (WHERE status='pending') AS pending_orders, COALESCE(SUM(amount) FILTER (WHERE status='pending'),0) AS pending_amount FROM payment_orders WHERE {predicate}"
        );
        PaymentOrderStats::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .one(db)
        .await?
        .ok_or_else(|| DbError::Other("payment stats returned no row".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn personal_predicate_retains_owner_even_for_admin() {
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        for role in [TenantRole::Admin, TenantRole::Member] {
            let scope = TenantScope::checked(tenant, user, role).unwrap();
            let (sql, values) = predicate(ReadScope::Personal(scope), Some("paid"), None).unwrap();
            assert_eq!(sql, "tenant_id=$1 AND user_id=$2 AND status=$3");
            assert_eq!(values, vec![tenant.into(), user.into(), "paid".into()]);
        }
    }
    #[test]
    fn tenant_member_and_platform_operator_cannot_request_all_orders() {
        let user = Uuid::new_v4();
        let tenant = TenantScope::checked(Uuid::new_v4(), user, TenantRole::Member).unwrap();
        assert!(predicate(ReadScope::Tenant(tenant), None, None).is_err());
        let operator = PlatformScope::checked(user, PlatformRole::Operator).unwrap();
        assert!(predicate(ReadScope::Platform(operator), None, None).is_err());
    }
}
