//! Transaction-bound distribution policy administration. Never used by settlement.
use super::{
    tenant::Tenant,
    tenant_audit_event::{AuditContext, TenantAuditEvent, lock_identity_admin},
    tenant_control::{self, TenantAuthzSnapshot},
    tenant_distribution_rule::{
        BeneficiaryScope, CreateDistributionRuleRequest, TenantDistributionRule,
    },
    user::User,
};
use crate::DbError;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use keycompute_types::{
    AuditResult, AuditScopeType, CredentialKind, PlatformRole, PlatformScope, TenantRole,
    TenantScope,
};
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
enum Authority {
    Tenant(TenantScope, TenantAuthzSnapshot),
    Platform(PlatformScope, i32),
}
/// A selector and signed versions, not a grant by itself. Live authority is
/// checked under locks on every operation, including nested transactions.
#[derive(Debug, Clone, Copy)]
pub struct PolicyActor {
    tenant: Uuid,
    authority: Authority,
}
impl PolicyActor {
    pub fn tenant(scope: TenantScope, snapshot: TenantAuthzSnapshot) -> Result<Self, DbError> {
        if scope.tenant_role() != TenantRole::Admin
            || snapshot.token_version < 0
            || snapshot.tenant_authz_version <= 0
            || snapshot.membership_authz_version <= 0
        {
            return Err(denied());
        }
        Ok(Self {
            tenant: scope.tenant_id(),
            authority: Authority::Tenant(scope, snapshot),
        })
    }
    pub fn platform(
        scope: PlatformScope,
        tenant: Uuid,
        token_version: i32,
    ) -> Result<Self, DbError> {
        if scope.platform_role() != PlatformRole::Root || tenant.is_nil() || token_version < 0 {
            return Err(denied());
        }
        Ok(Self {
            tenant,
            authority: Authority::Platform(scope, token_version),
        })
    }
    fn user(self) -> Uuid {
        match self.authority {
            Authority::Tenant(s, _) => s.user_id(),
            Authority::Platform(s, _) => s.user_id(),
        }
    }
}
#[derive(Debug, Clone)]
pub struct PolicyPatch {
    pub expected_updated_at: DateTime<Utc>,
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub commission_rate: Option<BigDecimal>,
    pub priority: Option<i32>,
    pub is_active: Option<bool>,
    pub effective_until: Option<Option<DateTime<Utc>>>,
}
impl PolicyPatch {
    pub fn empty(expected_updated_at: DateTime<Utc>) -> Self {
        Self {
            expected_updated_at,
            name: None,
            description: None,
            commission_rate: None,
            priority: None,
            is_active: None,
            effective_until: None,
        }
    }
}
fn invalid(message: &str) -> DbError {
    DbError::Other(format!("invalid distribution policy: {message}"))
}
fn denied() -> DbError {
    DbError::Other("distribution policy authorization denied".into())
}
fn conflict(id: Uuid) -> DbError {
    DbError::OptimisticConflict {
        entity: "distribution policy".into(),
        id: id.to_string(),
    }
}
fn reason(value: &str) -> Result<&str, DbError> {
    let v = value.trim();
    if v.is_empty() || v.chars().count() > 500 || v.chars().any(char::is_control) {
        return Err(invalid("bounded reason required"));
    }
    Ok(v)
}
pub fn validate_rate(value: &BigDecimal) -> Result<(), DbError> {
    if value < &BigDecimal::from(0)
        || value > &BigDecimal::from(1)
        || value.normalized().as_bigint_and_exponent().1 > 4
    {
        return Err(invalid(
            "commission rate must be within 0..1 with at most four decimal places",
        ));
    }
    Ok(())
}
fn normalize(rule: &mut TenantDistributionRule) -> Result<(), DbError> {
    rule.name = rule.name.trim().to_owned();
    if rule.name.is_empty()
        || rule.name.chars().count() > 255
        || rule.name.chars().any(char::is_control)
    {
        return Err(invalid("name must contain 1..255 printable characters"));
    }
    if rule.description.as_ref().is_some_and(|v| {
        v.chars().count() > 4096 || v.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
    }) {
        return Err(invalid("description is invalid or too long"));
    }
    validate_rate(&rule.commission_rate)?;
    if !(-1000..=1000).contains(&rule.priority) {
        return Err(invalid("priority must be within -1000..1000"));
    }
    if rule
        .effective_until
        .is_some_and(|v| v <= rule.effective_from)
    {
        return Err(invalid("invalid effective interval"));
    }
    match (rule.beneficiary_scope, rule.beneficiary_id) {
        (BeneficiaryScope::Everyone, None) => {}
        (BeneficiaryScope::TenantMember, Some(id)) if !id.is_nil() => {}
        _ => return Err(invalid("beneficiary scope and ID do not agree")),
    }
    Ok(())
}
async fn lock(
    tx: &DatabaseTransaction,
    who: PolicyActor,
    audit: &AuditContext,
) -> Result<(Tenant, AuditContext), DbError> {
    if audit.credential_kind != CredentialKind::Jwt
        || audit.actor_user_id != who.user()
        || audit.request_id.is_none()
        || audit.request_id.is_some_and(|v| v.is_nil())
    {
        return Err(denied());
    }
    let (tenant, actor) = match who.authority {
        Authority::Tenant(scope, snapshot) => {
            let authority =
                tenant_control::revalidate_in_transaction(tx, scope, snapshot, audit).await?;
            let tenant = Tenant::find_by_id(tx, who.tenant)
                .await?
                .ok_or_else(denied)?;
            (tenant, authority.actor)
        }
        Authority::Platform(scope, version) => {
            lock_identity_admin(tx).await?;
            let tenant = Tenant::find_by_id_for_update(tx, who.tenant)
                .await?
                .ok_or_else(|| DbError::not_found("Tenant", who.tenant))?;
            let user = User::find_by_id_for_update(tx, scope.user_id())
                .await?
                .filter(|u| {
                    u.status == "active" && u.platform_role == "root" && u.token_version == version
                })
                .ok_or_else(denied)?;
            (
                tenant,
                AuditContext {
                    actor_platform_role: user.platform_role()?,
                    actor_tenant_role: None,
                    ..*audit
                },
            )
        }
    };
    // Same group lock as default initialization; acquired before any policy row.
    tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtext('kc_dist_rule_upsert'),hashtext($1))",
        [who.tenant.to_string().into()],
    ))
    .await?;
    Ok((tenant, actor))
}
async fn begin(
    db: &(impl ConnectionTrait + TransactionTrait),
) -> Result<DatabaseTransaction, DbError> {
    let tx = db.begin().await?;
    tx.execute_unprepared("SET LOCAL lock_timeout='3s'; SET LOCAL statement_timeout='8s'")
        .await?;
    Ok(tx)
}
async fn finish<T>(tx: DatabaseTransaction, result: Result<T, DbError>) -> Result<T, DbError> {
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
async fn target(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    id: Uuid,
) -> Result<TenantDistributionRule, DbError> {
    TenantDistributionRule::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM tenant_distribution_rules WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
        [tenant.into(), id.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| DbError::not_found("DistributionRule", id))
}
async fn validate_live_target(
    tx: &DatabaseTransaction,
    tenant: &Tenant,
    row: &TenantDistributionRule,
) -> Result<(), DbError> {
    if row.is_active && !tenant.is_active() {
        return Err(invalid("active policy requires an active tenant"));
    }
    if row.is_active
        && let Some(id) = row.beneficiary_id
    {
        // The identity administration fence prevents concurrent removal/status changes.
        let member = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT 1 FROM tenant_memberships m JOIN users u ON u.id=m.user_id WHERE m.tenant_id=$1 AND m.user_id=$2 AND m.status='active' AND u.status='active'",
            [tenant.id.into(), id.into()])).await?;
        if member.is_none() {
            return Err(invalid(
                "beneficiary must be an active member of the target tenant",
            ));
        }
    }
    if row.is_active
        && row.beneficiary_scope == BeneficiaryScope::Everyone
        && row.priority == TenantDistributionRule::GLOBAL_OVERRIDE_PRIORITY
    {
        let duplicate = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT 1 FROM tenant_distribution_rules WHERE tenant_id=$1 AND beneficiary_scope='everyone' AND beneficiary_id IS NULL AND priority=100 AND is_active AND id<>$2 LIMIT 1",
            [tenant.id.into(), row.id.into()])).await?;
        if duplicate.is_some() {
            return Err(conflict(row.id));
        }
    }
    Ok(())
}
fn snapshot(row: &TenantDistributionRule) -> Value {
    // Never include caller-controlled name/description in the audit payload.
    json!({"tenant_id":row.tenant_id,"beneficiary_scope":row.beneficiary_scope,
        "beneficiary_id":row.beneficiary_id,"commission_rate":row.commission_rate.to_string(),
        "priority":row.priority,"is_active":row.is_active,"effective_from":row.effective_from,
        "effective_until":row.effective_until,"updated_at":row.updated_at})
}
async fn record(
    tx: &DatabaseTransaction,
    who: PolicyActor,
    audit: &AuditContext,
    action: &str,
    before: Option<&TenantDistributionRule>,
    after: Option<&TenantDistributionRule>,
    why: &str,
) -> Result<(), DbError> {
    let id = after.or(before).expect("policy mutation has a resource").id;
    let meta = json!({"reason":why,"before":before.map(snapshot),"after":after.map(snapshot),
        "name_changed":before.map(|b| after.is_some_and(|a| a.name!=b.name)).unwrap_or(after.is_some()),
        "description_changed":before.map(|b| after.is_some_and(|a| a.description!=b.description)).unwrap_or(after.is_some())});
    TenantAuditEvent::append(
        tx,
        AuditScopeType::Tenant,
        Some(who.tenant),
        audit,
        action,
        "distribution_rule",
        Some(&id.to_string()),
        AuditResult::Success,
        meta,
    )
    .await?;
    Ok(())
}
async fn insert(
    tx: &DatabaseTransaction,
    row: &TenantDistributionRule,
) -> Result<TenantDistributionRule, DbError> {
    TenantDistributionRule::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO tenant_distribution_rules(id,tenant_id,beneficiary_scope,beneficiary_id,name,description,commission_rate,priority,is_active,effective_from,effective_until) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) RETURNING *",
        [row.id.into(),row.tenant_id.into(),row.beneficiary_scope.as_str().into(),row.beneficiary_id.into(),
        row.name.clone().into(),row.description.clone().into(),row.commission_rate.clone().into(),row.priority.into(),
        row.is_active.into(),row.effective_from.into(),row.effective_until.into()])).one(tx).await?
        .ok_or_else(|| DbError::Other("distribution insert returned no row".into()))
}
async fn update_row(
    tx: &DatabaseTransaction,
    before: &TenantDistributionRule,
    after: &TenantDistributionRule,
) -> Result<TenantDistributionRule, DbError> {
    TenantDistributionRule::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE tenant_distribution_rules SET name=$4,description=$5,commission_rate=$6,priority=$7,is_active=$8,effective_until=$9,updated_at=GREATEST(clock_timestamp(),updated_at+interval '1 microsecond') WHERE tenant_id=$1 AND id=$2 AND updated_at=$3 RETURNING *",
        [before.tenant_id.into(),before.id.into(),before.updated_at.into(),after.name.clone().into(),
        after.description.clone().into(),after.commission_rate.clone().into(),after.priority.into(),after.is_active.into(),after.effective_until.into()]))
        .one(tx).await?.ok_or_else(|| conflict(before.id))
}
fn new_row(req: &CreateDistributionRuleRequest) -> TenantDistributionRule {
    let now = Utc::now();
    TenantDistributionRule {
        id: Uuid::new_v4(),
        tenant_id: req.tenant_id,
        beneficiary_scope: req.beneficiary_scope,
        beneficiary_id: req.beneficiary_id,
        name: req.name.clone(),
        description: req.description.clone(),
        commission_rate: req.commission_rate.clone(),
        priority: req.priority.unwrap_or(0),
        is_active: true,
        effective_from: req.effective_from.unwrap_or(now),
        effective_until: req.effective_until,
        created_at: now,
        updated_at: now,
    }
}
pub async fn create(
    db: &(impl ConnectionTrait + TransactionTrait),
    who: PolicyActor,
    audit: &AuditContext,
    req: &CreateDistributionRuleRequest,
    why: &str,
) -> Result<TenantDistributionRule, DbError> {
    let why = reason(why)?;
    if req.tenant_id != who.tenant {
        return Err(denied());
    }
    let mut row = new_row(req);
    normalize(&mut row)?;
    let tx = begin(db).await?;
    let result = async {
        let (tenant, actor) = lock(&tx, who, audit).await?;
        validate_live_target(&tx, &tenant, &row).await?;
        let row = insert(&tx, &row).await?;
        record(
            &tx,
            who,
            &actor,
            "distribution.rule.create",
            None,
            Some(&row),
            why,
        )
        .await?;
        Ok(row)
    }
    .await;
    finish(tx, result).await
}
pub async fn update(
    db: &(impl ConnectionTrait + TransactionTrait),
    who: PolicyActor,
    audit: &AuditContext,
    id: Uuid,
    patch: &PolicyPatch,
    why: &str,
) -> Result<TenantDistributionRule, DbError> {
    let why = reason(why)?;
    let tx = begin(db).await?;
    let result = async {
        let (tenant, actor) = lock(&tx, who, audit).await?;
        let before = target(&tx, who.tenant, id).await?;
        if before.updated_at != patch.expected_updated_at {
            return Err(conflict(id));
        }
        let mut after = before.clone();
        if let Some(v) = &patch.name {
            after.name = v.clone();
        }
        if let Some(v) = &patch.description {
            after.description = v.clone();
        }
        if let Some(v) = &patch.commission_rate {
            after.commission_rate = v.clone();
        }
        if let Some(v) = patch.priority {
            after.priority = v;
        }
        if let Some(v) = patch.is_active {
            after.is_active = v;
        }
        if let Some(v) = patch.effective_until {
            after.effective_until = v;
        }
        normalize(&mut after)?;
        validate_live_target(&tx, &tenant, &after).await?;
        if after.name == before.name
            && after.description == before.description
            && after.commission_rate == before.commission_rate
            && after.priority == before.priority
            && after.is_active == before.is_active
            && after.effective_until == before.effective_until
        {
            return Ok(before);
        }
        let after = update_row(&tx, &before, &after).await?;
        record(
            &tx,
            who,
            &actor,
            "distribution.rule.update",
            Some(&before),
            Some(&after),
            why,
        )
        .await?;
        Ok(after)
    }
    .await;
    finish(tx, result).await
}
pub async fn delete(
    db: &(impl ConnectionTrait + TransactionTrait),
    who: PolicyActor,
    audit: &AuditContext,
    id: Uuid,
    expected_updated_at: DateTime<Utc>,
    why: &str,
) -> Result<(), DbError> {
    let why = reason(why)?;
    let tx = begin(db).await?;
    let result = async {
        let (_, actor) = lock(&tx, who, audit).await?;
        let before = target(&tx, who.tenant, id).await?;
        if before.updated_at != expected_updated_at {
            return Err(conflict(id));
        }
        let n=tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "DELETE FROM tenant_distribution_rules WHERE tenant_id=$1 AND id=$2 AND updated_at=$3",
            [who.tenant.into(),id.into(),expected_updated_at.into()])).await?.rows_affected();
        if n != 1 {
            return Err(conflict(id));
        }
        record(
            &tx,
            who,
            &actor,
            "distribution.rule.delete",
            Some(&before),
            None,
            why,
        )
        .await?;
        Ok(())
    }
    .await;
    finish(tx, result).await
}
/// Preserve the existing default-override upsert contract with current authority,
/// deterministic duplicate deactivation, no-op idempotency and atomic audit.
pub async fn upsert_default(
    db: &(impl ConnectionTrait + TransactionTrait),
    who: PolicyActor,
    audit: &AuditContext,
    name: &str,
    rate: BigDecimal,
    why: &str,
) -> Result<TenantDistributionRule, DbError> {
    let why = reason(why)?;
    let req = CreateDistributionRuleRequest {
        tenant_id: who.tenant,
        beneficiary_scope: BeneficiaryScope::Everyone,
        beneficiary_id: None,
        name: name.into(),
        description: None,
        commission_rate: rate,
        priority: Some(100),
        effective_from: None,
        effective_until: None,
    };
    let mut proposed = new_row(&req);
    normalize(&mut proposed)?;
    let tx = begin(db).await?;
    let result=async {
        let (tenant,actor)=lock(&tx,who,audit).await?;
        let rows=TenantDistributionRule::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT * FROM tenant_distribution_rules WHERE tenant_id=$1 AND beneficiary_scope='everyone' AND beneficiary_id IS NULL AND priority=100 ORDER BY created_at,id LIMIT 1001 FOR UPDATE",
            [who.tenant.into()])).all(&tx).await?;
        if rows.len()>1000 { return Err(invalid("too many duplicate defaults; manual policy review required")); }
        let mut rows=rows.into_iter();
        let before=rows.next();
        for extra in rows.filter(|r|r.is_active) {
            let mut disabled=extra.clone(); disabled.is_active=false;
            let disabled=update_row(&tx,&extra,&disabled).await?;
            record(&tx,who,&actor,"distribution.rule.disable_duplicate",Some(&extra),Some(&disabled),why).await?;
        }
        if let Some(before)=before {
            let mut after=before.clone(); after.name=proposed.name; after.commission_rate=proposed.commission_rate;
            after.is_active=true; after.effective_until=None;
            normalize(&mut after)?; validate_live_target(&tx,&tenant,&after).await?;
            if after.name==before.name && after.commission_rate==before.commission_rate && before.is_active && before.effective_until.is_none() { return Ok(before); }
            let after=update_row(&tx,&before,&after).await?;
            record(&tx,who,&actor,"distribution.rule.default",Some(&before),Some(&after),why).await?;
            Ok(after)
        } else {
            validate_live_target(&tx,&tenant,&proposed).await?;
            let after=insert(&tx,&proposed).await?;
            record(&tx,who,&actor,"distribution.rule.default",None,Some(&after),why).await?;
            Ok(after)
        }
    }.await;
    finish(tx, result).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rates_are_exact_and_reasons_are_bounded() {
        for value in ["0", "1", "0.0001", "0.03000"] {
            validate_rate(&value.parse().unwrap()).unwrap();
        }
        for value in ["-0.1", "1.0001", "0.00001"] {
            assert!(validate_rate(&value.parse().unwrap()).is_err());
        }
        assert!(reason(" ").is_err());
        assert!(reason("ok\nnot ok").is_err());
        assert!(reason(&"x".repeat(501)).is_err());
    }
    #[test]
    fn roles_and_invalid_versions_do_not_construct_write_scopes() {
        let t = Uuid::new_v4();
        let u = Uuid::new_v4();
        let s = TenantScope::checked(t, u, TenantRole::Member).unwrap();
        let v = TenantAuthzSnapshot {
            token_version: 0,
            tenant_authz_version: 1,
            membership_authz_version: 1,
        };
        assert!(PolicyActor::tenant(s, v).is_err());
        assert!(
            PolicyActor::platform(
                PlatformScope::checked(u, PlatformRole::Operator).unwrap(),
                t,
                0
            )
            .is_err()
        );
        assert!(
            PolicyActor::platform(
                PlatformScope::checked(u, PlatformRole::Root).unwrap(),
                Uuid::nil(),
                0
            )
            .is_err()
        );
    }
}
