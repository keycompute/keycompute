//! Console payment reads always carry an explicit personal, tenant or platform scope.
use super::*;
use keycompute_types::{PlatformRole, PlatformScope, TenantRole, TenantScope};

#[derive(Clone, Copy)]
enum ReadScope {
    Personal(TenantScope),
    Tenant(TenantScope),
    Platform(PlatformScope),
}

fn current_tenant_actor(require_admin: bool) -> String {
    let role = if require_admin {
        " AND m.tenant_role='admin'"
    } else {
        ""
    };
    format!(
        "EXISTS (SELECT 1 FROM tenant_memberships m JOIN users u ON u.id=m.user_id JOIN tenants t ON t.id=m.tenant_id WHERE m.tenant_id=$1 AND m.user_id=$2 AND m.status='active' AND u.status='active' AND t.status='active'{role})"
    )
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
            predicates.push(current_tenant_actor(false));
        }
        ReadScope::Tenant(scope) => {
            if scope.tenant_role() != TenantRole::Admin {
                return Err(DbError::Other("tenant administrator scope required".into()));
            }
            predicates.push("tenant_id=$1".to_string());
            values.extend([scope.tenant_id().into(), scope.user_id().into()]);
            predicates.push(current_tenant_actor(true));
        }
        ReadScope::Platform(scope) => {
            // Operator may consume separate aggregate diagnostics, not raw
            // individual orders, provider payloads or payment credentials.
            if scope.platform_role() != PlatformRole::Root {
                return Err(DbError::Other("platform billing scope required".into()));
            }
            values.push(scope.user_id().into());
            predicates.push("EXISTS (SELECT 1 FROM users actor WHERE actor.id=$1 AND actor.status='active' AND actor.platform_role='root')".into());
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
            assert!(sql.starts_with("tenant_id=$1 AND user_id=$2 AND "));
            assert!(sql.contains(&current_tenant_actor(false)));
            assert!(sql.ends_with(" AND status=$3"));
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

/// Tenant reporting never fetches provider credentials, capability URLs or payloads.
#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct PaymentOrderReportRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub amount: Decimal,
    pub currency: String,
    pub status: String,
    pub payment_method: String,
    pub payment_scene: String,
    pub paid_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub expired_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
const REPORT_COLUMNS: &str = "id,tenant_id,user_id,amount,currency,status,payment_method,payment_scene,paid_at,closed_at,expired_at,created_at,updated_at";

impl PaymentOrder {
    pub async fn find_report_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        id: Uuid,
    ) -> Result<Option<PaymentOrderReportRow>, DbError> {
        let (predicate, mut values) = predicate(ReadScope::Tenant(scope), None, None)?;
        values.push(id.into());
        Ok(
            PaymentOrderReportRow::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT {REPORT_COLUMNS} FROM payment_orders WHERE {predicate} AND id=${}",
                    values.len()
                ),
                values,
            ))
            .one(db)
            .await?,
        )
    }
    pub async fn list_report_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        status: Option<&str>,
        owner: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PaymentOrderReportRow>, DbError> {
        let (predicate, mut values) = predicate(ReadScope::Tenant(scope), status, owner)?;
        let index = values.len() + 1;
        values.extend([limit.clamp(1, 100).into(), offset.max(0).into()]);
        Ok(PaymentOrderReportRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT {REPORT_COLUMNS} FROM payment_orders WHERE {predicate} ORDER BY created_at DESC,id DESC LIMIT ${index} OFFSET ${}",index+1),values,
        )).all(db).await?)
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;
    #[test]
    fn metadata_projection_and_scope_never_include_payment_secrets() {
        for column in [
            "pay_url",
            "notify_data",
            "provider_payload",
            "body",
            "remarks",
            "last_error_message",
        ] {
            assert!(!REPORT_COLUMNS.split(',').any(|actual| actual == column));
        }
        let scope =
            TenantScope::checked(Uuid::new_v4(), Uuid::new_v4(), TenantRole::Admin).unwrap();
        let (sql, values) =
            predicate(ReadScope::Tenant(scope), Some("paid"), Some(Uuid::new_v4())).unwrap();
        assert!(sql.contains("m.tenant_role='admin'"));
        assert!(sql.contains("u.status='active'"));
        assert_eq!(values.len(), 4);
    }
}
