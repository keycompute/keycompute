//! Root-only global node earnings policy; accepted ledger credits stay immutable.
use crate::models::{financial_scope::FinancialScope, system_setting::setting_keys};
use crate::{AuditContext, DbError, TenantAuditEvent};
use chrono::{DateTime, Utc};
use keycompute_types::{AuditResult, AuditScopeType};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use serde::Serialize;

#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct TipRatioSetting {
    pub ratio: String,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone)]
pub struct UpdateTipRatio {
    pub ratio: String,
    pub expected_updated_at: DateTime<Utc>,
    pub reason: String,
}
impl TipRatioSetting {
    pub fn normalize(value: &str) -> Result<String, DbError> {
        let value = value
            .trim()
            .parse::<Decimal>()
            .map_err(|_| DbError::Other("tip_ratio_invalid".into()))?
            .normalize();
        if value <= Decimal::ZERO || value > Decimal::ONE || value.scale() > 4 {
            return Err(DbError::Other("tip_ratio_invalid".into()));
        }
        Ok(value.to_string())
    }
    pub async fn read(db: &impl ConnectionTrait, scope: FinancialScope) -> Result<Self, DbError> {
        scope.require_root_global()?;
        let mut values = scope.values();
        values.push(setting_keys::NODE_TIP_RATIO.into());
        let sql = format!(
            "SELECT value AS ratio,updated_at FROM system_settings WHERE key=$10 AND is_sensitive IS FALSE AND {}",
            scope.predicate()
        );
        Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .one(db)
        .await?
        .ok_or_else(|| DbError::Other("financial_authority_invalid".into()))
    }
    pub async fn update(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: FinancialScope,
        audit: &AuditContext,
        command: &UpdateTipRatio,
    ) -> Result<Self, DbError> {
        scope.require_root_global()?;
        let ratio = Self::normalize(&command.ratio)?;
        let reason = command.reason.trim();
        if reason.is_empty() || reason.len() > 500 || reason.chars().any(char::is_control) {
            return Err(DbError::Other("financial_reason_invalid".into()));
        }
        let tx = db.begin().await?;
        let result=async {
            tx.execute_unprepared("SET LOCAL lock_timeout='3s'; SET LOCAL statement_timeout='10s'").await?;
            let current=scope.lock(&tx,audit).await?;
            let old=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT value AS ratio,updated_at FROM system_settings WHERE key=$1 AND is_sensitive IS FALSE FOR UPDATE",[setting_keys::NODE_TIP_RATIO.into()]))
                .one(&tx).await?.ok_or_else(||DbError::Other("tip_ratio_unavailable".into()))?;
            scope.current_actor(&tx).await?;
            if old.updated_at!=command.expected_updated_at {return Err(DbError::Other("tip_ratio_revision_conflict".into()));}
            if Self::normalize(&old.ratio).ok().as_deref()==Some(ratio.as_str()){return Ok(old);}
            let row=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
                "UPDATE system_settings SET value=$2,updated_at=GREATEST(clock_timestamp(),updated_at+interval '1 microsecond') WHERE key=$1 AND updated_at=$3 RETURNING value AS ratio,updated_at",
                [setting_keys::NODE_TIP_RATIO.into(),ratio.into(),command.expected_updated_at.into()]))
                .one(&tx).await?.ok_or_else(||DbError::Other("tip_ratio_revision_conflict".into()))?;
            TenantAuditEvent::append(&tx,AuditScopeType::Platform,None,&current,"settings.node_tip_ratio","system_setting",Some(setting_keys::NODE_TIP_RATIO),AuditResult::Success,
                serde_json::json!({"before":{"ratio":old.ratio},"after":{"ratio":row.ratio},"reason":reason})).await?;
            scope.current_actor(&tx).await?;
            Ok::<_,DbError>(row)
        }.await;
        match result {
            Ok(row) => {
                tx.commit().await?;
                Ok(row)
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ratio_precision_matches_the_immutable_ledger_domain() {
        for value in ["0", "-0.1", "1.0001", "0.12345", "invalid"] {
            assert!(TipRatioSetting::normalize(value).is_err());
        }
        assert_eq!(TipRatioSetting::normalize("0.90000").unwrap(), "0.9");
        assert_eq!(TipRatioSetting::normalize(" 0.0001 ").unwrap(), "0.0001");
        assert_eq!(TipRatioSetting::normalize("1").unwrap(), "1");
    }
}
