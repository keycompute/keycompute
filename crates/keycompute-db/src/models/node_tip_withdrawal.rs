//! Immutable tenant-owned withdrawal intents; no secret-bearing list projections.
use super::{
    financial_scope::FinancialScope,
    node_tip::{self, NodeTip},
};
use crate::{AuditContext, DbError, TenantAuditEvent, UserBalance};
use chrono::{DateTime, Utc};
use keycompute_types::{AuditResult, AuditScopeType};
use rust_decimal::Decimal;
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const WITHDRAWAL_TYPE_ALIPAY: &str = "alipay";
pub const WITHDRAWAL_TYPE_BALANCE: &str = "balance";
pub const WITHDRAWAL_STATUS_PENDING: &str = "pending";
pub const WITHDRAWAL_STATUS_APPROVED: &str = "approved";
pub const WITHDRAWAL_STATUS_COMPLETED: &str = "completed";
pub const WITHDRAWAL_STATUS_REJECTED: &str = "rejected";

#[derive(Clone, FromQueryResult)]
pub struct NodeTipWithdrawal {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub owner_user_id: Uuid,
    pub request_id: Uuid,
    request_fingerprint: String,
    pub withdrawal_type: String,
    pub total_amount: Decimal,
    pub currency: String,
    encrypted_alipay_account: Option<String>,
    encrypted_real_name: Option<String>,
    pub status: String,
    pub admin_id: Option<Uuid>,
    pub admin_remark: Option<String>,
    pub payout_reference: Option<String>,
    pub balance_transaction_id: Option<Uuid>,
    pub revision: i64,
    pub actioned_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct WithdrawalView {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub owner_user_id: Uuid,
    pub request_id: Uuid,
    pub withdrawal_type: String,
    pub total_amount: Decimal,
    pub currency: String,
    pub status: String,
    pub payout_details_present: bool,
    pub admin_id: Option<Uuid>,
    pub admin_remark: Option<String>,
    pub payout_reference: Option<String>,
    pub balance_transaction_id: Option<Uuid>,
    pub revision: i64,
    pub actioned_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone)]
pub struct WithdrawalFilter {
    pub status: Option<String>,
    pub currency: Option<String>,
    pub limit: i64,
    pub offset: i64,
}
#[derive(Clone)]
pub struct WithdrawalIntent {
    pub request_id: Uuid,
    pub withdrawal_type: String,
    pub currency: String,
    pub recipient_fingerprint: String,
    pub encrypted_alipay_account: Option<String>,
    pub encrypted_real_name: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithdrawalReview {
    Approve,
    Reject,
}
#[derive(Debug, Clone)]
pub struct ReviewWithdrawal {
    pub id: Uuid,
    pub expected_revision: i64,
    pub action: WithdrawalReview,
    pub reason: String,
}
#[derive(Debug, Clone)]
pub struct CompleteWithdrawal {
    pub id: Uuid,
    pub expected_revision: i64,
    pub reason: String,
    pub payout_reference: String,
}
pub struct PayoutSecrets {
    pub alipay_account: String,
    pub real_name: String,
}
impl std::fmt::Debug for NodeTipWithdrawal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeTipWithdrawal")
            .field("id", &self.id)
            .field("tenant_id", &self.tenant_id)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for WithdrawalIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WithdrawalIntent")
            .field("request_id", &self.request_id)
            .field("payout_details", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for PayoutSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PayoutSecrets([REDACTED])")
    }
}
const VIEW: &str = "w.id,w.tenant_id,w.owner_user_id,w.request_id,w.withdrawal_type,w.total_amount,w.currency,w.status,(w.encrypted_alipay_account IS NOT NULL AND w.encrypted_real_name IS NOT NULL) AS payout_details_present,w.admin_id,w.admin_remark,w.payout_reference,w.balance_transaction_id,w.revision,w.actioned_at,w.completed_at,w.created_at,w.updated_at";
fn invalid(code: &str) -> DbError {
    DbError::Other(code.into())
}
fn reason(value: &str) -> Result<&str, DbError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 500 || value.chars().any(char::is_control) {
        return Err(invalid("financial_reason_invalid"));
    }
    Ok(value)
}
fn fingerprint(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    hex::encode(hash.finalize())
}
fn validate_filter(filter: &WithdrawalFilter) -> Result<Option<String>, DbError> {
    node_tip::page(filter.limit, filter.offset)?;
    if filter
        .status
        .as_deref()
        .is_some_and(|s| !matches!(s, "pending" | "approved" | "completed" | "rejected"))
    {
        return Err(invalid("withdrawal_status_invalid"));
    }
    filter
        .currency
        .as_deref()
        .map(node_tip::currency)
        .transpose()
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
async fn begin(
    db: &(impl ConnectionTrait + TransactionTrait),
) -> Result<DatabaseTransaction, DbError> {
    let tx = db.begin().await?;
    tx.execute_unprepared("SET LOCAL lock_timeout='3s'; SET LOCAL statement_timeout='10s'")
        .await?;
    Ok(tx)
}
async fn audit(
    tx: &DatabaseTransaction,
    actor: &AuditContext,
    row: &NodeTipWithdrawal,
    action: &str,
    why: &str,
) -> Result<(), DbError> {
    TenantAuditEvent::append(tx,AuditScopeType::Tenant,Some(row.tenant_id),actor,action,"tip_withdrawal",Some(&row.id.to_string()),AuditResult::Success,
        serde_json::json!({"owner_user_id":row.owner_user_id,"amount":row.total_amount.to_string(),"currency":row.currency,"status":row.status,"version":row.revision,"reason":why,"request_id":row.request_id})).await?;
    Ok(())
}
impl NodeTipWithdrawal {
    fn view(&self) -> WithdrawalView {
        WithdrawalView {
            id: self.id,
            tenant_id: self.tenant_id,
            owner_user_id: self.owner_user_id,
            request_id: self.request_id,
            withdrawal_type: self.withdrawal_type.clone(),
            total_amount: self.total_amount,
            currency: self.currency.clone(),
            status: self.status.clone(),
            payout_details_present: self.encrypted_alipay_account.is_some()
                && self.encrypted_real_name.is_some(),
            admin_id: self.admin_id,
            admin_remark: self.admin_remark.clone(),
            payout_reference: self.payout_reference.clone(),
            balance_transaction_id: self.balance_transaction_id,
            revision: self.revision,
            actioned_at: self.actioned_at,
            completed_at: self.completed_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
    pub fn recipient_fingerprint(
        kind: &str,
        account: Option<&str>,
        name: Option<&str>,
    ) -> Result<String, DbError> {
        match kind {
            "balance" if account.is_none() && name.is_none() => Ok(fingerprint(&["balance"])),
            "alipay" => {
                let a = account
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
                    .ok_or_else(|| invalid("payout_recipient_invalid"))?;
                let n = name
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
                    .ok_or_else(|| invalid("payout_recipient_invalid"))?;
                Ok(fingerprint(&["alipay", a, n]))
            }
            _ => Err(invalid("withdrawal_method_invalid")),
        }
    }
    pub async fn list_in_scope(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
        filter: &WithdrawalFilter,
    ) -> Result<Vec<WithdrawalView>, DbError> {
        scope.tenant_id()?;
        let money = validate_filter(filter)?;
        let mut values = scope.values();
        values.extend([
            filter.status.as_deref().into(),
            money.into(),
            filter.limit.into(),
            filter.offset.into(),
        ]);
        let sql = format!(
            "SELECT {VIEW} FROM node_tip_withdrawals w WHERE w.tenant_id=$8 {} AND ($10::text IS NULL OR w.status=$10) AND ($11::text IS NULL OR w.currency=$11) AND {} ORDER BY w.created_at DESC,w.id DESC LIMIT $12 OFFSET $13",
            scope.owner_predicate("w"),
            scope.predicate()
        );
        Ok(
            WithdrawalView::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .all(db)
            .await?,
        )
    }
    pub async fn count_in_scope(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
        filter: &WithdrawalFilter,
    ) -> Result<i64, DbError> {
        scope.tenant_id()?;
        let money = validate_filter(filter)?;
        let mut values = scope.values();
        values.extend([filter.status.as_deref().into(), money.into()]);
        let sql = format!(
            "SELECT (SELECT COUNT(*)::bigint FROM node_tip_withdrawals w WHERE w.tenant_id=$8 {} AND ($10::text IS NULL OR w.status=$10) AND ($11::text IS NULL OR w.currency=$11)) AS n WHERE {}",
            scope.owner_predicate("w"),
            scope.predicate()
        );
        Ok(db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?
            .ok_or_else(|| invalid("financial_authority_invalid"))?
            .try_get("", "n")?)
    }
    pub async fn find_in_scope(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
        id: Uuid,
    ) -> Result<Option<WithdrawalView>, DbError> {
        scope.tenant_id()?;
        let mut values = scope.values();
        values.push(id.into());
        let sql = format!(
            "SELECT {VIEW} FROM node_tip_withdrawals w WHERE w.tenant_id=$8 AND w.id=$10 {} AND {}",
            scope.owner_predicate("w"),
            scope.predicate()
        );
        Ok(
            WithdrawalView::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .one(db)
            .await?,
        )
    }
    async fn locked(
        tx: &DatabaseTransaction,
        scope: FinancialScope,
        id: Uuid,
    ) -> Result<Self, DbError> {
        scope.tenant_id()?;
        let mut values = scope.values();
        values.push(id.into());
        let sql = format!(
            "SELECT w.* FROM node_tip_withdrawals w WHERE w.tenant_id=$8 AND w.id=$10 {} AND {} FOR UPDATE",
            scope.owner_predicate("w"),
            scope.predicate()
        );
        Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .one(tx)
        .await?
        .ok_or_else(|| DbError::not_found("Tip withdrawal", id))
    }
}

impl Default for WithdrawalFilter {
    fn default() -> Self {
        Self {
            status: None,
            currency: None,
            limit: 50,
            offset: 0,
        }
    }
}
impl NodeTipWithdrawal {
    pub async fn create(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: FinancialScope,
        actor: &AuditContext,
        spec: &WithdrawalIntent,
    ) -> Result<WithdrawalView, DbError> {
        scope.require_personal()?;
        let tenant = scope.tenant_id()?;
        let money = node_tip::currency(&spec.currency)?;
        if spec.request_id.is_nil()
            || money != "CNY"
            || spec.recipient_fingerprint.len() != 64
            || !spec
                .recipient_fingerprint
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        {
            return Err(invalid("withdrawal_request_invalid"));
        }
        match spec.withdrawal_type.as_str() {
            "balance"
                if spec.encrypted_alipay_account.is_none()
                    && spec.encrypted_real_name.is_none() => {}
            "alipay"
                if spec
                    .encrypted_alipay_account
                    .as_ref()
                    .is_some_and(|v| !v.is_empty() && v.len() <= 4096)
                    && spec
                        .encrypted_real_name
                        .as_ref()
                        .is_some_and(|v| !v.is_empty() && v.len() <= 4096) => {}
            _ => return Err(invalid("withdrawal_request_invalid")),
        }
        let key = fingerprint(&[
            "tip-withdrawal-v2",
            &tenant.to_string(),
            &scope.user_id().to_string(),
            &spec.withdrawal_type,
            &money,
            &spec.recipient_fingerprint,
        ]);
        let tx = begin(db).await?;
        let result=async {
            let current=scope.lock(&tx,actor).await?;
            tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT pg_advisory_xact_lock(hashtextextended($1,7266))",[format!("{tenant}:{}:{money}",scope.user_id()).into()])).await?;
            scope.current_actor(&tx).await?;
            let old=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT * FROM node_tip_withdrawals WHERE tenant_id=$1 AND owner_user_id=$2 AND request_id=$3 FOR UPDATE",
                [tenant.into(),scope.user_id().into(),spec.request_id.into()])).one(&tx).await?;
            if let Some(old)=old {
                if old.request_fingerprint!=key {return Err(invalid("withdrawal_idempotency_conflict"));}
                scope.current_actor(&tx).await?;
                return Ok(old.view());
            }
            let amount=NodeTip::summary(&tx,scope,&money).await?.pending_amount;
            if amount<=Decimal::ZERO {return Err(invalid("no_withdrawable_tips"));}
            let mut row=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
                "INSERT INTO node_tip_withdrawals(tenant_id,owner_user_id,request_id,request_fingerprint,withdrawal_type,total_amount,currency,encrypted_alipay_account,encrypted_real_name) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING *",
                [tenant.into(),scope.user_id().into(),spec.request_id.into(),key.into(),spec.withdrawal_type.as_str().into(),amount.into(),money.as_str().into(),spec.encrypted_alipay_account.as_deref().into(),spec.encrypted_real_name.as_deref().into()])).one(&tx).await?.ok_or_else(||invalid("withdrawal_insert_failed"))?;
            if spec.withdrawal_type=="balance" {
                let (_,transaction)=UserBalance::credit_tips(&tx,scope.user_id(),tenant,amount,Some("Tenant node earnings converted to CNY balance")).await?;
                row=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
                    "UPDATE node_tip_withdrawals SET status='completed',balance_transaction_id=$4,completed_at=clock_timestamp(),actioned_at=clock_timestamp() WHERE tenant_id=$1 AND owner_user_id=$2 AND id=$3 AND status='pending' AND withdrawal_type='balance' RETURNING *",
                    [tenant.into(),scope.user_id().into(),row.id.into(),transaction.id.into()])).one(&tx).await?.ok_or_else(||invalid("withdrawal_state_conflict"))?;
            }
            audit(&tx,&current,&row,if spec.withdrawal_type=="balance" {"tips.convert"}else{"withdrawal.create"},"owner requested withdrawal").await?;
            // A balance or audit lock wait can outlive a signed credential.
            // Reject before committing rather than grant authority from entry time.
            scope.current_actor(&tx).await?;
            Ok(row.view())
        }.await;
        finish(tx, result).await
    }
    pub async fn review(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: FinancialScope,
        actor: &AuditContext,
        command: &ReviewWithdrawal,
    ) -> Result<WithdrawalView, DbError> {
        scope.require_admin()?;
        let why = reason(&command.reason)?;
        if command.id.is_nil() || command.expected_revision <= 0 {
            return Err(invalid("withdrawal_revision_invalid"));
        }
        let tx = begin(db).await?;
        let result=async {
            let current=scope.lock(&tx,actor).await?;
            let before=Self::locked(&tx,scope,command.id).await?;
            if before.revision!=command.expected_revision || before.status!="pending" || before.withdrawal_type!="alipay" {return Err(invalid("withdrawal_state_conflict"));}
            let status=match command.action {WithdrawalReview::Approve=>"approved",WithdrawalReview::Reject=>"rejected"};
            let mut values=scope.values();values.extend([command.id.into(),command.expected_revision.into(),status.into(),why.into()]);
            let sql=format!("UPDATE node_tip_withdrawals w SET status=$12,admin_id=$1,admin_remark=$13,actioned_at=clock_timestamp() WHERE w.tenant_id=$8 AND w.id=$10 AND w.revision=$11 AND w.status='pending' AND {} RETURNING w.*",scope.predicate());
            let row=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,sql,values)).one(&tx).await?.ok_or_else(||invalid("withdrawal_state_conflict"))?;
            audit(&tx,&current,&row,if command.action==WithdrawalReview::Approve {"withdrawal.approve"}else{"withdrawal.reject"},why).await?;
            // A balance or audit lock wait can outlive a signed credential.
            // Reject before committing rather than grant authority from entry time.
            scope.current_actor(&tx).await?;
            Ok(row.view())
        }.await;
        finish(tx, result).await
    }
    /// Records a root's explicit attestation of an external payout. This does
    /// not send money, invoke Alipay or infer success from an approval.
    pub async fn complete_external(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: FinancialScope,
        actor: &AuditContext,
        command: &CompleteWithdrawal,
    ) -> Result<WithdrawalView, DbError> {
        scope.require_root_tenant()?;
        let why = reason(&command.reason)?;
        let reference = reason(&command.payout_reference)?;
        if command.id.is_nil() || command.expected_revision <= 0 {
            return Err(invalid("withdrawal_revision_invalid"));
        }
        let tx = begin(db).await?;
        let result=async {
            let current=scope.lock(&tx,actor).await?;
            let before=Self::locked(&tx,scope,command.id).await?;
            if before.status=="completed" && before.payout_reference.as_deref()==Some(reference)
                && before.revision.checked_sub(1)==Some(command.expected_revision) {return Ok(before.view());}
            if before.revision!=command.expected_revision || before.status!="approved" || before.withdrawal_type!="alipay" {return Err(invalid("withdrawal_state_conflict"));}
            let mut values=scope.values();values.extend([command.id.into(),command.expected_revision.into(),reference.into(),why.into()]);
            let sql=format!("UPDATE node_tip_withdrawals w SET status='completed',payout_reference=$12,admin_id=$1,admin_remark=$13,completed_at=clock_timestamp(),actioned_at=clock_timestamp() WHERE w.tenant_id=$8 AND w.id=$10 AND w.revision=$11 AND w.status='approved' AND {} RETURNING w.*",scope.predicate());
            let row=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,sql,values)).one(&tx).await?.ok_or_else(||invalid("withdrawal_state_conflict"))?;
            audit(&tx,&current,&row,"withdrawal.complete",why).await?;
            // A balance or audit lock wait can outlive a signed credential.
            // Reject before committing rather than grant authority from entry time.
            scope.current_actor(&tx).await?;
            Ok(row.view())
        }.await;
        finish(tx, result).await
    }
    /// Only a current root with an explicit target and reason can obtain the
    /// encrypted payout fields. The server decrypts after this audit commits.
    pub async fn support_payout(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: FinancialScope,
        actor: &AuditContext,
        id: Uuid,
        why: &str,
    ) -> Result<PayoutSecrets, DbError> {
        scope.require_root_tenant()?;
        let why = reason(why)?;
        let tx = begin(db).await?;
        let result = async {
            let current = scope.lock(&tx, actor).await?;
            let row = Self::locked(&tx, scope, id).await?;
            if row.withdrawal_type != "alipay" {
                return Err(invalid("payout_details_unavailable"));
            }
            audit(&tx, &current, &row, "withdrawal.payout_access", why).await?;
            scope.current_actor(&tx).await?;
            Ok(PayoutSecrets {
                alipay_account: row
                    .encrypted_alipay_account
                    .ok_or_else(|| invalid("payout_details_unavailable"))?,
                real_name: row
                    .encrypted_real_name
                    .ok_or_else(|| invalid("payout_details_unavailable"))?,
            })
        }
        .await;
        finish(tx, result).await
    }
}
