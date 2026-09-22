//! Bounded read models for console presentation; never used to authorize spending.
use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::Value;
use uuid::Uuid;

async fn json_query(
    db: &impl ConnectionTrait,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<Value, DbError> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await?
        .ok_or_else(|| DbError::Other("display query returned no row".into()))?;
    row.try_get_by_index(0).map_err(DbError::from)
}

/// Half-open UTC interval. Only day/hour grains and at most 366 buckets are allowed.
pub async fn usage_trend(
    db: &impl ConnectionTrait,
    user: Uuid,
    tenant: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    grain: &str,
) -> Result<Value, DbError> {
    let seconds = match grain {
        "day" => 86400,
        "hour" => 3600,
        _ => return Err(DbError::Other("invalid trend grain".into())),
    };
    let limit = if grain == "day" { 366 } else { 168 };
    let duration = (to - from).num_seconds();
    if from >= to || duration > seconds * (limit - 1) {
        return Err(DbError::Other("invalid trend interval".into()));
    }
    json_query(
        db,
        TREND_SQL,
        vec![
            user.into(),
            from.into(),
            to.into(),
            grain.into(),
            tenant.into(),
        ],
    )
    .await
}
const TREND_SQL: &str = r#"
WITH buckets AS (
 SELECT generate_series(date_trunc($4, $2::timestamptz AT TIME ZONE 'UTC'),
   date_trunc($4, ($3::timestamptz - INTERVAL '1 microsecond') AT TIME ZONE 'UTC'),
   CASE WHEN $4='day' THEN INTERVAL '1 day' ELSE INTERVAL '1 hour' END) AS bucket
), totals AS (
 SELECT date_trunc($4, created_at AT TIME ZONE 'UTC') AS bucket, COUNT(*) AS requests,
 COALESCE(SUM(total_tokens),0) AS total_tokens, COALESCE(SUM(user_amount),0) AS total_cost
 FROM usage_logs WHERE user_id=$1 AND tenant_id=$5 AND created_at >= $2 AND created_at < $3 GROUP BY 1
) SELECT jsonb_build_object('from',$2::timestamptz,'to',$3::timestamptz,'granularity',$4::text,
 'as_of',statement_timestamp(),'buckets',COALESCE(jsonb_agg(jsonb_build_object(
 'start',to_char(b.bucket, 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),'requests',COALESCE(t.requests,0),
 'total_tokens',COALESCE(t.total_tokens,0),'total_cost',COALESCE(t.total_cost,0)::text)
 ORDER BY b.bucket),'[]'::jsonb)) FROM buckets b LEFT JOIN totals t USING(bucket)
"#;

/// Counts and bounded previews, not complete key/order/usage collections.
pub async fn dashboard(
    db: &impl ConnectionTrait,
    user: Uuid,
    tenant: Uuid,
) -> Result<Value, DbError> {
    json_query(db, DASHBOARD_SQL, vec![user.into(), tenant.into()]).await
}
const DASHBOARD_SQL: &str = r#"
SELECT jsonb_build_object('as_of',statement_timestamp(),
 'stats',(SELECT jsonb_build_object('total_requests',COUNT(*),'total_tokens',COALESCE(SUM(total_tokens),0),
 'total_input_tokens',COALESCE(SUM(input_tokens),0),'total_output_tokens',COALESCE(SUM(output_tokens),0),
 'total_cost',COALESCE(SUM(user_amount),0)::text,'period','all_time') FROM usage_logs WHERE user_id=$1 AND tenant_id=$2),
 'active_key_count',(SELECT COUNT(*) FROM produce_ai_keys WHERE user_id=$1 AND tenant_id=$2
 AND NOT revoked AND (expires_at IS NULL OR expires_at>statement_timestamp())),
 'active_keys',(SELECT COALESCE(jsonb_agg(to_jsonb(k) ORDER BY k.created_at DESC,k.id DESC),'[]'::jsonb) FROM
 (SELECT id,name,produce_ai_key_preview AS key_preview,last_used_at,created_at FROM produce_ai_keys
 WHERE user_id=$1 AND tenant_id=$2 AND NOT revoked AND (expires_at IS NULL OR expires_at>statement_timestamp())
 ORDER BY created_at DESC,id DESC LIMIT 4) k),
 'recent_usage',(SELECT COALESCE(jsonb_agg(to_jsonb(u) ORDER BY u.created_at DESC,u.id DESC),'[]'::jsonb) FROM
 (SELECT id,request_id,model_name AS model,input_tokens,output_tokens,total_tokens,user_amount::text AS cost,status,created_at
 FROM usage_logs WHERE user_id=$1 AND tenant_id=$2 ORDER BY created_at DESC,id DESC LIMIT 5) u),
 'recent_orders',(SELECT COALESCE(jsonb_agg(to_jsonb(p) ORDER BY p.created_at DESC,p.id DESC),'[]'::jsonb) FROM
 (SELECT id,amount::text AS amount,currency,status,created_at FROM payment_orders WHERE user_id=$1 AND tenant_id=$2
 ORDER BY created_at DESC,id DESC LIMIT 3) p))
"#;

/// Independent aggregate subqueries prevent fanout multiplication of money.
pub async fn distribution(
    db: &impl ConnectionTrait,
    scope: keycompute_types::TenantScope,
) -> Result<Value, DbError> {
    json_query(db, r#"
    SELECT jsonb_build_object('as_of',statement_timestamp(),'earnings',
      jsonb_build_object('user_id',$1::uuid,'currency','CNY',
        'total_earnings',d.total::text,'pending_amount',d.pending::text,'settled_amount',d.settled::text,
        'level1_referrals',r.level1,'level2_referrals',r.level2))
    FROM (SELECT COALESCE(SUM(dr.share_amount),0) AS total,
      COALESCE(SUM(dr.share_amount) FILTER (WHERE dr.status='pending'),0) AS pending,
      COALESCE(SUM(dr.share_amount) FILTER (WHERE dr.status='settled'),0) AS settled
      FROM distribution_records dr JOIN usage_logs ul ON ul.tenant_id=dr.tenant_id AND ul.id=dr.usage_log_id
      WHERE dr.beneficiary_id=$1 AND dr.tenant_id=$2 AND ul.currency='CNY') d
    CROSS JOIN (SELECT COUNT(*) FILTER (WHERE level1_referrer_id=$1) AS level1,
      COUNT(*) FILTER (WHERE level2_referrer_id=$1) AS level2
      FROM user_referrals WHERE level1_referrer_id=$1 OR level2_referrer_id=$1) r
    WHERE EXISTS (SELECT 1 FROM tenant_memberships m JOIN users u ON u.id=m.user_id JOIN tenants t ON t.id=m.tenant_id WHERE m.tenant_id=$2 AND m.user_id=$1 AND m.status='active' AND u.status='active' AND t.status='active')
    "#, vec![scope.user_id().into(),scope.tenant_id().into()]).await
}
