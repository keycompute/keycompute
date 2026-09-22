//! Current-authority distribution reporting. No ledger or policy mutation rights.
use crate::{DbError, TenantDistributionRule};
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use keycompute_types::{PlatformRole, PlatformScope, TenantRole, TenantScope};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
pub enum DistributionScope {
    Owned(TenantScope),
    Tenant(TenantScope),
    Platform(PlatformScope, Uuid),
}
const ACTIVE: &str = "EXISTS (SELECT 1 FROM tenant_memberships am JOIN users au ON au.id=am.user_id JOIN tenants at ON at.id=am.tenant_id WHERE am.tenant_id=$1 AND am.user_id=$2 AND am.status='active' AND au.status='active' AND at.status='active')";
const ADMIN: &str = "EXISTS (SELECT 1 FROM tenant_memberships am JOIN users au ON au.id=am.user_id JOIN tenants at ON at.id=am.tenant_id WHERE am.tenant_id=$1 AND am.user_id=$2 AND am.tenant_role='admin' AND am.status='active' AND au.status='active' AND at.status='active')";
const ROOT: &str = "EXISTS (SELECT 1 FROM users au WHERE au.id=$2 AND au.platform_role='root' AND au.status='active')";
fn invalid(message: &str) -> DbError {
    DbError::Other(format!("invalid distribution input: {message}"))
}
fn denied() -> DbError {
    DbError::Other("distribution authorization denied".into())
}
impl DistributionScope {
    pub fn tenant_id(self) -> Uuid {
        match self {
            Self::Owned(s) | Self::Tenant(s) => s.tenant_id(),
            Self::Platform(_, t) => t,
        }
    }
    fn parts(self) -> Result<(Uuid, Uuid, &'static str), DbError> {
        let (t, u, p) = match self {
            Self::Owned(s) => (s.tenant_id(), s.user_id(), ACTIVE),
            Self::Tenant(s) if s.tenant_role() == TenantRole::Admin => {
                (s.tenant_id(), s.user_id(), ADMIN)
            }
            Self::Platform(s, t) if s.platform_role() == PlatformRole::Root => {
                (t, s.user_id(), ROOT)
            }
            _ => return Err(denied()),
        };
        if t.is_nil() || u.is_nil() {
            return Err(denied());
        }
        Ok((t, u, p))
    }
}
#[derive(Debug, Clone, Default)]
pub struct RecordFilter {
    pub beneficiary_id: Option<Uuid>,
    pub status: Option<String>,
    pub level: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub currency: Option<String>,
}
#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct DistributionRecordReport {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub beneficiary_scope: String,
    pub beneficiary_id: Option<Uuid>,
    pub usage_log_id: Uuid,
    pub referred_id: Uuid,
    pub amount: BigDecimal,
    pub currency: String,
    pub commission: BigDecimal,
    pub share_ratio: BigDecimal,
    pub level: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct DistributionCurrencyStats {
    pub currency: String,
    pub total_earnings: BigDecimal,
    pub pending_amount: BigDecimal,
    pub settled_amount: BigDecimal,
    pub level1_earnings: BigDecimal,
    pub level2_earnings: BigDecimal,
    pub record_count: i64,
}
const RECORD_COLUMNS: &str = "r.id,r.tenant_id,r.beneficiary_scope,r.beneficiary_id,r.usage_log_id,u.user_id AS referred_id,u.user_amount AS amount,u.currency,r.share_amount AS commission,r.share_ratio,r.level,r.status,r.created_at";
const TOTAL_COLUMNS: &str = "u.currency,COALESCE(SUM(r.share_amount),0) AS total_earnings,COALESCE(SUM(r.share_amount) FILTER(WHERE r.status='pending'),0) AS pending_amount,COALESCE(SUM(r.share_amount) FILTER(WHERE r.status='settled'),0) AS settled_amount,COALESCE(SUM(r.share_amount) FILTER(WHERE r.level='level1'),0) AS level1_earnings,COALESCE(SUM(r.share_amount) FILTER(WHERE r.level='level2'),0) AS level2_earnings,COUNT(*)::bigint AS record_count";
#[derive(Clone, Copy)]
enum RecordProjection {
    Rows,
    Count,
    Totals,
}
fn validate_filter(f: &RecordFilter) -> Result<(), DbError> {
    if f.beneficiary_id.is_some_and(|v| v.is_nil()) {
        return Err(invalid("beneficiary ID is required"));
    }
    if f.status
        .as_deref()
        .is_some_and(|v| !matches!(v, "pending" | "settled" | "cancelled"))
    {
        return Err(invalid("unknown status"));
    }
    if f.level
        .as_deref()
        .is_some_and(|v| !matches!(v, "level1" | "level2"))
    {
        return Err(invalid("unknown level"));
    }
    if matches!((f.from,f.until),(Some(a),Some(b)) if a>=b) {
        return Err(invalid("from must precede until"));
    }
    if f.currency
        .as_deref()
        .is_some_and(|v| !(3..=8).contains(&v.len()) || !v.bytes().all(|c| c.is_ascii_uppercase()))
    {
        return Err(invalid("invalid currency"));
    }
    Ok(())
}
fn record_query(
    scope: DistributionScope,
    f: &RecordFilter,
    projection: RecordProjection,
    id: Option<Uuid>,
    page: Option<(i64, i64)>,
) -> Result<Statement, DbError> {
    validate_filter(f)?;
    let (tenant, actor, authority) = scope.parts()?;
    let owner = if matches!(scope, DistributionScope::Owned(_)) {
        " AND r.beneficiary_id=$2"
    } else {
        ""
    };
    let columns = match projection {
        RecordProjection::Rows => RECORD_COLUMNS,
        RecordProjection::Count => "COUNT(*)::bigint AS total",
        RecordProjection::Totals => TOTAL_COLUMNS,
    };
    let mut sql = format!(
        "SELECT {columns} FROM distribution_records r JOIN usage_logs u ON u.tenant_id=r.tenant_id AND u.id=r.usage_log_id WHERE r.tenant_id=$1 AND {authority}{owner} AND ($3::uuid IS NULL OR r.beneficiary_id=$3) AND ($4::text IS NULL OR r.status=$4) AND ($5::text IS NULL OR r.level=$5) AND ($6::timestamptz IS NULL OR r.created_at>=$6) AND ($7::timestamptz IS NULL OR r.created_at<$7) AND ($8::text IS NULL OR u.currency=$8) AND ($9::uuid IS NULL OR r.id=$9)"
    );
    let mut values = vec![
        tenant.into(),
        actor.into(),
        f.beneficiary_id.into(),
        f.status.clone().into(),
        f.level.clone().into(),
        f.from.into(),
        f.until.into(),
        f.currency.clone().into(),
        id.into(),
    ];
    match projection {
        RecordProjection::Rows => sql.push_str(" ORDER BY r.created_at DESC,r.id DESC"),
        RecordProjection::Totals => sql.push_str(" GROUP BY u.currency ORDER BY u.currency"),
        RecordProjection::Count => {}
    }
    if let Some((limit, offset)) = page {
        sql.push_str(" LIMIT $10 OFFSET $11");
        values.extend([limit.clamp(1, 100).into(), offset.max(0).into()]);
    }
    Ok(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
}
pub async fn records(
    db: &impl ConnectionTrait,
    scope: DistributionScope,
    f: &RecordFilter,
    limit: i64,
    offset: i64,
) -> Result<Vec<DistributionRecordReport>, DbError> {
    Ok(DistributionRecordReport::find_by_statement(record_query(
        scope,
        f,
        RecordProjection::Rows,
        None,
        Some((limit, offset)),
    )?)
    .all(db)
    .await?)
}
pub async fn record(
    db: &impl ConnectionTrait,
    scope: DistributionScope,
    id: Uuid,
) -> Result<Option<DistributionRecordReport>, DbError> {
    if id.is_nil() {
        return Ok(None);
    }
    Ok(DistributionRecordReport::find_by_statement(record_query(
        scope,
        &RecordFilter::default(),
        RecordProjection::Rows,
        Some(id),
        None,
    )?)
    .one(db)
    .await?)
}
pub async fn record_count(
    db: &impl ConnectionTrait,
    scope: DistributionScope,
    f: &RecordFilter,
) -> Result<i64, DbError> {
    let row = db
        .query_one(record_query(scope, f, RecordProjection::Count, None, None)?)
        .await?
        .ok_or_else(|| DbError::Other("distribution count unavailable".into()))?;
    Ok(row.try_get("", "total")?)
}
pub async fn record_stats(
    db: &impl ConnectionTrait,
    scope: DistributionScope,
    f: &RecordFilter,
) -> Result<Vec<DistributionCurrencyStats>, DbError> {
    Ok(DistributionCurrencyStats::find_by_statement(record_query(
        scope,
        f,
        RecordProjection::Totals,
        None,
        None,
    )?)
    .all(db)
    .await?)
}

#[derive(Debug, Clone, Default)]
pub struct RuleFilter {
    pub search: Option<String>,
    pub beneficiary_id: Option<Uuid>,
    pub is_active: Option<bool>,
}
fn rule_query(
    scope: DistributionScope,
    f: &RuleFilter,
    id: Option<Uuid>,
    count: bool,
    page: Option<(i64, i64)>,
) -> Result<Statement, DbError> {
    if matches!(scope, DistributionScope::Owned(_)) {
        return Err(denied());
    }
    let (tenant, actor, authority) = scope.parts()?;
    if f.beneficiary_id.is_some_and(|v| v.is_nil())
        || f.search.as_ref().is_some_and(|v| v.chars().count() > 255)
    {
        return Err(invalid("invalid rule filter"));
    }
    let search = f
        .search
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(super::query::escape_like_pattern);
    let columns = if count {
        "COUNT(*)::bigint AS total"
    } else {
        "r.*"
    };
    let mut sql = format!(
        "SELECT {columns} FROM tenant_distribution_rules r WHERE r.tenant_id=$1 AND {authority} AND ($3::text IS NULL OR r.name ILIKE '%'||$3||'%' ESCAPE '\\') AND ($4::uuid IS NULL OR r.beneficiary_id=$4) AND ($5::boolean IS NULL OR r.is_active=$5) AND ($6::uuid IS NULL OR r.id=$6)"
    );
    let mut values = vec![
        tenant.into(),
        actor.into(),
        search.into(),
        f.beneficiary_id.into(),
        f.is_active.into(),
        id.into(),
    ];
    if !count {
        sql.push_str(" ORDER BY r.priority DESC,r.created_at ASC,r.id ASC");
    }
    if let Some((limit, offset)) = page {
        sql.push_str(" LIMIT $7 OFFSET $8");
        values.extend([limit.clamp(1, 100).into(), offset.max(0).into()]);
    }
    Ok(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
}
pub async fn rules(
    db: &impl ConnectionTrait,
    scope: DistributionScope,
    f: &RuleFilter,
    limit: i64,
    offset: i64,
) -> Result<Vec<TenantDistributionRule>, DbError> {
    Ok(TenantDistributionRule::find_by_statement(rule_query(
        scope,
        f,
        None,
        false,
        Some((limit, offset)),
    )?)
    .all(db)
    .await?)
}
pub async fn rule_count(
    db: &impl ConnectionTrait,
    scope: DistributionScope,
    f: &RuleFilter,
) -> Result<i64, DbError> {
    let r = db
        .query_one(rule_query(scope, f, None, true, None)?)
        .await?
        .ok_or_else(|| DbError::Other("rule count unavailable".into()))?;
    Ok(r.try_get("", "total")?)
}
pub async fn rule(
    db: &impl ConnectionTrait,
    scope: DistributionScope,
    id: Uuid,
) -> Result<Option<TenantDistributionRule>, DbError> {
    if id.is_nil() {
        return Ok(None);
    }
    Ok(TenantDistributionRule::find_by_statement(rule_query(
        scope,
        &RuleFilter::default(),
        Some(id),
        false,
        None,
    )?)
    .one(db)
    .await?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_shapes_share_scope_time_and_currency_predicates() {
        let t = Uuid::new_v4();
        let u = Uuid::new_v4();
        let scope =
            DistributionScope::Platform(PlatformScope::checked(u, PlatformRole::Root).unwrap(), t);
        let f = RecordFilter {
            beneficiary_id: Some(Uuid::new_v4()),
            from: Some(Utc::now()),
            currency: Some("CNY".into()),
            ..Default::default()
        };
        let rows = record_query(scope, &f, RecordProjection::Rows, None, Some((1, 0))).unwrap();
        let count = record_query(scope, &f, RecordProjection::Count, None, None).unwrap();
        let totals = record_query(scope, &f, RecordProjection::Totals, None, None).unwrap();
        fn predicate(q: &Statement) -> &str {
            q.sql
                .split_once(" WHERE r.tenant_id=$1")
                .unwrap()
                .1
                .split(" ORDER BY ")
                .next()
                .unwrap()
                .split(" GROUP BY ")
                .next()
                .unwrap()
        }
        assert_eq!(predicate(&rows), predicate(&count));
        assert_eq!(predicate(&count), predicate(&totals));
        assert_eq!(&rows.values.unwrap().0[..9], &count.values.unwrap().0);
        assert!(totals.sql.contains("GROUP BY u.currency"));
    }
    #[test]
    fn personal_admin_never_widens_and_member_cannot_form_admin_scope() {
        let t = Uuid::new_v4();
        let u = Uuid::new_v4();
        let member = TenantScope::checked(t, u, TenantRole::Member).unwrap();
        assert!(DistributionScope::Tenant(member).parts().is_err());
        for role in [TenantRole::Member, TenantRole::Admin] {
            let s = DistributionScope::Owned(TenantScope::checked(t, u, role).unwrap());
            let q = record_query(
                s,
                &RecordFilter::default(),
                RecordProjection::Rows,
                None,
                None,
            )
            .unwrap();
            assert!(q.sql.contains("r.beneficiary_id=$2"));
            assert!(q.sql.contains("r.tenant_id=$1"));
            assert!(rule_query(s, &RuleFilter::default(), None, false, None).is_err());
        }
        let operator = PlatformScope::checked(u, PlatformRole::Operator).unwrap();
        assert!(DistributionScope::Platform(operator, t).parts().is_err());
    }
    #[test]
    fn invalid_filters_fail_before_query_execution() {
        for f in [
            RecordFilter {
                status: Some("anything".into()),
                ..Default::default()
            },
            RecordFilter {
                beneficiary_id: Some(Uuid::nil()),
                ..Default::default()
            },
            RecordFilter {
                currency: Some("cny".into()),
                ..Default::default()
            },
            RecordFilter {
                from: Some(Utc::now()),
                until: Some(Utc::now() - chrono::Duration::days(1)),
                ..Default::default()
            },
        ] {
            assert!(validate_filter(&f).is_err());
        }
    }
}
