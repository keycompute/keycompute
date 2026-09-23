//! Explicit read-only platform operations. No business objects or secrets are projected.
use crate::DbError;
use chrono::{DateTime, Duration, Utc};
use keycompute_types::{CredentialKind, PlatformRole, PlatformScope, TenantRole};
use sea_orm::{ConnectionTrait, DbBackend, Statement, Value};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub struct OperationsMembership {
    pub tenant_id: Uuid,
    pub tenant_role: TenantRole,
    pub tenant_authz_version: i64,
    pub membership_authz_version: i64,
}
#[derive(Debug, Clone, Copy)]
pub struct OperationsSession {
    pub credential_kind: CredentialKind,
    pub token_version: i32,
    pub expires_at: i64,
    pub selected: Option<OperationsMembership>,
}
/// Read capability constructed from central platform authorization, checked again in SQL.
#[derive(Debug, Clone, Copy)]
pub struct PlatformOperationsScope {
    platform: PlatformScope,
    session: OperationsSession,
}
#[derive(Debug, Clone, Copy)]
pub enum OperationsTarget {
    Platform,
    Tenant(Uuid),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantHealth {
    pub tenant_id: Uuid,
    pub name: String,
    pub slug: String,
    pub status: String,
    pub default_rpm_limit: i32,
    pub default_tpm_limit: i32,
    pub active_members: i64,
    pub active_admins: i64,
    pub suspended_members: i64,
    pub provider_accounts: i64,
    pub enabled_accounts: i64,
    pub online_nodes: i64,
    pub excluded_nodes: i64,
    pub queued_tasks: i64,
    pub leased_tasks: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantHealthPage {
    pub items: Vec<TenantHealth>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    pub as_of: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrencyOperations {
    pub currency: String,
    pub requests: i64,
    pub successful_requests: i64,
    pub total_tokens: String,
    pub billed_amount: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageOperations {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub currencies: Vec<CurrencyOperations>,
    pub as_of: DateTime<Utc>,
}
#[derive(Debug, Clone)]
pub struct TenantHealthQuery {
    pub search: Option<String>,
    pub status: Option<String>,
    pub limit: i64,
    pub offset: i64,
}
fn denied() -> DbError {
    DbError::Other("platform_operations_authority_invalid".into())
}
fn invalid() -> DbError {
    DbError::Other("platform_operations_query_invalid".into())
}
const AUTHORITY: &str = "EXISTS(SELECT 1 FROM users ops_actor WHERE ops_actor.id=$1 AND ops_actor.platform_role=$2 AND ops_actor.platform_role IN ('root','operator') AND ops_actor.status='active' AND ops_actor.token_version=$3 AND clock_timestamp()<to_timestamp($4::double precision) AND ($5::uuid IS NULL OR EXISTS(SELECT 1 FROM tenant_memberships ops_member JOIN tenants ops_selected ON ops_selected.id=ops_member.tenant_id WHERE ops_member.user_id=$1 AND ops_member.tenant_id=$5 AND ops_member.status='active' AND ops_member.tenant_role=$6 AND ops_member.authz_version=$8 AND ops_selected.status='active' AND ops_selected.authz_version=$7)))";
const HEALTH: &str = "t.id AS tenant_id,t.name,t.slug,t.status,t.default_rpm_limit,t.default_tpm_limit,
(SELECT COUNT(*)::bigint FROM tenant_memberships m JOIN users u ON u.id=m.user_id WHERE m.tenant_id=t.id AND m.status='active' AND u.status='active') AS active_members,
(SELECT COUNT(*)::bigint FROM tenant_memberships m JOIN users u ON u.id=m.user_id WHERE m.tenant_id=t.id AND m.status='active' AND m.tenant_role='admin' AND u.status='active') AS active_admins,
(SELECT COUNT(*)::bigint FROM tenant_memberships m WHERE m.tenant_id=t.id AND m.status='suspended') AS suspended_members,
(SELECT COUNT(*)::bigint FROM accounts a WHERE a.tenant_id=t.id) AS provider_accounts,
(SELECT COUNT(*)::bigint FROM accounts a WHERE a.tenant_id=t.id AND a.enabled) AS enabled_accounts,
(SELECT COUNT(*)::bigint FROM nodes n WHERE n.tenant_id=t.id AND n.status='online') AS online_nodes,
(SELECT COUNT(*)::bigint FROM nodes n WHERE n.tenant_id=t.id AND n.status='excluded') AS excluded_nodes,
(SELECT COUNT(*)::bigint FROM node_tasks n WHERE n.tenant_id=t.id AND n.status='queued') AS queued_tasks,
(SELECT COUNT(*)::bigint FROM node_tasks n WHERE n.tenant_id=t.id AND n.status='leased') AS leased_tasks";
impl PlatformOperationsScope {
    pub fn checked(platform: PlatformScope, session: OperationsSession) -> Result<Self, DbError> {
        if platform.user_id().is_nil()
            || !matches!(
                platform.platform_role(),
                PlatformRole::Root | PlatformRole::Operator
            )
            || session.credential_kind != CredentialKind::Jwt
            || session.token_version < 0
            || session.expires_at <= Utc::now().timestamp()
            || session.selected.is_some_and(|m| {
                m.tenant_id.is_nil()
                    || m.tenant_authz_version <= 0
                    || m.membership_authz_version <= 0
            })
        {
            return Err(denied());
        }
        Ok(Self { platform, session })
    }
    fn values(self) -> Vec<Value> {
        let member = self.session.selected;
        vec![
            self.platform.user_id().into(),
            self.platform.platform_role().as_str().into(),
            self.session.token_version.into(),
            self.session.expires_at.into(),
            member.map(|m| m.tenant_id).into(),
            member.map(|m| m.tenant_role.as_str()).into(),
            member.map(|m| m.tenant_authz_version).into(),
            member.map(|m| m.membership_authz_version).into(),
        ]
    }
    /// Primary validation for in-process diagnostics, without global locks or cached authority.
    pub async fn validate_current(self, db: &impl ConnectionTrait) -> Result<(), DbError> {
        if db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!("SELECT 1 WHERE {AUTHORITY}"),
                self.values(),
            ))
            .await?
            .is_none()
        {
            return Err(denied());
        }
        Ok(())
    }
    pub async fn tenants(
        self,
        db: &impl ConnectionTrait,
        query: &TenantHealthQuery,
    ) -> Result<TenantHealthPage, DbError> {
        if !(1..=100).contains(&query.limit)
            || !(0..=1_000_000).contains(&query.offset)
            || query
                .status
                .as_deref()
                .is_some_and(|s| !matches!(s, "active" | "inactive"))
            || query
                .search
                .as_ref()
                .is_some_and(|s| s.len() > 128 || s.chars().any(char::is_control))
        {
            return Err(invalid());
        }
        let search = query.search.as_deref().unwrap_or("").trim();
        let mut values = self.values();
        values.extend([
            search.into(),
            query.status.as_deref().into(),
            query.limit.into(),
            query.offset.into(),
        ]);
        let sql = format!(
            "WITH authority AS MATERIALIZED (SELECT 1 AS granted WHERE {AUTHORITY}), visible AS MATERIALIZED (SELECT t.id,t.name,t.slug,t.status,t.default_rpm_limit,t.default_tpm_limit,t.created_at FROM tenants t CROSS JOIN authority WHERE ($9='' OR strpos(lower(t.name),lower($9))>0 OR strpos(lower(t.slug),lower($9))>0) AND ($10::text IS NULL OR t.status=$10)), selected AS (SELECT * FROM visible ORDER BY created_at DESC,id DESC LIMIT $11 OFFSET $12), health AS (SELECT {HEALTH},t.created_at FROM selected t) SELECT (SELECT COUNT(*)::bigint FROM visible) AS total,(SELECT COALESCE(jsonb_agg(to_jsonb(h)-'created_at' ORDER BY h.created_at DESC,h.tenant_id DESC),'[]'::jsonb) FROM health h) AS items,statement_timestamp() AS as_of FROM authority"
        );
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?
            .ok_or_else(denied)?;
        let items = serde_json::from_value(row.try_get("", "items")?).map_err(|_| invalid())?;
        Ok(TenantHealthPage {
            items,
            total: row.try_get("", "total")?,
            limit: query.limit,
            offset: query.offset,
            as_of: row.try_get("", "as_of")?,
        })
    }
    pub async fn tenant(
        self,
        db: &impl ConnectionTrait,
        tenant: Uuid,
    ) -> Result<TenantHealth, DbError> {
        if tenant.is_nil() {
            return Err(invalid());
        }
        let mut values = self.values();
        values.push(tenant.into());
        let sql = format!(
            "WITH authority AS MATERIALIZED (SELECT 1 WHERE {AUTHORITY}), health AS (SELECT {HEALTH} FROM tenants t CROSS JOIN authority WHERE t.id=$9) SELECT (SELECT to_jsonb(h) FROM health h) AS item FROM authority"
        );
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?
            .ok_or_else(denied)?;
        let item: Option<serde_json::Value> = row.try_get("", "item")?;
        serde_json::from_value(item.ok_or_else(|| DbError::not_found("Tenant", tenant))?)
            .map_err(|_| invalid())
    }
    pub async fn usage(
        self,
        db: &impl ConnectionTrait,
        target: OperationsTarget,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<UsageOperations, DbError> {
        if from >= to || to.signed_duration_since(from) > Duration::days(31) {
            return Err(invalid());
        }
        let mut values = self.values();
        values.extend([from.into(), to.into()]);
        let (tenant_filter, existence) = match target {
            OperationsTarget::Platform => ("TRUE", "TRUE"),
            OperationsTarget::Tenant(id) => {
                if id.is_nil() {
                    return Err(invalid());
                }
                values.push(id.into());
                (
                    "u.tenant_id=$11",
                    "EXISTS(SELECT 1 FROM tenants WHERE id=$11)",
                )
            }
        };
        let sql = format!(
            "WITH authority AS MATERIALIZED (SELECT 1 WHERE {AUTHORITY}), aggregate AS (SELECT u.currency,COUNT(*)::bigint AS requests,COUNT(*) FILTER(WHERE u.status='success')::bigint AS successful_requests,COALESCE(SUM(u.total_tokens::numeric),0)::text AS total_tokens,COALESCE(SUM(u.user_amount),0)::text AS billed_amount FROM usage_logs u CROSS JOIN authority WHERE u.created_at >= $9 AND u.created_at < $10 AND {tenant_filter} GROUP BY u.currency) SELECT {existence} AS target_exists,(SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY a.currency),'[]'::jsonb) FROM aggregate a) AS currencies,statement_timestamp() AS as_of FROM authority"
        );
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?
            .ok_or_else(denied)?;
        if !row.try_get::<bool>("", "target_exists")? {
            return Err(DbError::not_found("Tenant", "target"));
        }
        Ok(UsageOperations {
            from,
            to,
            currencies: serde_json::from_value(row.try_get("", "currencies")?)
                .map_err(|_| invalid())?,
            as_of: row.try_get("", "as_of")?,
        })
    }
}

impl Default for TenantHealthQuery {
    fn default() -> Self {
        Self {
            search: None,
            status: None,
            limit: 20,
            offset: 0,
        }
    }
}
