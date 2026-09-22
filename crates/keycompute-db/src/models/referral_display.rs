//! Bounded referral display query. Counts and page values share one SQL snapshot.
use crate::DbError;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ReferralDisplay {
    pub user_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub total_consumption: BigDecimal,
    pub earnings: BigDecimal,
}
#[derive(Debug)]
pub struct ReferralDisplayPage {
    pub referrals: Vec<ReferralDisplay>,
    pub total: i64,
    pub as_of: DateTime<Utc>,
}
#[derive(Debug, FromQueryResult)]
struct PageRow {
    total: i64,
    as_of: DateTime<Utc>,
    user_id: Option<Uuid>,
    email: Option<String>,
    name: Option<String>,
    created_at: Option<DateTime<Utc>>,
    total_consumption: BigDecimal,
    earnings: BigDecimal,
}
// Page materialization happens BEFORE aggregate joins. Aggregating usage and
// commissions separately prevents the one-to-many join from multiplying money.
const PAGE_SQL: &str = r#"
WITH matched AS NOT MATERIALIZED (
    SELECT r.id, r.user_id, r.created_at, u.email, u.name
    FROM user_referrals r JOIN users u ON u.id = r.user_id
    WHERE r.level1_referrer_id = $1 OR r.level2_referrer_id = $1
), page AS MATERIALIZED (
    SELECT * FROM matched ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3
), consumption AS (
    SELECT ul.user_id, SUM(ul.user_amount) AS amount
    FROM usage_logs ul JOIN page p ON p.user_id = ul.user_id WHERE ul.tenant_id=$4 AND ul.currency='CNY' GROUP BY ul.user_id
), earnings AS (
    SELECT ul.user_id, SUM(dr.share_amount) AS amount
    FROM page p JOIN usage_logs ul ON ul.user_id = p.user_id
    JOIN distribution_records dr ON dr.usage_log_id = ul.id AND dr.tenant_id=ul.tenant_id
    WHERE dr.beneficiary_id = $1 AND dr.tenant_id=$4 AND ul.currency='CNY' GROUP BY ul.user_id
), totals AS (SELECT COUNT(*)::bigint AS total FROM matched)
SELECT totals.total, statement_timestamp() AS as_of,
       p.user_id, p.email, p.name, p.created_at,
       COALESCE(c.amount, 0) AS total_consumption,
       COALESCE(e.amount, 0) AS earnings
FROM totals LEFT JOIN page p ON TRUE
LEFT JOIN consumption c ON c.user_id = p.user_id
LEFT JOIN earnings e ON e.user_id = p.user_id
WHERE EXISTS (SELECT 1 FROM tenant_memberships m JOIN users actor ON actor.id=m.user_id JOIN tenants t ON t.id=m.tenant_id WHERE m.tenant_id=$4 AND m.user_id=$1 AND m.status='active' AND actor.status='active' AND t.status='active')
ORDER BY p.created_at DESC, p.id DESC
"#;
/// Cross-tenant referral relationships are valid; only the beneficiary's
/// actual relationship grants visibility. Financial columns are limited to
/// the authenticated tenant and the legacy CNY display currency.
pub async fn find_referral_display_page(
    db: &impl ConnectionTrait,
    scope: keycompute_types::TenantScope,
    limit: i64,
    offset: i64,
) -> Result<ReferralDisplayPage, DbError> {
    if !(1..=100).contains(&limit) || !(0..=100_000_000).contains(&offset) {
        return Err(DbError::Other("invalid bounded referral pagination".into()));
    }
    let rows = PageRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        PAGE_SQL,
        [
            scope.user_id().into(),
            limit.into(),
            offset.into(),
            scope.tenant_id().into(),
        ],
    ))
    .all(db)
    .await?;
    let head = rows
        .first()
        .ok_or_else(|| DbError::Other("missing referral count row".into()))?;
    let total = head.total;
    let as_of = head.as_of;
    let mut referrals = Vec::with_capacity(rows.len().min(limit as usize));
    for row in rows {
        // LEFT JOIN keeps the total even for an empty or out-of-range page.
        let Some(user_id) = row.user_id else {
            continue;
        };
        referrals.push(ReferralDisplay {
            user_id,
            email: row
                .email
                .ok_or_else(|| DbError::Other("referral user email is absent".into()))?,
            name: row.name,
            created_at: row
                .created_at
                .ok_or_else(|| DbError::Other("referral date is absent".into()))?,
            total_consumption: row.total_consumption,
            earnings: row.earnings,
        });
    }
    Ok(ReferralDisplayPage {
        referrals,
        total,
        as_of,
    })
}
