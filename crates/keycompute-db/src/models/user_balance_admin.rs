//! Current platform authority and immutable manual-wallet operations.
use super::*;
use crate::models::financial_scope::FinancialScope;
use crate::{AuditContext, TenantAuditEvent};
use keycompute_types::{AuditResult, AuditScopeType};

/// Only the explicit financial scope supplies tenant and actor identity.
/// This command is intentionally not deserializable or Debug: the key is secret.
pub struct ManualBalanceCommand<'a> {
    pub kind: ManualBalanceOperationKind,
    pub user_id: Uuid,
    pub amount: Decimal,
    pub reason: &'a str,
    pub idempotency_key: &'a str,
}
enum ManualAttempt {
    Decision(ManualBalanceOperationDecision),
    // Existing expiry repair remains committed only with its denial audit.
    RepairedFailure(DbError),
}
async fn append_manual_audit(
    tx: &DatabaseTransaction,
    scope: FinancialScope,
    actor: &AuditContext,
    command: &ManualBalanceCommand<'_>,
    operation_id: Option<Uuid>,
    action: &str,
    result: AuditResult,
) -> Result<(), DbError> {
    TenantAuditEvent::append(
        tx,
        AuditScopeType::Tenant,
        Some(scope.tenant_id()?),
        actor,
        action,
        "balance",
        Some(&command.user_id.to_string()),
        result,
        serde_json::json!({"owner_user_id":command.user_id,"operation":command.kind.as_str(),
            "amount":command.amount.to_string(),"currency":"CNY","reason":command.reason.trim(),
            "operation_id":operation_id}),
    )
    .await?;
    Ok(())
}
impl UserBalance {
    /// Authenticate before claiming/replaying idempotency, and retain authority
    /// through wallet mutation and audit. Nested callers get a real savepoint.
    pub async fn apply_admin_manual_operation(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: FinancialScope,
        audit: &AuditContext,
        command: &ManualBalanceCommand<'_>,
    ) -> Result<ManualBalanceOperationDecision, DbError> {
        scope.require_root_tenant()?;
        if command.user_id.is_nil() {
            return Err(DbError::Other("financial_target_invalid".into()));
        }
        let tx = db.begin().await?;
        let result = async {
            tx.execute_unprepared("SET LOCAL lock_timeout='3s'; SET LOCAL statement_timeout='10s'")
                .await?;
            let result = Self::manual_balance_in_tx(&tx, scope, audit, command).await?;
            // Wall-clock expiry can change even while identity rows are locked.
            scope.current_actor(&tx).await?;
            Ok::<_, DbError>(result)
        }
        .await;
        match result {
            Ok(result) => {
                tx.commit().await?;
                match result {
                    ManualAttempt::Decision(d) => Ok(d),
                    ManualAttempt::RepairedFailure(e) => Err(e),
                }
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }
    async fn manual_balance_in_tx(
        tx: &DatabaseTransaction,
        scope: FinancialScope,
        audit: &AuditContext,
        command: &ManualBalanceCommand<'_>,
    ) -> Result<ManualAttempt, DbError> {
        let ManualBalanceCommand {
            kind,
            user_id,
            amount,
            reason,
            idempotency_key,
        } = *command;
        let tenant_id = scope.tenant_id()?;
        let actor_user_id = scope.user_id();
        if amount <= Decimal::ZERO
            || amount >= Decimal::from(10_000_000_000u64)
            || amount != amount.round_dp(10)
        {
            return Err(DbError::Other(
                "administrator balance operation amount must fit positive DECIMAL(20,10)"
                    .to_string(),
            ));
        }
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().any(char::is_control) {
            return Err(DbError::Other(
                "administrator balance operation reason must not be empty".to_string(),
            ));
        }
        if reason.chars().count() > MAX_ADMIN_BALANCE_OPERATION_REASON_CHARS {
            return Err(DbError::Other(format!(
                "administrator balance operation reason must not exceed {MAX_ADMIN_BALANCE_OPERATION_REASON_CHARS} characters"
            )));
        }
        if idempotency_key.is_empty()
            || idempotency_key.len() > MAX_ADMIN_BALANCE_IDEMPOTENCY_KEY_BYTES
            || !idempotency_key
                .bytes()
                .all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return Err(DbError::Other(format!(
                "administrator balance Idempotency-Key must contain between 1 and {MAX_ADMIN_BALANCE_IDEMPOTENCY_KEY_BYTES} visible ASCII bytes"
            )));
        }

        let key_hash = sha256_hex(idempotency_key.as_bytes());
        let fingerprint = manual_balance_request_fingerprint(
            kind,
            tenant_id,
            user_id,
            actor_user_id,
            amount,
            reason,
        );
        let current = scope.lock(tx, audit).await?;

        // PostgreSQL's unique-index conflict handling waits for an in-flight
        // claimant. The following SELECT is a new READ COMMITTED snapshot, so
        // a waiter observes the winner's completed row after it commits.
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO admin_balance_operations
               (idempotency_key_hash, request_fingerprint, operation_type,
                tenant_id, user_id, actor_user_id, amount, reason)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
               ON CONFLICT (idempotency_key_hash) DO NOTHING"#,
            [
                key_hash.clone().into(),
                fingerprint.clone().into(),
                kind.as_str().into(),
                tenant_id.into(),
                user_id.into(),
                actor_user_id.into(),
                amount.into(),
                reason.to_string().into(),
            ],
        ))
        .await?;

        let claim_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM admin_balance_operations WHERE idempotency_key_hash = $1 AND tenant_id=$2 AND user_id=$3 AND actor_user_id=$4 FOR UPDATE",
            [
                key_hash.clone().into(),
                tenant_id.into(),
                user_id.into(),
                actor_user_id.into(),
            ],
        );
        let Some(claim) = AdminBalanceOperation::find_by_statement(claim_stmt)
            .one(tx)
            .await?
        else {
            return Ok(ManualAttempt::Decision(
                ManualBalanceOperationDecision::Conflict,
            ));
        };
        if claim.idempotency_key_hash != key_hash {
            return Err(DbError::Other(
                "administrator balance idempotency claim key hash mismatch".to_string(),
            ));
        }
        if !claim.matches_request(
            &fingerprint,
            kind,
            tenant_id,
            user_id,
            actor_user_id,
            amount,
            reason,
        ) {
            return Ok(ManualAttempt::Decision(
                ManualBalanceOperationDecision::Conflict,
            ));
        }
        if let Some(outcome) = claim.completed_outcome()? {
            return Ok(ManualAttempt::Decision(
                ManualBalanceOperationDecision::Completed(outcome),
            ));
        }

        let operation = match kind {
            ManualBalanceOperationKind::Recharge => {
                Self::recharge_in_tx(tx, tenant_id, user_id, amount, None, Some(reason)).await
            }
            ManualBalanceOperationKind::Consume => {
                Self::consume_in_tx(tx, tenant_id, user_id, amount, None, Some(reason)).await
            }
            ManualBalanceOperationKind::Freeze => {
                Self::freeze_in_tx(tx, tenant_id, user_id, amount, Some(reason)).await
            }
            ManualBalanceOperationKind::Unfreeze => {
                Self::unfreeze_in_tx(tx, tenant_id, user_id, amount, Some(reason)).await
            }
        };
        let (updated_balance, balance_transaction) = match operation {
            Ok(result) => result,
            Err(error)
                if kind == ManualBalanceOperationKind::Unfreeze
                    && error.is_insufficient_balance() =>
            {
                // Preserve any expiry reclamation performed by unfreeze, but
                // remove this unfinished claim so a later request may succeed
                // after funds are manually frozen.
                tx.execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM admin_balance_operations WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND actor_user_id=$4 AND completed_at IS NULL",
                    [claim.id.into(),tenant_id.into(),user_id.into(),actor_user_id.into()],
                ))
                .await?;
                append_manual_audit(
                    tx,
                    scope,
                    &current,
                    command,
                    Some(claim.id),
                    "balance.unfreeze_denied",
                    AuditResult::Denied,
                )
                .await?;
                return Ok(ManualAttempt::RepairedFailure(error));
            }
            Err(error) => return Err(error),
        };
        if updated_balance.tenant_id != tenant_id || balance_transaction.tenant_id != tenant_id {
            return Err(DbError::Other(format!(
                "user {user_id} balance belongs to tenant {}, not {tenant_id}",
                updated_balance.tenant_id
            )));
        }

        let completed_claim =
            AdminBalanceOperation::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"UPDATE admin_balance_operations
                   SET balance_transaction_id = $1, balance_before = $2,
                       balance_after = $3, frozen_balance_after = $4,
                       completed_at = NOW()
                   WHERE id = $5 AND tenant_id=$6 AND user_id=$7 AND actor_user_id=$8 AND completed_at IS NULL
                   RETURNING *"#,
                [
                    balance_transaction.id.into(),
                    balance_transaction.balance_before.into(),
                    balance_transaction.balance_after.into(),
                    updated_balance.frozen_balance.into(),
                    claim.id.into(), tenant_id.into(),user_id.into(),actor_user_id.into(),
                ],
            ))
            .one(tx)
            .await?
            .ok_or_else(|| {
                DbError::Other(format!(
                    "administrator balance operation {} was not completed",
                    claim.id
                ))
            })?;
        let outcome = completed_claim.completed_outcome()?.ok_or_else(|| {
            DbError::Other(format!(
                "administrator balance operation {} has no completed result",
                claim.id
            ))
        })?;
        append_manual_audit(
            tx,
            scope,
            &current,
            command,
            Some(outcome.operation_id),
            &format!("balance.{}", kind.as_str()),
            AuditResult::Success,
        )
        .await?;
        Ok(ManualAttempt::Decision(
            ManualBalanceOperationDecision::Completed(outcome),
        ))
    }
}
