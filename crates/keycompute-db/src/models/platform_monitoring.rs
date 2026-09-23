//! Root-only raw diagnostics. Operator aggregates use platform_operations instead.
//! Read authorization rows are shared-locked, never the global administrative fence.
use super::financial_scope::{FinancialAccess, FinancialScope};
use crate::{Account, AuditContext, DbError, TenantAuditEvent};
use keycompute_types::{AuditResult, AuditScopeType};
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
pub enum MonitoringAction {
    Overview,
    Requests,
    Request,
    Summary,
    Targets,
    ProbeRequest,
    ProbeResult,
}
impl MonitoringAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Overview => "monitoring.overview",
            Self::Requests => "monitoring.requests",
            Self::Request => "monitoring.request",
            Self::Summary => "monitoring.summary",
            Self::Targets => "monitoring.targets",
            Self::ProbeRequest => "monitoring.probe_request",
            Self::ProbeResult => "monitoring.probe_result",
        }
    }
}
/// Only valid current root platform scopes may obtain this connection.
/// It must be committed with its audit before any result reaches the client.
pub struct MonitoringRead {
    tx: DatabaseTransaction,
    scope: FinancialScope,
    actor: AuditContext,
}
impl MonitoringRead {
    pub async fn begin(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: FinancialScope,
        audit: &AuditContext,
    ) -> Result<Self, DbError> {
        scope.require_root_global()?;
        let tx = db.begin().await?;
        tx.execute_unprepared("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ; SET LOCAL lock_timeout='1000ms'; SET LOCAL statement_timeout='5000ms'").await?;
        let actor = scope.lock_root_read(&tx, audit).await?;
        Ok(Self { tx, scope, actor })
    }
    pub fn connection(&self) -> &DatabaseTransaction {
        &self.tx
    }
    pub async fn finish(
        self,
        action: MonitoringAction,
        resource: Option<Uuid>,
        metadata: Value,
    ) -> Result<(), DbError> {
        self.scope.check_expiry()?;
        TenantAuditEvent::append(
            &self.tx,
            AuditScopeType::Platform,
            None,
            &self.actor,
            action.as_str(),
            "monitoring",
            resource.as_ref().map(|id| id.to_string()).as_deref(),
            AuditResult::Success,
            metadata,
        )
        .await?;
        self.scope.check_expiry()?;
        self.tx.commit().await?;
        Ok(())
    }
}
impl Account {
    /// Recheck each console probe step without selecting credentials a second time.
    /// The version is not the only evidence: legacy/internal SQL writers may
    /// change connection material without advancing the application revision.
    pub async fn console_probe_is_current(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
        account: &Account,
        enabled_only: bool,
    ) -> Result<bool, DbError> {
        if scope.access() != FinancialAccess::PlatformGlobal {
            scope.require_admin()?;
        }
        let mut values = scope.values();
        values.extend([
            account.id.into(),
            account.tenant_id.into(),
            account.upstream_config_version.into(),
            enabled_only.into(),
            account.endpoint.clone().into(),
            account.provider.clone().into(),
            account.upstream_api_key_encrypted.clone().into(),
            account.models_supported.clone().into(),
            account.api_capabilities.clone().into(),
        ]);
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM accounts a JOIN tenants owner ON owner.id=a.tenant_id WHERE a.id=$10 AND a.tenant_id=$11 AND a.upstream_config_version=$12 AND owner.status='active' AND (NOT $13::boolean OR a.enabled) AND a.endpoint=$14 AND a.provider=$15 AND a.upstream_api_key_encrypted=$16 AND a.models_supported=$17 AND a.api_capabilities=$18 AND ($8::uuid IS NULL OR a.tenant_id=$8) AND {}) AS allowed",
            scope.predicate()
        );
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?
            .ok_or_else(|| DbError::Other("probe authority state unavailable".into()))?;
        Ok(row.try_get("", "allowed")?)
    }

    /// Console probing material and its complete original authority are one primary query.
    /// The public monitoring lists never materialize this encrypted credential column.
    pub async fn load_console_probe(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
        id: Uuid,
    ) -> Result<Option<Account>, DbError> {
        if scope.access() != FinancialAccess::PlatformGlobal {
            scope.require_admin()?;
        }
        if id.is_nil() {
            return Err(DbError::Other("invalid monitoring account".into()));
        }
        let mut values = scope.values();
        values.push(id.into());
        let sql = format!(
            "SELECT a.* FROM accounts a JOIN tenants owner ON owner.id=a.tenant_id WHERE a.id=$10 AND owner.status='active' AND ($8::uuid IS NULL OR a.tenant_id=$8) AND {}",
            scope.predicate()
        );
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .one(db)
        .await?)
    }
}
