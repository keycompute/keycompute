//! Console reservation metadata is read-only; recovery preserves billing ownership.
use super::*;
use crate::models::financial_scope::{FinancialAccess, FinancialScope};
use crate::{AuditContext, TenantAuditEvent};
use keycompute_types::{AuditResult, AuditScopeType};

pub struct ReleaseReservationCommand<'a> {
    pub user_id: Uuid,
    pub request_id: Uuid,
    pub expected_owner_token: Uuid,
    pub reason: &'a str,
}
impl UserBalance {
    /// One primary SQL snapshot provides both the exact frozen-funds aggregate
    /// and the bounded page. Reading never reclaims money or locks wallet rows.
    pub async fn find_breakdown_page_in_scope(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
        user_id: Uuid,
        cursor: Option<BalanceReservationPageCursor>,
        limit: u64,
    ) -> Result<Option<UserBalanceBreakdownPage>, DbError> {
        scope.require_admin()?;
        scope.tenant_id()?;
        validate_balance_reservation_page_size(limit)?;
        if user_id.is_nil() {
            return Err(DbError::Other("financial_target_invalid".into()));
        }
        let mut values = scope.values();
        values.extend([
            user_id.into(),
            cursor.map(|v| v.created_at).into(),
            cursor.map(|v| v.id).into(),
            ((limit + 1) as i64).into(),
        ]);
        let sql = format!(
            r#"WITH owner AS (
            SELECT tenant_id,user_id FROM tenant_memberships WHERE tenant_id=$8 AND user_id=$10
        ), wallet AS (
            SELECT b.* FROM user_balances b JOIN owner m ON m.tenant_id=b.tenant_id AND m.user_id=b.user_id
        ), details AS (
            SELECT r.* FROM balance_reservations r
            WHERE r.tenant_id=$8 AND r.user_id=$10 AND r.status='active'
              AND ($11::timestamptz IS NULL OR (r.created_at,r.id)<($11,$12::uuid))
            ORDER BY r.created_at DESC,r.id DESC LIMIT $13
        ) SELECT EXISTS(SELECT 1 FROM owner) AS visible,
            (SELECT to_jsonb(b)||jsonb_build_object('available_balance',b.available_balance::text,
                'frozen_balance',b.frozen_balance::text,'total_recharged',b.total_recharged::text,
                'total_consumed',b.total_consumed::text) FROM wallet b) AS wallet,
            (SELECT COALESCE(SUM(amount),0) FROM balance_reservations WHERE tenant_id=$8 AND user_id=$10 AND status='active') AS reserved,
            (SELECT COALESCE(jsonb_agg(to_jsonb(r)||jsonb_build_object('amount',r.amount::text) ORDER BY r.created_at DESC,r.id DESC),'[]'::jsonb) FROM details r) AS entries
        WHERE {}"#,
            scope.predicate()
        );
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?
            .ok_or_else(|| DbError::Other("financial_authority_invalid".into()))?;
        if !row.try_get::<bool>("", "visible")? {
            return Err(DbError::not_found("Tenant member", user_id));
        }
        let wallet: Option<serde_json::Value> = row.try_get("", "wallet")?;
        let Some(wallet) = wallet else {
            return Ok(None);
        };
        let balance: UserBalance = serde_json::from_value(wallet)
            .map_err(|_| DbError::Other("financial_snapshot_invalid".into()))?;
        let reserved: Decimal = row.try_get("", "reserved")?;
        let mut reservations: Vec<BalanceReservation> =
            serde_json::from_value(row.try_get("", "entries")?)
                .map_err(|_| DbError::Other("financial_snapshot_invalid".into()))?;
        let has_more = reservations.len() > limit as usize;
        reservations.truncate(limit as usize);
        let next_cursor = if has_more {
            reservations.last().map(|r| BalanceReservationPageCursor {
                created_at: r.created_at,
                id: r.id,
            })
        } else {
            None
        };
        let breakdown = BalanceReservation::breakdown_with_active_total(balance, reserved)?;
        Ok(Some(UserBalanceBreakdownPage {
            breakdown,
            reservations,
            next_cursor,
        }))
    }
}
impl BalanceReservation {
    async fn find_recovery_target(
        tx: &DatabaseTransaction,
        tenant_id: Uuid,
        user_id: Uuid,
        request_id: Uuid,
        lock: bool,
    ) -> Result<Option<Self>, DbError> {
        Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            format!("SELECT * FROM balance_reservations WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3{}",if lock {" FOR UPDATE"}else{""}),
            [tenant_id.into(),user_id.into(),request_id.into()])).one(tx).await.map_err(DbError::from)
    }
    /// Root may explicitly force recovery; a tenant administrator may release
    /// only an already expired reservation. Accepted late settlement is unchanged.
    pub async fn admin_release(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: FinancialScope,
        audit: &AuditContext,
        command: &ReleaseReservationCommand<'_>,
    ) -> Result<Option<AdminRequestReservationRelease>, DbError> {
        scope.require_admin()?;
        if command.user_id.is_nil()
            || command.request_id.is_nil()
            || command.expected_owner_token.is_nil()
        {
            return Err(DbError::Other("financial_target_invalid".into()));
        }
        let reason = command.reason.trim();
        if reason.is_empty()
            || reason.chars().count() > MAX_BALANCE_RESERVATION_RELEASE_REASON_CHARS
            || reason.chars().any(char::is_control)
        {
            return Err(DbError::Other("financial_reason_invalid".into()));
        }
        let tx = db.begin().await?;
        let result = async {
            tx.execute_unprepared("SET LOCAL lock_timeout='3s'; SET LOCAL statement_timeout='10s'")
                .await?;
            let result = Self::release_recovery_in_tx(&tx, scope, audit, command).await?;
            scope.current_actor(&tx).await?;
            Ok::<_, DbError>(result)
        }
        .await;
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
    async fn release_recovery_in_tx(
        tx: &DatabaseTransaction,
        scope: FinancialScope,
        audit: &AuditContext,
        command: &ReleaseReservationCommand<'_>,
    ) -> Result<Option<AdminRequestReservationRelease>, DbError> {
        let tenant_id = scope.tenant_id()?;
        let user_id = command.user_id;
        let request_id = command.request_id;
        let reason = command.reason.trim();
        let released_by = scope.user_id();
        let current = scope.lock(tx, audit).await?;
        if Self::find_recovery_target(tx, tenant_id, user_id, request_id, false)
            .await?
            .is_none()
        {
            return Ok(None);
        }
        // Wallet first, reservation second: compatible with normal settlement.
        let balance = UserBalance::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE tenant_id=$1 AND user_id=$2 FOR UPDATE",
            [tenant_id.into(), user_id.into()],
        ))
        .one(tx)
        .await?
        .ok_or_else(|| DbError::not_found("UserBalance", user_id))?;
        let reservation = Self::find_recovery_target(tx, tenant_id, user_id, request_id, true)
            .await?
            .ok_or_else(|| DbError::not_found("Balance reservation", request_id))?;
        scope.current_actor(tx).await?;
        if reservation.owner_token != command.expected_owner_token {
            return Ok(None);
        }
        if reservation.status != "active" {
            if Self::is_administrative_release_tombstone(
                &reservation.status,
                reservation.release_kind.as_deref(),
            ) && reservation.released_by == Some(released_by)
                && reservation.release_reason.as_deref() == Some(reason)
            {
                let breakdown = Self::breakdown_for_locked_balance(tx, balance).await?;
                return Ok(Some(AdminRequestReservationRelease {
                    breakdown,
                    released_reservation: reservation,
                }));
            }
            return Ok(None);
        }
        if scope.access() == FinancialAccess::TenantAdmin && reservation.expires_at > Utc::now() {
            return Ok(None);
        }
        if balance.frozen_balance < reservation.amount {
            return Err(DbError::Other("financial_reservation_inconsistent".into()));
        }
        let updated=UserBalance::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE user_balances SET available_balance=available_balance+$1,frozen_balance=frozen_balance-$1,updated_at=clock_timestamp() WHERE tenant_id=$2 AND user_id=$3 RETURNING *",
            [reservation.amount.into(),tenant_id.into(),user_id.into()])).one(tx).await?.ok_or_else(||DbError::not_found("UserBalance",user_id))?;
        let released=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE balance_reservations SET status='released',released_at=clock_timestamp(),release_kind='administrative',release_reason=$1,released_by=$2,updated_at=clock_timestamp() WHERE tenant_id=$3 AND user_id=$4 AND request_id=$5 AND owner_token=$6 AND status='active' RETURNING *",
            [reason.into(),released_by.into(),tenant_id.into(),user_id.into(),request_id.into(),command.expected_owner_token.into()]))
            .one(tx).await?.ok_or_else(||DbError::Other("financial_reservation_changed".into()))?;
        TenantAuditEvent::append(tx,AuditScopeType::Tenant,Some(tenant_id),&current,"balance.reservation_release","balance_reservation",
            Some(&released.id.to_string()),AuditResult::Success,
            serde_json::json!({"owner_user_id":user_id,"request_id":request_id,"amount":released.amount.to_string(),"currency":"CNY","reason":reason})).await?;
        let breakdown = Self::breakdown_for_locked_balance(tx, updated).await?;
        Ok(Some(AdminRequestReservationRelease {
            breakdown,
            released_reservation: released,
        }))
    }
}
