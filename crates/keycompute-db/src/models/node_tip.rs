//! Tenant/currency-scoped node earnings and immutable accepted-work credits.
use super::financial_scope::FinancialScope;
use crate::DbError;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use serde::Serialize;
use std::str::FromStr;
use uuid::Uuid;

#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct NodeTip {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub usage_log_id: Uuid,
    pub node_id: Uuid,
    pub owner_user_id: Uuid,
    pub consumer_user_id: Uuid,
    pub tip_amount: Decimal,
    pub currency: String,
    pub tip_ratio: Decimal,
    pub bill_amount: Decimal,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct NodeTipSummary {
    pub pending_amount: Decimal,
    pub reserved_amount: Decimal,
    pub withdrawn_amount: Decimal,
    pub total_amount: Decimal,
    pub pending_count: i64,
}
pub(super) fn currency(value: &str) -> Result<String, DbError> {
    let value = value.trim().to_ascii_uppercase();
    if value.len() != 3 || !value.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(DbError::Other("tip_currency_invalid".into()));
    }
    Ok(value)
}
pub(super) fn page(limit: i64, offset: i64) -> Result<(), DbError> {
    if !(1..=100).contains(&limit) || !(0..=1_000_000).contains(&offset) {
        return Err(DbError::Other("tip_page_invalid".into()));
    }
    Ok(())
}
impl NodeTip {
    pub async fn summary(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
        money: &str,
    ) -> Result<NodeTipSummary, DbError> {
        scope.tenant_id()?;
        let mut values = scope.values();
        values.push(currency(money)?.into());
        let sql = format!(
            "WITH credits AS (SELECT COALESCE(SUM(t.tip_amount),0) AS amount,COUNT(*)::bigint AS n FROM node_tips t WHERE t.tenant_id=$8 AND t.currency=$10 {}), withdrawals AS (SELECT COALESCE(SUM(w.total_amount) FILTER(WHERE w.status IN ('pending','approved')),0) AS reserved,COALESCE(SUM(w.total_amount) FILTER(WHERE w.status='completed'),0) AS paid FROM node_tip_withdrawals w WHERE w.tenant_id=$8 AND w.currency=$10 {}) SELECT c.amount-w.reserved-w.paid AS pending_amount,w.reserved AS reserved_amount,w.paid AS withdrawn_amount,c.amount AS total_amount,c.n AS pending_count FROM credits c CROSS JOIN withdrawals w WHERE {}",
            scope.owner_predicate("t"),
            scope.owner_predicate("w"),
            scope.predicate()
        );
        Self::validate_summary(
            NodeTipSummary::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("financial_authority_invalid".into()))?,
        )
    }
    fn validate_summary(row: NodeTipSummary) -> Result<NodeTipSummary, DbError> {
        if row.pending_amount < Decimal::ZERO {
            return Err(DbError::Other("tip_ledger_inconsistent".into()));
        }
        Ok(row)
    }
    pub async fn list_in_scope(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
        money: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        scope.tenant_id()?;
        page(limit, offset)?;
        let mut values = scope.values();
        values.extend([currency(money)?.into(), limit.into(), offset.into()]);
        let sql = format!(
            "SELECT t.* FROM node_tips t WHERE t.tenant_id=$8 AND t.currency=$10 {} AND {} ORDER BY t.created_at DESC,t.id DESC LIMIT $11 OFFSET $12",
            scope.owner_predicate("t"),
            scope.predicate()
        );
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .all(db)
        .await?)
    }
    pub async fn count_in_scope(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
        money: &str,
    ) -> Result<i64, DbError> {
        scope.tenant_id()?;
        let mut values = scope.values();
        values.push(currency(money)?.into());
        let sql = format!(
            "SELECT (SELECT COUNT(*)::bigint FROM node_tips t WHERE t.tenant_id=$8 AND t.currency=$10 {}) AS n WHERE {}",
            scope.owner_predicate("t"),
            scope.predicate()
        );
        Ok(db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?
            .ok_or_else(|| DbError::Other("financial_authority_invalid".into()))?
            .try_get("", "n")?)
    }
    /// This is an internal settlement capability, never a console authorization
    /// shortcut. It consumes a fixed tenant and already-recorded usage identity.
    pub async fn create_from_usage_log(
        db: &(impl ConnectionTrait + TransactionTrait),
        tenant_id: Uuid,
        usage_log_id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        let candidate=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT u.id FROM usage_logs u WHERE u.id=$1 AND u.tenant_id=$2 AND EXISTS(SELECT 1 FROM node_tasks t WHERE t.request_id=u.request_id AND t.tenant_id=u.tenant_id AND t.user_id=u.user_id AND t.status='succeeded' AND t.assigned_node_id IS NOT NULL) AND NOT EXISTS(SELECT 1 FROM node_tips credited WHERE credited.tenant_id=u.tenant_id AND credited.usage_log_id=u.id) FOR KEY SHARE OF u",[usage_log_id.into(),tenant_id.into()])).await?;
        if candidate.is_none() {
            return Ok(None);
        }
        let tx = db.begin().await?;
        super::tenant::Tenant::find_by_id_for_key_share(&tx, tenant_id)
            .await?
            .ok_or_else(|| DbError::not_found("Tenant", tenant_id))?;
        // Compatible source locks retain immutable IDs without taking the
        // task/node write-lock order in reverse of normal worker completion.
        let row=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT u.user_id,u.currency,u.user_amount::text AS amount,n.id AS node_id,n.owner_user_id FROM usage_logs u JOIN node_tasks t ON t.request_id=u.request_id AND t.tenant_id=u.tenant_id AND t.user_id=u.user_id JOIN nodes n ON n.id=t.assigned_node_id AND n.tenant_id=u.tenant_id WHERE u.id=$1 AND u.tenant_id=$2 AND t.status='succeeded' FOR KEY SHARE OF u,t,n",[usage_log_id.into(),tenant_id.into()])).await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(None);
        };
        let consumer: Uuid = row.try_get("", "user_id")?;
        let owner: Uuid = row.try_get("", "owner_user_id")?;
        if consumer == owner {
            tx.rollback().await?;
            return Ok(None);
        }
        let node_id: Uuid = row.try_get("", "node_id")?;
        let money = currency(&row.try_get::<String>("", "currency")?)?;
        let amount = Decimal::from_str(&row.try_get::<String>("", "amount")?)
            .map_err(|_| DbError::Other("tip_amount_invalid".into()))?
            .round_dp(10);
        let ratio = tx
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT value FROM system_settings WHERE key=$1",
                [super::system_setting::setting_keys::NODE_TIP_RATIO.into()],
            ))
            .await?
            .map(|r| r.try_get_by_index::<String>(0))
            .transpose()?
            .unwrap_or_else(|| "0.90".into());
        let ratio =
            Decimal::from_str(&ratio).map_err(|_| DbError::Other("tip_ratio_invalid".into()))?;
        if ratio < Decimal::ZERO || ratio > Decimal::ONE {
            return Err(DbError::Other("tip_ratio_invalid".into()));
        }
        let tip = (amount * ratio).round_dp(10);
        if amount <= Decimal::ZERO || tip <= Decimal::ZERO {
            tx.rollback().await?;
            return Ok(None);
        }
        let row=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO node_tips(tenant_id,usage_log_id,node_id,owner_user_id,consumer_user_id,tip_amount,currency,tip_ratio,bill_amount) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(usage_log_id) DO NOTHING RETURNING *",
            [tenant_id.into(),usage_log_id.into(),node_id.into(),owner.into(),consumer.into(),tip.into(),money.into(),ratio.into(),amount.into()])).one(&tx).await?;
        tx.commit().await?;
        Ok(row)
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;
    use sea_orm::{Database, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Debug, Default)]
    struct NoNodeProxy {
        reads: AtomicUsize,
        begins: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl ProxyDatabaseTrait for NoNodeProxy {
        async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
            assert!(
                statement.sql.contains("EXISTS") && statement.sql.contains("FOR KEY SHARE OF u")
            );
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(vec![])
        }
        async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
            panic!("no-node lookup must never mutate state")
        }
        async fn begin(&self) {
            self.begins.fetch_add(1, Ordering::Relaxed);
        }
    }
    #[tokio::test]
    async fn non_node_tip_probe_uses_one_authoritative_read_and_no_transaction() {
        let proxy = Arc::new(NoNodeProxy::default());
        #[derive(Debug)]
        struct Shared(Arc<NoNodeProxy>);
        #[async_trait::async_trait]
        impl ProxyDatabaseTrait for Shared {
            async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
                self.0.query(statement).await
            }
            async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
                self.0.execute(statement).await
            }
            async fn begin(&self) {
                self.0.begin().await;
            }
        }
        let db = Database::connect_proxy(
            DbBackend::Postgres,
            Arc::new(Box::new(Shared(proxy.clone()))),
        )
        .await
        .unwrap();
        assert!(
            NodeTip::create_from_usage_log(&db, Uuid::new_v4(), Uuid::new_v4())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(proxy.reads.load(Ordering::Relaxed), 1);
        assert_eq!(proxy.begins.load(Ordering::Relaxed), 0);
    }
}
