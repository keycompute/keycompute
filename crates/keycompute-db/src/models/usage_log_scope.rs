//! Explicit read scopes. These never confer authority to write or settle a ledger.
use super::*;
use keycompute_types::{PlatformScope, TenantRole, TenantScope};

#[derive(Debug, Clone, Copy)]
enum ReadScope {
    Personal(TenantScope),
    Tenant(TenantScope),
    Platform(PlatformScope),
}

#[derive(Default, Clone, Copy)]
struct Window {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
}
impl Window {
    fn new(from: Option<DateTime<Utc>>, to: Option<DateTime<Utc>>) -> Self {
        Self { from, to }
    }
}

/// Stable, secret-free tenant reporting projection, independent of future ledger fields.
#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct UsageLogReportRow {
    pub id: Uuid,
    pub request_id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub produce_ai_key_id: Uuid,
    pub model_name: String,
    pub provider_name: String,
    pub account_id: Uuid,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub total_tokens: i32,
    pub input_unit_price_snapshot: BigDecimal,
    pub output_unit_price_snapshot: BigDecimal,
    pub user_amount: BigDecimal,
    pub currency: String,
    pub usage_source: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}
const REPORT_COLUMNS: &str = "l.id,l.request_id,l.tenant_id,l.user_id,l.produce_ai_key_id,l.model_name,l.provider_name,l.account_id,l.input_tokens,l.output_tokens,l.total_tokens,l.input_unit_price_snapshot,l.output_unit_price_snapshot,l.user_amount,l.currency,l.usage_source,l.status,l.started_at,l.finished_at,l.created_at";

/// All selectors are private SQL constants; all request values are bound.
fn query(
    scope: ReadScope,
    select: &str,
    suffix: &str,
    window: Window,
    pagination: Option<(i64, i64)>,
    id: Option<Uuid>,
) -> Result<Statement, DbError> {
    query_filtered(scope, select, suffix, window, pagination, id, None)
}

fn query_filtered(
    scope: ReadScope,
    select: &str,
    suffix: &str,
    window: Window,
    pagination: Option<(i64, i64)>,
    id: Option<Uuid>,
    owner: Option<Uuid>,
) -> Result<Statement, DbError> {
    if matches!((window.from, window.to), (Some(from), Some(to)) if from >= to) {
        return Err(DbError::Other(
            "usage range must have start before end".into(),
        ));
    }
    let (predicate, mut values) = match scope {
        ReadScope::Personal(scope) | ReadScope::Tenant(scope) => {
            (String::new(), vec![scope.tenant_id().into(), scope.user_id().into()])
        }
        ReadScope::Platform(scope) => (
            "EXISTS (SELECT 1 FROM users actor WHERE actor.id=$1 AND actor.status='active' AND actor.platform_role IN ('root','operator'))".to_owned(),
            vec![scope.user_id().into()],
        ),
    };
    let predicate = match scope {
        ReadScope::Personal(_) => format!("l.tenant_id=$1 AND l.user_id=$2 AND {ACTIVE_MEMBER}"),
        ReadScope::Tenant(scope) => {
            if scope.tenant_role() != TenantRole::Admin {
                return Err(DbError::Other(
                    "tenant administrator usage scope required".into(),
                ));
            }
            format!("l.tenant_id=$1 AND {ACTIVE_ADMIN}")
        }
        ReadScope::Platform(_) => predicate,
    };
    let from_index = values.len() + 1;
    values.extend([window.from.into(), window.to.into()]);
    let mut sql = format!(
        "SELECT {select} FROM usage_logs l WHERE {predicate} \
         AND (${from_index}::timestamptz IS NULL OR l.created_at>=${from_index}) \
         AND (${}::timestamptz IS NULL OR l.created_at<${})",
        from_index + 1,
        from_index + 1,
    );
    if let Some(id) = id {
        values.push(id.into());
        sql.push_str(&format!(" AND l.id=${}", values.len()));
    }
    if let Some(owner) = owner {
        if owner.is_nil() {
            return Err(DbError::Other("usage owner must be a real UUID".into()));
        }
        values.push(owner.into());
        sql.push_str(&format!(" AND l.user_id=${}", values.len()));
    }
    sql.push(' ');
    sql.push_str(suffix);
    if let Some((limit, offset)) = pagination {
        let limit_index = values.len() + 1;
        values.extend([limit.clamp(1, 100).into(), offset.max(0).into()]);
        sql.push_str(&format!(
            " LIMIT ${limit_index} OFFSET ${}",
            limit_index + 1
        ));
    }
    Ok(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
}

const ACTIVE_MEMBER: &str = "EXISTS (SELECT 1 FROM tenant_memberships actor \
    JOIN users au ON au.id=actor.user_id JOIN tenants t ON t.id=actor.tenant_id \
    WHERE actor.tenant_id=$1 AND actor.user_id=$2 AND actor.status='active' \
      AND au.status='active' AND t.status='active')";
const ACTIVE_ADMIN: &str = "EXISTS (SELECT 1 FROM tenant_memberships actor \
    JOIN users au ON au.id=actor.user_id JOIN tenants t ON t.id=actor.tenant_id \
    WHERE actor.tenant_id=$1 AND actor.user_id=$2 AND actor.tenant_role='admin' \
      AND actor.status='active' AND au.status='active' AND t.status='active')";
const TOTALS: &str = "COUNT(*) AS total_requests, \
    COALESCE(SUM(l.input_tokens),0)::bigint AS total_input_tokens, \
    COALESCE(SUM(l.output_tokens),0)::bigint AS total_output_tokens, \
    COALESCE(SUM(l.total_tokens),0)::bigint AS total_tokens, \
    COALESCE(SUM(l.user_amount),0) AS total_amount";
const MODEL_TOTALS: &str = "l.model_name, COUNT(*) AS request_count, \
    COALESCE(SUM(l.input_tokens),0)::bigint AS input_tokens, \
    COALESCE(SUM(l.output_tokens),0)::bigint AS output_tokens, \
    COALESCE(SUM(l.user_amount),0) AS amount";

async fn list(
    db: &impl ConnectionTrait,
    scope: ReadScope,
    window: Window,
    limit: i64,
    offset: i64,
) -> Result<Vec<UsageLog>, DbError> {
    Ok(UsageLog::find_by_statement(query(
        scope,
        "l.*",
        "ORDER BY l.created_at DESC, l.id DESC",
        window,
        Some((limit, offset)),
        None,
    )?)
    .all(db)
    .await?)
}
async fn count(
    db: &impl ConnectionTrait,
    scope: ReadScope,
    window: Window,
) -> Result<i64, DbError> {
    let row = db
        .query_one(query(scope, "COUNT(*)", "", window, None, None)?)
        .await?
        .ok_or_else(|| DbError::Other("usage count returned no row".into()))?;
    Ok(row.try_get_by_index(0)?)
}
async fn stats(
    db: &impl ConnectionTrait,
    scope: ReadScope,
    window: Window,
) -> Result<UsageStats, DbError> {
    UsageStats::find_by_statement(query(scope, TOTALS, "", window, None, None)?)
        .one(db)
        .await?
        .ok_or_else(|| DbError::Other("usage statistics returned no row".into()))
}
async fn by_model(
    db: &impl ConnectionTrait,
    scope: ReadScope,
    window: Window,
) -> Result<Vec<ModelStatsRow>, DbError> {
    Ok(ModelStatsRow::find_by_statement(query(
        scope,
        MODEL_TOTALS,
        "GROUP BY l.model_name ORDER BY request_count DESC, l.model_name",
        window,
        None,
        None,
    )?)
    .all(db)
    .await?)
}
async fn find(
    db: &impl ConnectionTrait,
    scope: ReadScope,
    id: Uuid,
) -> Result<Option<UsageLog>, DbError> {
    Ok(
        UsageLog::find_by_statement(query(scope, "l.*", "", Window::default(), None, Some(id))?)
            .one(db)
            .await?,
    )
}

/// Personal reads always bind the authenticated user, even for tenant admins.
#[derive(Debug, Clone, Copy)]
pub struct UserUsageScope(TenantScope);
impl UserUsageScope {
    pub fn new(scope: TenantScope) -> Self {
        Self(scope)
    }
    pub async fn find(
        &self,
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<UsageLog>, DbError> {
        find(db, ReadScope::Personal(self.0), id).await
    }
    pub async fn list(
        &self,
        db: &impl ConnectionTrait,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<UsageLog>, DbError> {
        list(
            db,
            ReadScope::Personal(self.0),
            Window::new(from, to),
            limit,
            offset,
        )
        .await
    }
    pub async fn count(
        &self,
        db: &impl ConnectionTrait,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<i64, DbError> {
        count(db, ReadScope::Personal(self.0), Window::new(from, to)).await
    }
    pub async fn stats(
        &self,
        db: &impl ConnectionTrait,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<UsageStats, DbError> {
        stats(
            db,
            ReadScope::Personal(self.0),
            Window::new(Some(from), Some(to)),
        )
        .await
    }
    pub async fn all_time_stats(
        &self,
        db: &impl ConnectionTrait,
    ) -> Result<UserUsageStats, DbError> {
        let totals = stats(db, ReadScope::Personal(self.0), Window::default()).await?;
        Ok(UserUsageStats {
            total_requests: totals.total_requests,
            total_input_tokens: totals.total_input_tokens,
            total_output_tokens: totals.total_output_tokens,
            total_tokens: totals.total_tokens,
            total_cost: totals.total_amount,
        })
    }
    pub async fn by_model(
        &self,
        db: &impl ConnectionTrait,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<ModelStatsRow>, DbError> {
        by_model(
            db,
            ReadScope::Personal(self.0),
            Window::new(Some(from), Some(to)),
        )
        .await
    }
}

/// Tenant-wide usage needs an administrator membership, not a platform role.
#[derive(Debug, Clone, Copy)]
pub struct TenantUsageScope(TenantScope);
impl TenantUsageScope {
    pub fn new(scope: TenantScope) -> Result<Self, DbError> {
        if scope.tenant_role() != TenantRole::Admin {
            return Err(DbError::Other(
                "tenant administrator usage scope required".into(),
            ));
        }
        Ok(Self(scope))
    }
    pub async fn find(
        &self,
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<UsageLog>, DbError> {
        find(db, ReadScope::Tenant(self.0), id).await
    }
    pub async fn list(
        &self,
        db: &impl ConnectionTrait,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<UsageLog>, DbError> {
        list(
            db,
            ReadScope::Tenant(self.0),
            Window::new(from, to),
            limit,
            offset,
        )
        .await
    }
    pub async fn count(
        &self,
        db: &impl ConnectionTrait,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<i64, DbError> {
        count(db, ReadScope::Tenant(self.0), Window::new(from, to)).await
    }
    pub async fn stats(
        &self,
        db: &impl ConnectionTrait,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<UsageStats, DbError> {
        stats(
            db,
            ReadScope::Tenant(self.0),
            Window::new(Some(from), Some(to)),
        )
        .await
    }
    pub async fn by_model(
        &self,
        db: &impl ConnectionTrait,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<ModelStatsRow>, DbError> {
        by_model(
            db,
            ReadScope::Tenant(self.0),
            Window::new(Some(from), Some(to)),
        )
        .await
    }
}

/// Only aggregate information is exposed to platform operators, never raw rows.
#[derive(Debug, Clone, Copy)]
pub struct PlatformUsageScope(PlatformScope);
#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct CurrencyUsageStats {
    pub currency: String,
    pub total_requests: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_tokens: i64,
    pub total_amount: BigDecimal,
}
impl PlatformUsageScope {
    pub fn new(scope: PlatformScope) -> Self {
        Self(scope)
    }
    pub async fn stats(
        &self,
        db: &impl ConnectionTrait,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CurrencyUsageStats>, DbError> {
        Ok(CurrencyUsageStats::find_by_statement(query(
            ReadScope::Platform(self.0),
            &format!("l.currency, {TOTALS}"),
            "GROUP BY l.currency ORDER BY l.currency",
            Window::new(Some(from), Some(to)),
            None,
            None,
        )?)
        .all(db)
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn personal_query_always_retains_tenant_and_user() {
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        for role in [TenantRole::Admin, TenantRole::Member] {
            let scope = ReadScope::Personal(TenantScope::checked(tenant, user, role).unwrap());
            let list = query(
                scope,
                "l.*",
                "ORDER BY l.created_at DESC, l.id DESC",
                Window::default(),
                Some((1, 0)),
                None,
            )
            .unwrap();
            let count = query(scope, "COUNT(*)", "", Window::default(), None, None).unwrap();
            assert!(list.sql.contains("l.tenant_id=$1 AND l.user_id=$2"));
            assert!(count.sql.contains(ACTIVE_MEMBER));
            assert_eq!(&list.values.unwrap().0[..4], &count.values.unwrap().0[..]);
        }
    }
    #[test]
    fn member_cannot_request_tenant_wide_usage() {
        let member =
            TenantScope::checked(Uuid::new_v4(), Uuid::new_v4(), TenantRole::Member).unwrap();
        assert!(TenantUsageScope::new(member).is_err());
        assert!(
            query(
                ReadScope::Tenant(member),
                "l.*",
                "",
                Window::default(),
                None,
                None
            )
            .is_err()
        );
    }
}

impl TenantUsageScope {
    pub async fn find_report(
        &self,
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<UsageLogReportRow>, DbError> {
        Ok(UsageLogReportRow::find_by_statement(query_filtered(
            ReadScope::Tenant(self.0),
            REPORT_COLUMNS,
            "",
            Window::default(),
            None,
            Some(id),
            None,
        )?)
        .one(db)
        .await?)
    }
    pub async fn list_report(
        &self,
        db: &impl ConnectionTrait,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        owner: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<UsageLogReportRow>, DbError> {
        Ok(UsageLogReportRow::find_by_statement(query_filtered(
            ReadScope::Tenant(self.0),
            REPORT_COLUMNS,
            "ORDER BY l.created_at DESC,l.id DESC",
            Window::new(from, to),
            Some((limit, offset)),
            None,
            owner,
        )?)
        .all(db)
        .await?)
    }
    pub async fn count_report(
        &self,
        db: &impl ConnectionTrait,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        owner: Option<Uuid>,
    ) -> Result<i64, DbError> {
        let row = db
            .query_one(query_filtered(
                ReadScope::Tenant(self.0),
                "COUNT(*)",
                "",
                Window::new(from, to),
                None,
                None,
                owner,
            )?)
            .await?
            .ok_or_else(|| DbError::Other("usage report count returned no row".into()))?;
        Ok(row.try_get_by_index(0)?)
    }
    pub async fn currency_stats(
        &self,
        db: &impl ConnectionTrait,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        owner: Option<Uuid>,
    ) -> Result<Vec<CurrencyUsageStats>, DbError> {
        Ok(CurrencyUsageStats::find_by_statement(query_filtered(
            ReadScope::Tenant(self.0),
            &format!("l.currency,{TOTALS}"),
            "GROUP BY l.currency ORDER BY l.currency",
            Window::new(from, to),
            None,
            None,
            owner,
        )?)
        .all(db)
        .await?)
    }
}

#[cfg(test)]
mod reporting_tests {
    use super::*;
    #[test]
    fn report_count_and_currency_totals_share_tenant_owner_and_time_predicates() {
        let scope = ReadScope::Tenant(
            TenantScope::checked(Uuid::new_v4(), Uuid::new_v4(), TenantRole::Admin).unwrap(),
        );
        let owner = Some(Uuid::new_v4());
        let count =
            query_filtered(scope, "COUNT(*)", "", Window::default(), None, None, owner).unwrap();
        let stats = query_filtered(
            scope,
            "l.currency",
            "GROUP BY l.currency",
            Window::default(),
            None,
            None,
            owner,
        )
        .unwrap();
        assert_eq!(count.values, stats.values);
        assert!(count.sql.contains(ACTIVE_ADMIN));
        assert!(count.sql.contains("l.user_id=$5"));
        assert!(!count.sql.contains("ORDER BY"));
        assert!(!count.sql.contains("FOR UPDATE"));
    }
}
