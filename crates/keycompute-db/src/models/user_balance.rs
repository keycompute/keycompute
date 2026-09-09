//! 用户余额模型

use crate::DbError;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// 交易类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransactionType {
    /// 充值
    Recharge,
    /// 消费
    Consume,
    /// 冻结
    Freeze,
    /// 解冻
    Unfreeze,
    /// 小费入账（tips 转为可用余额）
    TipCredit,
}

impl TransactionType {
    pub fn as_str(&self) -> &'static str {
        match self {
            TransactionType::Recharge => "recharge",
            TransactionType::Consume => "consume",
            TransactionType::Freeze => "freeze",
            TransactionType::Unfreeze => "unfreeze",
            TransactionType::TipCredit => "tip_credit",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "recharge" => Some(TransactionType::Recharge),
            "consume" => Some(TransactionType::Consume),
            "freeze" => Some(TransactionType::Freeze),
            "unfreeze" => Some(TransactionType::Unfreeze),
            "tip_credit" => Some(TransactionType::TipCredit),
            _ => None,
        }
    }
}

/// 用户余额模型
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct UserBalance {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    /// 可用余额
    pub available_balance: Decimal,
    /// 冻结余额
    pub frozen_balance: Decimal,
    /// 累计充值金额
    pub total_recharged: Decimal,
    /// 累计消费金额
    pub total_consumed: Decimal,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Durable pre-dispatch balance reservation keyed by the logical billing
/// request. Active rows own the matching amount in `frozen_balance`.
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct BalanceReservation {
    pub id: Uuid,
    pub request_id: Uuid,
    pub owner_token: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub amount: Decimal,
    pub status: String,
    pub usage_log_id: Option<Uuid>,
    pub expires_at: DateTime<Utc>,
    pub settled_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    pub release_kind: Option<String>,
    pub release_reason: Option<String>,
    pub released_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Immutable snapshot of one reservation ownership or state transition.
/// `event_sequence` is the authoritative ordering key; timestamps may be
/// equal for multiple events emitted by the same database transaction.
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct BalanceReservationEvent {
    pub id: Uuid,
    pub event_sequence: i64,
    pub reservation_id: Uuid,
    pub request_id: Uuid,
    pub owner_token: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub event_type: String,
    pub amount: Decimal,
    pub status: String,
    pub usage_log_id: Option<Uuid>,
    pub expires_at: DateTime<Utc>,
    pub settled_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    pub release_kind: Option<String>,
    pub release_reason: Option<String>,
    pub released_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl BalanceReservationEvent {
    /// Return the complete immutable history for one logical billing request.
    pub async fn find_by_request(
        db: &impl ConnectionTrait,
        request_id: Uuid,
    ) -> Result<Vec<Self>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM balance_reservation_events WHERE request_id = $1 ORDER BY event_sequence",
            [request_id.into()],
        );
        Ok(Self::find_by_statement(stmt).all(db).await?)
    }
}

/// Maximum administrator-provided reservation release reason length. This is
/// measured in Unicode scalar values, matching PostgreSQL `CHAR_LENGTH`.
pub const MAX_BALANCE_RESERVATION_RELEASE_REASON_CHARS: usize = 1000;
/// Maximum normalized audit reason length for administrator balance changes.
pub const MAX_ADMIN_BALANCE_OPERATION_REASON_CHARS: usize = 1000;
/// Maximum visible-ASCII byte length accepted for administrator balance
/// operation idempotency keys.
pub const MAX_ADMIN_BALANCE_IDEMPOTENCY_KEY_BYTES: usize = 256;
/// Hard upper bound for active reservation detail pages. Keeping this limit
/// in the data layer prevents future callers from accidentally reintroducing
/// an unbounded query while the owning balance row is locked.
pub const MAX_BALANCE_RESERVATION_PAGE_SIZE: u64 = 100;

/// An atomic view of a balance and the active request reservations that own
/// part of its persisted frozen total.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserBalanceBreakdown {
    pub balance: UserBalance,
    pub active_reserved: Decimal,
    pub manually_frozen: Decimal,
}

/// Stable keyset cursor for active reservation detail pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceReservationPageCursor {
    pub created_at: DateTime<Utc>,
    pub id: Uuid,
}

/// An exact balance breakdown plus one bounded page of active reservations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserBalanceBreakdownPage {
    pub breakdown: UserBalanceBreakdown,
    pub reservations: Vec<BalanceReservation>,
    pub next_cursor: Option<BalanceReservationPageCursor>,
}

/// Result of an administrative reservation release, including the atomic
/// post-release balance view assembled before the transaction commits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminRequestReservationRelease {
    pub breakdown: UserBalanceBreakdown,
    pub released_reservation: BalanceReservation,
}

/// Administrator-initiated balance movement protected by a durable
/// `Idempotency-Key` claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ManualBalanceOperationKind {
    Recharge,
    Consume,
    Freeze,
    Unfreeze,
}

impl ManualBalanceOperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recharge => "recharge",
            Self::Consume => "consume",
            Self::Freeze => "freeze",
            Self::Unfreeze => "unfreeze",
        }
    }
}

/// Immutable result snapshot returned for both the first successful manual
/// operation and every matching retry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualBalanceOperationOutcome {
    pub operation_id: Uuid,
    pub operation_type: String,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub actor_user_id: Uuid,
    pub amount: Decimal,
    pub reason: String,
    pub balance_transaction_id: Uuid,
    pub balance_before: Decimal,
    pub balance_after: Decimal,
    pub frozen_balance_after: Decimal,
}

/// A matching retry returns the persisted [`ManualBalanceOperationOutcome`].
/// Reusing the same key for any different normalized request returns
/// [`Conflict`](Self::Conflict) without touching the balance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualBalanceOperationDecision {
    Completed(ManualBalanceOperationOutcome),
    Conflict,
}

#[derive(Debug, FromQueryResult)]
struct AdminBalanceOperation {
    id: Uuid,
    idempotency_key_hash: String,
    request_fingerprint: String,
    operation_type: String,
    tenant_id: Uuid,
    user_id: Uuid,
    actor_user_id: Uuid,
    amount: Decimal,
    reason: String,
    balance_transaction_id: Option<Uuid>,
    balance_before: Option<Decimal>,
    balance_after: Option<Decimal>,
    frozen_balance_after: Option<Decimal>,
    completed_at: Option<DateTime<Utc>>,
}

impl AdminBalanceOperation {
    // Keep every immutable claim field explicit at the comparison site. A
    // catch-all request object would make it easier to omit a field from the
    // idempotency contract when this financial operation evolves.
    #[allow(clippy::too_many_arguments)]
    fn matches_request(
        &self,
        fingerprint: &str,
        kind: ManualBalanceOperationKind,
        tenant_id: Uuid,
        user_id: Uuid,
        actor_user_id: Uuid,
        amount: Decimal,
        reason: &str,
    ) -> bool {
        self.request_fingerprint == fingerprint
            && self.operation_type == kind.as_str()
            && self.tenant_id == tenant_id
            && self.user_id == user_id
            && self.actor_user_id == actor_user_id
            && self.amount == amount
            && self.reason == reason
    }

    fn completed_outcome(&self) -> Result<Option<ManualBalanceOperationOutcome>, DbError> {
        if self.completed_at.is_none() {
            if self.balance_transaction_id.is_some()
                || self.balance_before.is_some()
                || self.balance_after.is_some()
                || self.frozen_balance_after.is_some()
            {
                return Err(DbError::Other(format!(
                    "incomplete administrator balance operation {} has result fields",
                    self.id
                )));
            }
            return Ok(None);
        }
        let balance_transaction_id = self.balance_transaction_id.ok_or_else(|| {
            DbError::Other(format!(
                "completed administrator balance operation {} has no transaction",
                self.id
            ))
        })?;
        let balance_before = self.balance_before.ok_or_else(|| {
            DbError::Other(format!(
                "completed administrator balance operation {} has no balance_before",
                self.id
            ))
        })?;
        let balance_after = self.balance_after.ok_or_else(|| {
            DbError::Other(format!(
                "completed administrator balance operation {} has no balance_after",
                self.id
            ))
        })?;
        let frozen_balance_after = self.frozen_balance_after.ok_or_else(|| {
            DbError::Other(format!(
                "completed administrator balance operation {} has no frozen balance",
                self.id
            ))
        })?;
        Ok(Some(ManualBalanceOperationOutcome {
            operation_id: self.id,
            operation_type: self.operation_type.clone(),
            tenant_id: self.tenant_id,
            user_id: self.user_id,
            actor_user_id: self.actor_user_id,
            amount: self.amount,
            reason: self.reason.clone(),
            balance_transaction_id,
            balance_before,
            balance_after,
            frozen_balance_after,
        }))
    }
}

fn sha256_hex(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn manual_balance_request_fingerprint(
    kind: ManualBalanceOperationKind,
    tenant_id: Uuid,
    user_id: Uuid,
    actor_user_id: Uuid,
    amount: Decimal,
    reason: &str,
) -> String {
    fn hash_field(hasher: &mut Sha256, field: &[u8]) {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }

    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"keycompute-admin-balance-operation-v1");
    hash_field(&mut hasher, kind.as_str().as_bytes());
    hash_field(&mut hasher, tenant_id.as_bytes());
    hash_field(&mut hasher, user_id.as_bytes());
    hash_field(&mut hasher, actor_user_id.as_bytes());
    hash_field(&mut hasher, amount.normalize().to_string().as_bytes());
    hash_field(&mut hasher, reason.as_bytes());
    hex::encode(hasher.finalize())
}

#[derive(Debug, FromQueryResult)]
struct ActiveReservationTotal {
    amount: Decimal,
}

#[derive(Debug, FromQueryResult)]
struct ReservationReclaimCandidate {
    user_id: Uuid,
    active_reserved: Decimal,
    expired_amount: Decimal,
    expired_count: i64,
}

#[derive(Debug, FromQueryResult)]
struct ReservationReclaimSummary {
    user_id: Uuid,
    expired_amount: Decimal,
    expired_count: i64,
}

struct ReclaimedBalanceState {
    balances: Vec<UserBalance>,
    active_totals: std::collections::HashMap<Uuid, Decimal>,
    reclaimed: u64,
}

fn validate_balance_reservation_page_size(limit: u64) -> Result<(), DbError> {
    if (1..=MAX_BALANCE_RESERVATION_PAGE_SIZE).contains(&limit) {
        Ok(())
    } else {
        Err(DbError::Other(format!(
            "balance reservation page size must be between 1 and {MAX_BALANCE_RESERVATION_PAGE_SIZE}"
        )))
    }
}

impl BalanceReservation {
    fn is_administrative_release_tombstone(status: &str, release_kind: Option<&str>) -> bool {
        status == "released" && release_kind == Some("administrative")
    }

    fn settlement_available_balances(
        available_balance: Decimal,
        reserved_amount: Decimal,
        consumed_amount: Decimal,
    ) -> (Decimal, Decimal) {
        let balance_before = available_balance + reserved_amount;
        (balance_before, balance_before - consumed_amount)
    }

    fn amount_to_reserve(
        available: Decimal,
        requested: Decimal,
        minimum_available: Decimal,
    ) -> Result<Decimal, DbError> {
        if minimum_available < Decimal::ZERO {
            return Err(DbError::Other(
                "minimum available balance must not be negative".to_string(),
            ));
        }
        if requested < Decimal::ZERO {
            return Err(DbError::Other(
                "balance reservation amount must not be negative".to_string(),
            ));
        }
        if available < minimum_available {
            return Err(DbError::insufficient_balance(
                minimum_available.to_string(),
                available.to_string(),
            ));
        }

        // Preserve the existing minimum-balance admission rule inside the
        // same transaction as the reservation. In particular, a request that
        // follows an explicit reservation of the remaining funds sees zero
        // here and is rejected instead of creating a zero-valued reservation.
        let amount = requested.max(minimum_available);
        if available < amount {
            return Err(DbError::insufficient_balance(
                amount.to_string(),
                available.to_string(),
            ));
        }
        Ok(amount)
    }

    async fn find_by_request(
        db: &impl ConnectionTrait,
        request_id: Uuid,
        for_update: bool,
    ) -> Result<Option<Self>, DbError> {
        let suffix = if for_update { " FOR UPDATE" } else { "" };
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT * FROM balance_reservations WHERE request_id = $1{suffix}"),
            [request_id.into()],
        );
        Ok(Self::find_by_statement(stmt).one(db).await?)
    }

    async fn active_total_by_user(
        db: &impl ConnectionTrait,
        user_id: Uuid,
    ) -> Result<Decimal, DbError> {
        let reserved_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COALESCE(SUM(amount), 0) AS amount FROM balance_reservations WHERE user_id = $1 AND status = 'active'",
            [user_id.into()],
        );
        Ok(ActiveReservationTotal::find_by_statement(reserved_stmt)
            .one(db)
            .await?
            .map(|total| total.amount)
            .unwrap_or(Decimal::ZERO))
    }

    async fn active_page_by_user(
        db: &impl ConnectionTrait,
        user_id: Uuid,
        cursor: Option<BalanceReservationPageCursor>,
        limit: u64,
    ) -> Result<(Vec<Self>, Option<BalanceReservationPageCursor>), DbError> {
        let mut values = vec![user_id.into()];
        let cursor_clause = if let Some(cursor) = cursor {
            values.push(cursor.created_at.into());
            values.push(cursor.id.into());
            " AND (created_at, id) < ($2, $3)"
        } else {
            ""
        };
        values.push(((limit + 1) as i64).into());
        let limit_parameter = values.len();
        let active_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT * FROM balance_reservations WHERE user_id = $1 AND status = 'active'{cursor_clause} ORDER BY created_at DESC, id DESC LIMIT ${limit_parameter}"
            ),
            values,
        );
        let mut reservations = Self::find_by_statement(active_stmt).all(db).await?;
        let has_more = reservations.len() > limit as usize;
        if has_more {
            reservations.truncate(limit as usize);
        }
        let next_cursor = has_more.then(|| {
            let last = reservations
                .last()
                .expect("a page with a lookahead row must contain a visible row");
            BalanceReservationPageCursor {
                created_at: last.created_at,
                id: last.id,
            }
        });
        Ok((reservations, next_cursor))
    }

    fn breakdown_with_active_total(
        balance: UserBalance,
        active_reserved: Decimal,
    ) -> Result<UserBalanceBreakdown, DbError> {
        if balance.frozen_balance < active_reserved {
            return Err(DbError::Other(format!(
                "active balance reservations for user {} exceed frozen balance",
                balance.user_id
            )));
        }
        Ok(UserBalanceBreakdown {
            manually_frozen: balance.frozen_balance - active_reserved,
            balance,
            active_reserved,
        })
    }

    async fn breakdown_for_locked_balance(
        tx: &DatabaseTransaction,
        balance: UserBalance,
    ) -> Result<UserBalanceBreakdown, DbError> {
        let active_reserved = Self::active_total_by_user(tx, balance.user_id).await?;
        Self::breakdown_with_active_total(balance, active_reserved)
    }

    /// Release expired reservations after their owning balance rows have been
    /// locked. Callers that lock more than one balance must do so in user ID
    /// order before entering this helper.
    async fn reclaim_expired_for_locked_balances(
        tx: &DatabaseTransaction,
        balances: Vec<UserBalance>,
    ) -> Result<ReclaimedBalanceState, DbError> {
        Self::reclaim_expired_for_locked_balances_inner(tx, balances, false).await
    }

    /// Background sweeping must not let one permanently inconsistent user
    /// roll back reclamation for every healthy user in the same locked batch.
    /// Only the known cross-table invariant violation is isolated here; all
    /// database and row-count errors still fail the complete transaction.
    async fn reclaim_expired_for_locked_balances_isolating_invalid_users(
        tx: &DatabaseTransaction,
        balances: Vec<UserBalance>,
    ) -> Result<ReclaimedBalanceState, DbError> {
        Self::reclaim_expired_for_locked_balances_inner(tx, balances, true).await
    }

    async fn reclaim_expired_for_locked_balances_inner(
        tx: &DatabaseTransaction,
        mut balances: Vec<UserBalance>,
        isolate_invalid_users: bool,
    ) -> Result<ReclaimedBalanceState, DbError> {
        if balances.is_empty() {
            return Ok(ReclaimedBalanceState {
                balances,
                active_totals: std::collections::HashMap::new(),
                reclaimed: 0,
            });
        }

        let user_ids = balances
            .iter()
            .map(|balance| balance.user_id)
            .collect::<Vec<_>>();
        // Balance rows are already locked before this query. Every supported
        // writer takes that lock before changing a reservation, and NOW() is
        // transaction-stable in PostgreSQL. Aggregate both the full active
        // ownership and the expired subset without materializing reservation
        // rows in the application.
        let candidates_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT user_id,
                      COALESCE(SUM(amount), 0) AS active_reserved,
                      COALESCE(SUM(amount) FILTER (WHERE expires_at <= NOW()), 0) AS expired_amount,
                      (COUNT(*) FILTER (WHERE expires_at <= NOW()))::BIGINT AS expired_count
               FROM balance_reservations
               WHERE user_id = ANY($1) AND status = 'active'
               GROUP BY user_id"#,
            [user_ids.clone().into()],
        );
        let candidates = ReservationReclaimCandidate::find_by_statement(candidates_stmt)
            .all(tx)
            .await?;
        let mut active_totals = balances
            .iter()
            .map(|balance| (balance.user_id, Decimal::ZERO))
            .collect::<std::collections::HashMap<_, _>>();
        let mut expired_by_user = std::collections::HashMap::new();
        for candidate in candidates {
            let expired_count = u64::try_from(candidate.expired_count).map_err(|_| {
                DbError::Other(format!(
                    "invalid expired reservation count for user {}",
                    candidate.user_id
                ))
            })?;
            if candidate.active_reserved < Decimal::ZERO
                || candidate.expired_amount < Decimal::ZERO
                || candidate.active_reserved < candidate.expired_amount
            {
                return Err(DbError::Other(format!(
                    "invalid balance reservation reclaim aggregate for user {}",
                    candidate.user_id
                )));
            }
            active_totals.insert(candidate.user_id, candidate.active_reserved);
            if expired_count > 0 {
                expired_by_user
                    .insert(candidate.user_id, (candidate.expired_amount, expired_count));
            }
        }
        let mut invalid_user_ids = std::collections::HashSet::new();
        for balance in &balances {
            let active_reserved = active_totals
                .get(&balance.user_id)
                .copied()
                .unwrap_or(Decimal::ZERO);
            if balance.frozen_balance < active_reserved {
                if isolate_invalid_users {
                    tracing::error!(
                        user_id = %balance.user_id,
                        frozen_balance = %balance.frozen_balance,
                        active_reserved = %active_reserved,
                        "余额预留聚合不变量异常，跳过该用户的过期预留回收"
                    );
                    invalid_user_ids.insert(balance.user_id);
                    continue;
                }
                return Err(DbError::Other(format!(
                    "active balance reservations for user {} exceed frozen balance",
                    balance.user_id
                )));
            }
        }

        if expired_by_user.is_empty() {
            return Ok(ReclaimedBalanceState {
                balances,
                active_totals,
                reclaimed: 0,
            });
        }

        let valid_user_ids = user_ids
            .into_iter()
            .filter(|user_id| !invalid_user_ids.contains(user_id))
            .collect::<Vec<_>>();
        if valid_user_ids.is_empty() {
            return Ok(ReclaimedBalanceState {
                balances,
                active_totals,
                reclaimed: 0,
            });
        }

        // The data-modifying CTE fires the existing per-row audit trigger but
        // sends only one amount/count summary per user back to the process.
        // NOW() has the same value as in the candidate aggregate above, so
        // both statements address exactly the same expiry horizon.
        let reclaim_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"WITH expired AS (
                   UPDATE balance_reservations
                   SET status = 'expired', updated_at = NOW()
                   WHERE user_id = ANY($1)
                     AND status = 'active'
                     AND expires_at <= NOW()
                   RETURNING user_id, amount
               )
               SELECT user_id,
                      COALESCE(SUM(amount), 0) AS expired_amount,
                      COUNT(*)::BIGINT AS expired_count
               FROM expired
               GROUP BY user_id"#,
            [valid_user_ids.into()],
        );
        let reclaimed_by_user = ReservationReclaimSummary::find_by_statement(reclaim_stmt)
            .all(tx)
            .await?
            .into_iter()
            .map(|summary| (summary.user_id, summary))
            .collect::<std::collections::HashMap<_, _>>();

        let expected_valid_count = expired_by_user
            .iter()
            .filter(|(user_id, _)| !invalid_user_ids.contains(user_id))
            .count();
        if reclaimed_by_user.len() != expected_valid_count {
            return Err(DbError::Other(format!(
                "expired reservation reclamation returned {} user summaries, expected {}",
                reclaimed_by_user.len(),
                expected_valid_count
            )));
        }

        let mut reclaimed = 0_u64;
        for (user_id, (expected_amount, expected_count)) in expired_by_user
            .iter()
            .filter(|(user_id, _)| !invalid_user_ids.contains(user_id))
        {
            let actual = reclaimed_by_user.get(user_id).ok_or_else(|| {
                DbError::Other(format!(
                    "expired reservation reclamation omitted user {user_id}"
                ))
            })?;
            let actual_count = u64::try_from(actual.expired_count).map_err(|_| {
                DbError::Other(format!(
                    "expired reservation count overflow for user {user_id}"
                ))
            })?;
            if actual.expired_amount != *expected_amount || actual_count != *expected_count {
                return Err(DbError::Other(format!(
                    "expired reservation reclamation for user {user_id} changed {actual_count} rows totaling {}, expected {expected_count} rows totaling {expected_amount}",
                    actual.expired_amount
                )));
            }
            reclaimed = reclaimed
                .checked_add(actual_count)
                .ok_or_else(|| DbError::Other("expired reservation count overflow".to_string()))?;
            let active_reserved = active_totals.get_mut(user_id).ok_or_else(|| {
                DbError::Other(format!(
                    "active reservation aggregate omitted user {user_id}"
                ))
            })?;
            *active_reserved -= actual.expired_amount;
        }

        for balance in &mut balances {
            let Some(summary) = reclaimed_by_user.get(&balance.user_id) else {
                continue;
            };
            if summary.expired_amount == Decimal::ZERO {
                continue;
            }
            let update_balance = Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE user_balances SET available_balance = available_balance + $1, frozen_balance = frozen_balance - $1, updated_at = NOW() WHERE user_id = $2 RETURNING *",
                [summary.expired_amount.into(), balance.user_id.into()],
            );
            *balance = UserBalance::find_by_statement(update_balance)
                .one(tx)
                .await?
                .ok_or_else(|| DbError::not_found("UserBalance", balance.user_id.to_string()))?;
        }

        Ok(ReclaimedBalanceState {
            balances,
            active_totals,
            reclaimed,
        })
    }

    /// Reclaim expired reservations independently of balance reads or new
    /// request admission. `batch_size` limits the number of user balance rows
    /// locked in one transaction; every expired reservation owned by those
    /// users is reclaimed together to preserve the aggregate invariant.
    pub async fn reclaim_expired(
        db: &(impl ConnectionTrait + TransactionTrait),
        batch_size: u64,
    ) -> Result<u64, DbError> {
        if batch_size == 0 {
            return Ok(0);
        }
        let batch_size = i64::try_from(batch_size).map_err(|_| {
            DbError::Other("reservation reclaim batch size is too large".to_string())
        })?;
        let tx = db.begin().await?;
        let lock_balances = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT ub.*
               FROM user_balances ub
               WHERE EXISTS (
                   SELECT 1
                   FROM balance_reservations br
                   WHERE br.user_id = ub.user_id
                     AND br.status = 'active'
                     AND br.expires_at <= NOW()
               )
                 AND COALESCE((
                     SELECT SUM(active_br.amount)
                     FROM balance_reservations active_br
                     WHERE active_br.user_id = ub.user_id
                       AND active_br.status = 'active'
                 ), 0) <= ub.frozen_balance
               ORDER BY ub.user_id
               LIMIT $1
               FOR UPDATE OF ub SKIP LOCKED"#,
            [batch_size.into()],
        );
        let balances = UserBalance::find_by_statement(lock_balances)
            .all(&tx)
            .await?;
        let reclaimed =
            Self::reclaim_expired_for_locked_balances_isolating_invalid_users(&tx, balances)
                .await?
                .reclaimed;
        tx.commit().await?;
        Ok(reclaimed)
    }

    /// Atomically move an estimated request cost from available to frozen
    /// balance. Every caller must provide an explicit bounded monetary
    /// estimate; there is no sentinel value for an implicit reservation size.
    // The principal, logical request, ownership generation, amount, admission
    // floor, and expiry are deliberately explicit parts of this DB contract.
    #[allow(clippy::too_many_arguments)]
    pub async fn reserve(
        db: &(impl ConnectionTrait + TransactionTrait),
        tenant_id: Uuid,
        user_id: Uuid,
        request_id: Uuid,
        owner_token: Uuid,
        amount: Decimal,
        minimum_available: Decimal,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, DbError> {
        // Reject invalid caller input before opening a transaction. Capacity
        // is evaluated again by `amount_to_reserve` after stale reservations
        // have been reclaimed under the balance-row lock.
        Self::amount_to_reserve(Decimal::MAX, amount, minimum_available)?;
        if expires_at <= Utc::now() {
            return Err(DbError::Other(
                "balance reservation expiry must be in the future".to_string(),
            ));
        }
        let tx = db.begin().await?;
        let lock_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [user_id.into()],
        );
        let mut balance = UserBalance::find_by_statement(lock_stmt)
            .one(&tx)
            .await?
            .ok_or_else(|| {
                DbError::insufficient_balance(minimum_available.to_string(), "0".to_string())
            })?;
        if balance.tenant_id != tenant_id {
            return Err(DbError::Other(
                "balance reservation tenant mismatch".to_string(),
            ));
        }

        // Release crash-orphaned reservations before evaluating capacity or
        // deciding whether the target request still owns frozen funds. Doing
        // this first also avoids double-counting a reservation that expires at
        // the boundary between the application clock and PostgreSQL's clock.
        let mut reclaimed = Self::reclaim_expired_for_locked_balances(&tx, vec![balance]).await?;
        balance = reclaimed
            .balances
            .pop()
            .expect("the locked balance must be preserved during reclamation");

        let mut reusable_request = None;
        let mut replaced_active_amount = Decimal::ZERO;
        if let Some(existing) = Self::find_by_request(&tx, request_id, true).await? {
            if existing.tenant_id != tenant_id || existing.user_id != user_id {
                return Err(DbError::Other(format!(
                    "balance reservation {request_id} belongs to another principal"
                )));
            }
            if existing.status == "active" {
                // A crash recovery or idempotent retry can recompute the
                // maximum charge with a newer pricing snapshot. Treat the
                // currently frozen amount as capacity owned by this logical
                // request, then resize it atomically instead of silently
                // retaining a stale amount.
                replaced_active_amount = existing.amount;
            }
            if existing.status == "settled" {
                return Err(DbError::Other(format!(
                    "balance reservation {request_id} is already settled"
                )));
            }
            if Self::is_administrative_release_tombstone(
                &existing.status,
                existing.release_kind.as_deref(),
            ) {
                return Err(DbError::Other(format!(
                    "balance reservation {request_id} was administratively released and cannot be reactivated"
                )));
            }
            reusable_request = Some(existing);
        }

        let reservable_balance = balance.available_balance + replaced_active_amount;
        let amount = Self::amount_to_reserve(reservable_balance, amount, minimum_available)?;
        let balance_delta = amount - replaced_active_amount;
        if balance_delta != Decimal::ZERO {
            let update_stmt = Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE user_balances SET available_balance = available_balance - $1, frozen_balance = frozen_balance + $1, updated_at = NOW() WHERE user_id = $2",
                [balance_delta.into(), user_id.into()],
            );
            tx.execute(update_stmt).await?;
        }
        let reserve_stmt = if reusable_request.is_some() {
            Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE balance_reservations SET owner_token = $1, amount = $2, status = 'active', usage_log_id = NULL, expires_at = $3, settled_at = NULL, released_at = NULL, release_kind = NULL, release_reason = NULL, released_by = NULL, updated_at = NOW() WHERE request_id = $4 RETURNING *",
                [
                    owner_token.into(),
                    amount.into(),
                    expires_at.into(),
                    request_id.into(),
                ],
            )
        } else {
            Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO balance_reservations (request_id, owner_token, tenant_id, user_id, amount, expires_at) VALUES ($1, $2, $3, $4, $5, $6) RETURNING *",
                [
                    request_id.into(),
                    owner_token.into(),
                    tenant_id.into(),
                    user_id.into(),
                    amount.into(),
                    expires_at.into(),
                ],
            )
        };
        let reservation = Self::find_by_statement(reserve_stmt)
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::Other("create balance reservation failed".to_string()))?;
        tx.commit().await?;
        Ok(reservation)
    }

    /// Settle an active reservation against the immutable usage ledger. The
    /// update releases unused funds and consumes the actual amount atomically.
    /// `Ok(None)` means no active reservation exists and the caller should use
    /// the legacy idempotent consumption path.
    pub async fn settle(
        db: &(impl ConnectionTrait + TransactionTrait),
        request_id: Uuid,
        expected_owner_token: Option<Uuid>,
        amount: Decimal,
        usage_log_id: Uuid,
        description: Option<&str>,
    ) -> Result<Option<(UserBalance, BalanceTransaction)>, DbError> {
        if amount < Decimal::ZERO {
            return Err(DbError::Other(
                "balance settlement amount must not be negative".to_string(),
            ));
        }
        // Use a locking-shaped lookup to force DbRouter onto the writer. The
        // lock itself is statement-scoped here; the transaction below takes
        // the authoritative balance/reservation locks in their common order.
        let Some(snapshot) = Self::find_by_request(db, request_id, true).await? else {
            return Ok(None);
        };
        let tx = db.begin().await?;
        let lock_balance = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [snapshot.user_id.into()],
        );
        let balance = UserBalance::find_by_statement(lock_balance)
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::not_found("UserBalance", snapshot.user_id.to_string()))?;
        let Some(reservation) = Self::find_by_request(&tx, request_id, true).await? else {
            return Err(DbError::Other(format!(
                "balance reservation {request_id} disappeared during settlement"
            )));
        };

        if reservation.status == "settled" {
            if reservation.usage_log_id != Some(usage_log_id) {
                return Err(DbError::Other(format!(
                    "balance reservation {request_id} is already bound to another usage log"
                )));
            }
            let transaction = BalanceTransaction::find_consumption_by_usage_log(&tx, usage_log_id)
                .await?
                .ok_or_else(|| {
                    DbError::Other(format!(
                        "settled balance reservation {request_id} has no consumption transaction"
                    ))
                })?;
            if transaction.user_id != reservation.user_id || transaction.amount != -amount {
                return Err(DbError::Other(format!(
                    "usage log {usage_log_id} is already bound to a different balance consumption"
                )));
            }
            tx.commit().await?;
            return Ok(Some((balance, transaction)));
        }
        if reservation.status != "active" {
            tx.commit().await?;
            return Ok(None);
        }
        if Some(reservation.owner_token) != expected_owner_token {
            // A prior attempt must never settle funds now owned by a newer
            // handler. Returning None routes its immutable ledger charge
            // through ordinary available/debt consumption instead.
            tx.commit().await?;
            return Ok(None);
        }
        if balance.frozen_balance < reservation.amount {
            return Err(DbError::Other(format!(
                "balance reservation {request_id} exceeds frozen balance"
            )));
        }

        // A stale owner can lose the reservation race and immediately fall
        // back to ordinary usage-ledger consumption. If the current owner
        // then reaches settlement for the same usage log, reconcile that
        // already-recorded debit by releasing the full reservation only.
        // Creating another transaction (or incrementing total_consumed again)
        // would both double-charge the user and violate the usage-log unique
        // index after the balance mutation.
        if let Some(transaction) =
            BalanceTransaction::find_consumption_by_usage_log(&tx, usage_log_id).await?
        {
            if transaction.user_id != reservation.user_id || transaction.amount != -amount {
                return Err(DbError::Other(format!(
                    "usage log {usage_log_id} is already bound to a different balance consumption"
                )));
            }
            let release_balance = Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE user_balances SET available_balance = available_balance + $1, frozen_balance = frozen_balance - $1, updated_at = NOW() WHERE user_id = $2 RETURNING *",
                [reservation.amount.into(), reservation.user_id.into()],
            );
            let updated_balance = UserBalance::find_by_statement(release_balance)
                .one(&tx)
                .await?
                .ok_or_else(|| {
                    DbError::not_found("UserBalance", reservation.user_id.to_string())
                })?;
            let settle_reservation = Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE balance_reservations SET status = 'settled', usage_log_id = $1, settled_at = NOW(), updated_at = NOW() WHERE request_id = $2",
                [usage_log_id.into(), request_id.into()],
            );
            tx.execute(settle_reservation).await?;
            tx.commit().await?;
            return Ok(Some((updated_balance, transaction)));
        }

        // The consumption ledger describes the logical post-unfreeze debit.
        // The reservation move itself is tracked by balance_reservations, so
        // include the released amount in `balance_before`; this preserves the
        // invariant `balance_after - balance_before == transaction.amount`.
        let (balance_before, balance_after) = Self::settlement_available_balances(
            balance.available_balance,
            reservation.amount,
            amount,
        );
        let update_balance = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE user_balances SET available_balance = available_balance + $1 - $2, frozen_balance = frozen_balance - $1, total_consumed = total_consumed + $2, updated_at = NOW() WHERE user_id = $3 RETURNING *",
            [
                reservation.amount.into(),
                amount.into(),
                reservation.user_id.into(),
            ],
        );
        let updated_balance = UserBalance::find_by_statement(update_balance)
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::not_found("UserBalance", reservation.user_id.to_string()))?;
        let transaction = BalanceTransaction::create_internal(
            &tx,
            reservation.tenant_id,
            reservation.user_id,
            None,
            Some(usage_log_id),
            TransactionType::Consume,
            -amount,
            balance_before,
            balance_after,
            description,
        )
        .await?;
        let settle_reservation = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE balance_reservations SET status = 'settled', usage_log_id = $1, settled_at = NOW(), updated_at = NOW() WHERE request_id = $2",
            [usage_log_id.into(), request_id.into()],
        );
        tx.execute(settle_reservation).await?;
        tx.commit().await?;
        Ok(Some((updated_balance, transaction)))
    }

    /// Release a request that failed before a usage settlement worker took
    /// ownership. Repeated releases are harmless.
    pub async fn release(
        db: &(impl ConnectionTrait + TransactionTrait),
        request_id: Uuid,
        owner_token: Uuid,
    ) -> Result<bool, DbError> {
        // A freshly-created reservation may not yet be visible on a read
        // replica. Force this locator query to the writer before opening the
        // lock-ordered release transaction.
        let Some(snapshot) = Self::find_by_request(db, request_id, true).await? else {
            return Ok(false);
        };
        if snapshot.owner_token != owner_token {
            return Ok(false);
        }
        let tx = db.begin().await?;
        let lock_balance = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [snapshot.user_id.into()],
        );
        let balance = UserBalance::find_by_statement(lock_balance)
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::not_found("UserBalance", snapshot.user_id.to_string()))?;
        let Some(reservation) = Self::find_by_request(&tx, request_id, true).await? else {
            return Err(DbError::Other(format!(
                "balance reservation {request_id} disappeared during release"
            )));
        };
        if reservation.status != "active" || reservation.owner_token != owner_token {
            tx.commit().await?;
            return Ok(false);
        }
        if balance.frozen_balance < reservation.amount {
            return Err(DbError::Other(format!(
                "balance reservation {request_id} exceeds frozen balance"
            )));
        }
        if reservation.amount > Decimal::ZERO {
            let update_balance = Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE user_balances SET available_balance = available_balance + $1, frozen_balance = frozen_balance - $1, updated_at = NOW() WHERE user_id = $2",
                [reservation.amount.into(), reservation.user_id.into()],
            );
            tx.execute(update_balance).await?;
        }
        let release_reservation = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE balance_reservations SET status = 'released', released_at = NOW(), release_kind = 'automatic', release_reason = 'dispatch aborted before settlement', released_by = NULL, updated_at = NOW() WHERE request_id = $1",
            [request_id.into()],
        );
        tx.execute(release_reservation).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Administratively release one active reservation. The expected user ID
    /// is part of the mutation contract so a request ID cannot release another
    /// user's funds. An exact retry of an already-completed administrative
    /// release returns the released row and the current balance breakdown;
    /// changing the owner token, actor, or normalized reason is a conflict. A
    /// late usage settlement sees a non-active reservation and falls back to
    /// the ordinary available-balance/debt consumption path.
    pub async fn admin_release(
        db: &(impl ConnectionTrait + TransactionTrait),
        user_id: Uuid,
        request_id: Uuid,
        expected_owner_token: Uuid,
        released_by: Uuid,
        reason: &str,
    ) -> Result<Option<AdminRequestReservationRelease>, DbError> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(DbError::Other(
                "balance reservation release reason must not be empty".to_string(),
            ));
        }
        if reason.chars().count() > MAX_BALANCE_RESERVATION_RELEASE_REASON_CHARS {
            return Err(DbError::Other(format!(
                "balance reservation release reason must not exceed {MAX_BALANCE_RESERVATION_RELEASE_REASON_CHARS} characters"
            )));
        }

        // Force request lookup onto the writer before acquiring locks in the
        // canonical balance-row -> reservation-row order.
        let Some(snapshot) = Self::find_by_request(db, request_id, true).await? else {
            return Ok(None);
        };
        if snapshot.user_id != user_id {
            return Ok(None);
        }

        let tx = db.begin().await?;
        let lock_balance = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [user_id.into()],
        );
        let balance = UserBalance::find_by_statement(lock_balance)
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::not_found("UserBalance", user_id.to_string()))?;
        let Some(reservation) = Self::find_by_request(&tx, request_id, true).await? else {
            return Err(DbError::Other(format!(
                "balance reservation {request_id} disappeared during administrative release"
            )));
        };
        if reservation.user_id != user_id || reservation.owner_token != expected_owner_token {
            tx.commit().await?;
            return Ok(None);
        }
        if reservation.status != "active" {
            if Self::is_administrative_release_tombstone(
                &reservation.status,
                reservation.release_kind.as_deref(),
            ) && reservation.released_by == Some(released_by)
                && reservation.release_reason.as_deref() == Some(reason)
            {
                let breakdown = Self::breakdown_for_locked_balance(&tx, balance).await?;
                tx.commit().await?;
                return Ok(Some(AdminRequestReservationRelease {
                    breakdown,
                    released_reservation: reservation,
                }));
            }
            tx.commit().await?;
            return Ok(None);
        }
        if balance.frozen_balance < reservation.amount {
            return Err(DbError::Other(format!(
                "balance reservation {request_id} exceeds frozen balance"
            )));
        }

        let update_balance = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE user_balances SET available_balance = available_balance + $1, frozen_balance = frozen_balance - $1, updated_at = NOW() WHERE user_id = $2 RETURNING *",
            [reservation.amount.into(), user_id.into()],
        );
        let updated_balance = UserBalance::find_by_statement(update_balance)
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::not_found("UserBalance", user_id.to_string()))?;
        let release_reservation = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE balance_reservations SET status = 'released', released_at = NOW(), release_kind = 'administrative', release_reason = $1, released_by = $2, updated_at = NOW() WHERE request_id = $3 AND status = 'active' RETURNING *",
            [
                reason.to_string().into(),
                released_by.into(),
                request_id.into(),
            ],
        );
        let released = Self::find_by_statement(release_reservation)
            .one(&tx)
            .await?
            .ok_or_else(|| {
                DbError::Other(format!(
                    "balance reservation {request_id} changed during administrative release"
                ))
            })?;
        let breakdown = Self::breakdown_for_locked_balance(&tx, updated_balance).await?;
        tx.commit().await?;
        Ok(Some(AdminRequestReservationRelease {
            breakdown,
            released_reservation: released,
        }))
    }
}

impl UserBalance {
    /// 总余额（可用 + 冻结）
    pub fn total_balance(&self) -> Decimal {
        self.available_balance + self.frozen_balance
    }

    /// 检查可用余额是否足够
    pub fn can_deduct(&self, amount: Decimal) -> bool {
        self.available_balance >= amount
    }
}

impl UserBalance {
    /// 获取或创建用户余额记录
    pub async fn get_or_create(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<UserBalance, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO user_balances (tenant_id, user_id) VALUES ($1, $2) ON CONFLICT (user_id) DO UPDATE SET updated_at = NOW() RETURNING *"#,
            [tenant_id.into(), user_id.into()],
        );
        let balance = UserBalance::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("get_or_create failed".to_string()))?;

        Ok(balance)
    }

    /// 根据用户ID查找余额
    pub async fn find_by_user(
        db: &impl ConnectionTrait,
        user_id: Uuid,
    ) -> Result<Option<UserBalance>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1",
            [user_id.into()],
        );
        let balance = UserBalance::find_by_statement(stmt).one(db).await?;
        Ok(balance)
    }

    /// Find a balance after atomically reclaiming any expired request
    /// reservations. This is the user-facing read path; the simpler
    /// `find_by_user` remains available inside existing transactions.
    pub async fn find_by_user_reclaiming_expired(
        db: &(impl ConnectionTrait + TransactionTrait),
        user_id: Uuid,
    ) -> Result<Option<UserBalance>, DbError> {
        let tx = db.begin().await?;
        let lock_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [user_id.into()],
        );
        let Some(balance) = UserBalance::find_by_statement(lock_stmt).one(&tx).await? else {
            tx.commit().await?;
            return Ok(None);
        };
        let mut reclaimed =
            BalanceReservation::reclaim_expired_for_locked_balances(&tx, vec![balance]).await?;
        let balance = reclaimed
            .balances
            .pop()
            .expect("the locked balance must be preserved during reclamation");
        tx.commit().await?;
        Ok(Some(balance))
    }

    /// Return the persisted balance split into request-owned and manually
    /// frozen funds after atomically reclaiming expired reservations.
    pub async fn find_breakdown_by_user(
        db: &(impl ConnectionTrait + TransactionTrait),
        user_id: Uuid,
    ) -> Result<Option<UserBalanceBreakdown>, DbError> {
        let tx = db.begin().await?;
        let lock_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [user_id.into()],
        );
        let Some(balance) = UserBalance::find_by_statement(lock_stmt).one(&tx).await? else {
            tx.commit().await?;
            return Ok(None);
        };
        let mut reclaimed =
            BalanceReservation::reclaim_expired_for_locked_balances(&tx, vec![balance]).await?;
        let balance = reclaimed
            .balances
            .pop()
            .expect("the locked balance must be preserved during reclamation");
        let active_reserved = reclaimed
            .active_totals
            .remove(&user_id)
            .expect("the locked balance must have an active reservation aggregate");
        let breakdown = BalanceReservation::breakdown_with_active_total(balance, active_reserved)?;
        tx.commit().await?;
        Ok(Some(breakdown))
    }

    /// Return an exact balance split and one bounded page of its active
    /// request reservations. Expiry reclamation, the aggregate, and the page
    /// query are protected by the same balance-row lock; only `limit + 1`
    /// detail rows are materialized while that lock is held.
    pub async fn find_breakdown_page_by_user(
        db: &(impl ConnectionTrait + TransactionTrait),
        user_id: Uuid,
        cursor: Option<BalanceReservationPageCursor>,
        limit: u64,
    ) -> Result<Option<UserBalanceBreakdownPage>, DbError> {
        validate_balance_reservation_page_size(limit)?;

        let tx = db.begin().await?;
        let lock_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [user_id.into()],
        );
        let Some(balance) = UserBalance::find_by_statement(lock_stmt).one(&tx).await? else {
            tx.commit().await?;
            return Ok(None);
        };
        let mut reclaimed =
            BalanceReservation::reclaim_expired_for_locked_balances(&tx, vec![balance]).await?;
        let balance = reclaimed
            .balances
            .pop()
            .expect("the locked balance must be preserved during reclamation");
        let active_reserved = reclaimed
            .active_totals
            .remove(&user_id)
            .expect("the locked balance must have an active reservation aggregate");
        let breakdown = BalanceReservation::breakdown_with_active_total(balance, active_reserved)?;
        let (reservations, next_cursor) =
            BalanceReservation::active_page_by_user(&tx, user_id, cursor, limit).await?;
        tx.commit().await?;
        Ok(Some(UserBalanceBreakdownPage {
            breakdown,
            reservations,
            next_cursor,
        }))
    }

    /// 批量根据用户ID查找余额
    pub async fn find_by_users(
        db: &impl ConnectionTrait,
        user_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, UserBalance>, DbError> {
        if user_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = ANY($1)",
            [user_ids.to_vec().into()],
        );
        let balances = UserBalance::find_by_statement(stmt).all(db).await?;
        Ok(balances.into_iter().map(|b| (b.user_id, b)).collect())
    }

    /// Batch balance read with the same expiry semantics as
    /// `find_by_user_reclaiming_expired`. Balance rows are locked in a stable
    /// order to preserve the lock order used by reservation settlement.
    pub async fn find_by_users_reclaiming_expired(
        db: &(impl ConnectionTrait + TransactionTrait),
        user_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, UserBalance>, DbError> {
        if user_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let mut user_ids = user_ids.to_vec();
        user_ids.sort_unstable();
        user_ids.dedup();

        let tx = db.begin().await?;
        let lock_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = ANY($1) ORDER BY user_id FOR UPDATE",
            [user_ids.into()],
        );
        let balances = UserBalance::find_by_statement(lock_stmt).all(&tx).await?;
        let balances = BalanceReservation::reclaim_expired_for_locked_balances(&tx, balances)
            .await?
            .balances;
        tx.commit().await?;
        Ok(balances
            .into_iter()
            .map(|balance| (balance.user_id, balance))
            .collect())
    }

    /// 充值（自身创建事务执行）
    pub async fn recharge(
        db: &(impl ConnectionTrait + TransactionTrait),
        user_id: Uuid,
        tenant_id: Uuid,
        amount: Decimal,
        order_id: Option<Uuid>,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction), DbError> {
        if amount <= Decimal::ZERO {
            return Err(DbError::Other(
                "balance recharge amount must be greater than zero".to_string(),
            ));
        }
        let tx = db.begin().await?;

        let result =
            Self::recharge_in_tx(&tx, user_id, tenant_id, amount, order_id, description).await?;

        tx.commit().await?;
        Ok(result)
    }

    /// 充值（在已有事务内执行）
    ///
    /// 与 [`recharge`] 功能相同，但不自行创建事务，接受外部传入的事务引用。
    /// 用于需要将充值操作与其它 DB 操作（如订单更新）放在同一事务中的场景。
    pub async fn recharge_in_tx(
        tx: &DatabaseTransaction,
        user_id: Uuid,
        tenant_id: Uuid,
        amount: Decimal,
        order_id: Option<Uuid>,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction), DbError> {
        if amount <= Decimal::ZERO {
            return Err(DbError::Other(
                "balance recharge amount must be greater than zero".to_string(),
            ));
        }
        // 先物化余额行，再加锁读取。如果两笔“首次充值”并发，
        // 直接 SELECT FOR UPDATE 会让两个事务都读到空集，导致第二笔
        // balance_transactions 的 balance_before/after 与实际余额不一致。
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO user_balances
               (user_id, tenant_id, available_balance, frozen_balance, total_recharged, total_consumed)
               VALUES ($1, $2, 0, 0, 0, 0)
               ON CONFLICT (user_id) DO NOTHING"#,
            [user_id.into(), tenant_id.into()],
        ))
        .await?;

        // 获取当前余额（加锁）
        let lock_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [user_id.into()],
        );
        let balance = UserBalance::find_by_statement(lock_stmt)
            .one(tx)
            .await?
            .ok_or_else(|| DbError::Other("recharge balance row disappeared".to_string()))?;

        let balance_before = balance.available_balance;
        let balance_after = balance_before + amount;

        // 已持有行锁，直接更新即可得到与流水一致的前后余额。
        let update_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_balances
               SET available_balance = available_balance + $1,
                   total_recharged = total_recharged + $1,
                   updated_at = NOW()
               WHERE user_id = $2
               RETURNING *"#,
            [amount.into(), user_id.into()],
        );
        let updated_balance = UserBalance::find_by_statement(update_stmt)
            .one(tx)
            .await?
            .ok_or_else(|| DbError::Other("recharge balance update failed".to_string()))?;

        // 记录交易
        let transaction = BalanceTransaction::create_internal(
            tx,
            updated_balance.tenant_id,
            user_id,
            order_id,
            None,
            TransactionType::Recharge,
            amount,
            balance_before,
            balance_after,
            description,
        )
        .await?;

        Ok((updated_balance, transaction))
    }

    /// 消费（事务内执行）
    pub async fn consume(
        db: &(impl ConnectionTrait + TransactionTrait),
        user_id: Uuid,
        amount: Decimal,
        usage_log_id: Option<Uuid>,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction), DbError> {
        // A zero-valued immutable usage ledger entry is a legitimate,
        // idempotently recorded terminal request (for example an upstream 429
        // before any tokens). Administrative consumption has no ledger ID and
        // must always be strictly positive.
        if amount < Decimal::ZERO || (amount == Decimal::ZERO && usage_log_id.is_none()) {
            return Err(DbError::Other(
                "balance consumption amount must be greater than zero unless it records a zero-cost usage ledger entry"
                    .to_string(),
            ));
        }
        let tx = db.begin().await?;

        let result = Self::consume_in_tx(&tx, user_id, amount, usage_log_id, description).await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn consume_in_tx(
        tx: &DatabaseTransaction,
        user_id: Uuid,
        amount: Decimal,
        usage_log_id: Option<Uuid>,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction), DbError> {
        let lock_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [user_id.into()],
        );
        let balance = UserBalance::find_by_statement(lock_stmt).one(tx).await?;

        let balance = match balance {
            Some(b) => b,
            None => return Err(DbError::not_found("UserBalance", user_id.to_string())),
        };
        let mut reclaimed =
            BalanceReservation::reclaim_expired_for_locked_balances(tx, vec![balance]).await?;
        let balance = reclaimed
            .balances
            .pop()
            .expect("the locked balance must be preserved during reclamation");

        // A usage log is the idempotency key for billable consumption. The
        // balance row lock serializes replays for this user; the partial
        // unique index remains the final guard against inconsistent callers.
        if let Some(usage_log_id) = usage_log_id {
            let existing =
                BalanceTransaction::find_consumption_by_usage_log(tx, usage_log_id).await?;
            if let Some(transaction) = existing {
                if transaction.user_id != user_id || transaction.amount != -amount {
                    return Err(DbError::Other(format!(
                        "usage log {usage_log_id} is already bound to a different balance consumption"
                    )));
                }
                return Ok((balance, transaction));
            }
        }

        // An admitted API request may legitimately cost more than the small
        // preflight threshold. Usage-ledger-backed consumption therefore
        // records the remainder as a negative available balance (auditable
        // debt) instead of leaving a durable settlement in a permanent retry
        // loop. Administrative/manual consumption keeps the strict
        // insufficient-balance check.
        if balance.available_balance < amount && usage_log_id.is_none() {
            return Err(DbError::insufficient_balance(
                amount.to_string(),
                balance.available_balance.to_string(),
            ));
        }

        let balance_before = balance.available_balance;
        let balance_after = balance_before - amount;

        let update_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_balances SET available_balance = available_balance - $1, total_consumed = total_consumed + $1, updated_at = NOW() WHERE user_id = $2 RETURNING *"#,
            [amount.into(), user_id.into()],
        );
        let updated_balance = UserBalance::find_by_statement(update_stmt)
            .one(tx)
            .await?
            .ok_or_else(|| DbError::not_found("UserBalance", user_id.to_string()))?;

        let transaction = BalanceTransaction::create_internal(
            tx,
            balance.tenant_id,
            user_id,
            None,
            usage_log_id,
            TransactionType::Consume,
            -amount,
            balance_before,
            balance_after,
            description,
        )
        .await?;

        Ok((updated_balance, transaction))
    }

    /// 冻结余额
    pub async fn freeze(
        db: &(impl ConnectionTrait + TransactionTrait),
        user_id: Uuid,
        amount: Decimal,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction), DbError> {
        if amount <= Decimal::ZERO {
            return Err(DbError::Other(
                "balance freeze amount must be greater than zero".to_string(),
            ));
        }
        let tx = db.begin().await?;

        let result = Self::freeze_in_tx(&tx, user_id, amount, description).await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn freeze_in_tx(
        tx: &DatabaseTransaction,
        user_id: Uuid,
        amount: Decimal,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction), DbError> {
        let lock_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [user_id.into()],
        );
        let balance = UserBalance::find_by_statement(lock_stmt).one(tx).await?;

        let balance = match balance {
            Some(b) => b,
            None => return Err(DbError::not_found("UserBalance", user_id.to_string())),
        };
        let mut reclaimed =
            BalanceReservation::reclaim_expired_for_locked_balances(tx, vec![balance]).await?;
        let balance = reclaimed
            .balances
            .pop()
            .expect("the locked balance must be preserved during reclamation");

        if balance.available_balance < amount {
            return Err(DbError::insufficient_balance(
                amount.to_string(),
                balance.available_balance.to_string(),
            ));
        }

        let balance_before = balance.available_balance;
        let balance_after = balance_before - amount;

        let update_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_balances SET available_balance = available_balance - $1, frozen_balance = frozen_balance + $1, updated_at = NOW() WHERE user_id = $2 RETURNING *"#,
            [amount.into(), user_id.into()],
        );
        let updated_balance = UserBalance::find_by_statement(update_stmt)
            .one(tx)
            .await?
            .ok_or_else(|| DbError::not_found("UserBalance", user_id.to_string()))?;

        let transaction = BalanceTransaction::create_internal(
            tx,
            balance.tenant_id,
            user_id,
            None,
            None,
            TransactionType::Freeze,
            -amount,
            balance_before,
            balance_after,
            description,
        )
        .await?;

        Ok((updated_balance, transaction))
    }

    /// 小费入账（tips 转为可用余额）
    ///
    /// 注意：调用方**必须**已在外部开启数据库事务（`db.begin()`），
    /// 此方法依赖事务内的 `SELECT ... FOR UPDATE` 行锁保证并发安全。
    /// 当前唯一调用方 node_tips.rs 已满足此前提。
    ///
    /// 签名限定 `&DatabaseTransaction` 而非 `&impl ConnectionTrait`，
    /// 以在编译期强制事务上下文约束。
    pub async fn credit_tips(
        db: &DatabaseTransaction,
        user_id: Uuid,
        tenant_id: Uuid,
        amount: Decimal,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction), DbError> {
        if amount <= Decimal::ZERO {
            return Err(DbError::Other(
                "balance tip credit amount must be greater than zero".to_string(),
            ));
        }
        let lock_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [user_id.into()],
        );
        let balance = UserBalance::find_by_statement(lock_stmt).one(db).await?;

        let effective_tenant_id = balance.as_ref().map(|b| b.tenant_id).unwrap_or(tenant_id);

        let upsert_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO user_balances (user_id, tenant_id, available_balance, total_recharged) VALUES ($1, $2, $3, $3) ON CONFLICT (user_id) DO UPDATE SET available_balance = user_balances.available_balance + $3, total_recharged = user_balances.total_recharged + $3, updated_at = NOW() RETURNING *"#,
            [user_id.into(), effective_tenant_id.into(), amount.into()],
        );
        let updated_balance = UserBalance::find_by_statement(upsert_stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("credit_tips upsert failed".to_string()))?;

        let balance_after = updated_balance.available_balance;
        let balance_before = balance_after - amount;

        let transaction = BalanceTransaction::create_internal(
            db,
            updated_balance.tenant_id,
            user_id,
            None,
            None,
            TransactionType::TipCredit,
            amount,
            balance_before,
            balance_after,
            description,
        )
        .await?;

        Ok((updated_balance, transaction))
    }

    /// 解冻余额
    pub async fn unfreeze(
        db: &(impl ConnectionTrait + TransactionTrait),
        user_id: Uuid,
        amount: Decimal,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction), DbError> {
        if amount <= Decimal::ZERO {
            return Err(DbError::Other(
                "balance unfreeze amount must be greater than zero".to_string(),
            ));
        }

        let tx = db.begin().await?;

        match Self::unfreeze_in_tx(&tx, user_id, amount, description).await {
            Ok(result) => {
                tx.commit().await?;
                Ok(result)
            }
            Err(error) if error.is_insufficient_balance() => {
                // Expiry reclamation is useful state repair independent from
                // the rejected manual release, so preserve it.
                tx.commit().await?;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    async fn unfreeze_in_tx(
        tx: &DatabaseTransaction,
        user_id: Uuid,
        amount: Decimal,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction), DbError> {
        let lock_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [user_id.into()],
        );
        let balance = UserBalance::find_by_statement(lock_stmt).one(tx).await?;

        let balance = match balance {
            Some(b) => b,
            None => return Err(DbError::not_found("UserBalance", user_id.to_string())),
        };

        let mut reclaimed =
            BalanceReservation::reclaim_expired_for_locked_balances(tx, vec![balance]).await?;
        let balance = reclaimed
            .balances
            .pop()
            .expect("the locked balance must be preserved during reclamation");

        // After expiry reclamation, every row still marked active owns frozen
        // funds. Keep those request-owned funds separate from manual freezes.
        let active_reserved = reclaimed
            .active_totals
            .remove(&user_id)
            .expect("the locked balance must have an active reservation aggregate");
        if balance.frozen_balance < active_reserved {
            return Err(DbError::Other(format!(
                "active balance reservations for user {user_id} exceed frozen balance"
            )));
        }
        let manually_frozen = balance.frozen_balance - active_reserved;
        if manually_frozen < amount {
            return Err(DbError::insufficient_balance(
                amount.to_string(),
                manually_frozen.to_string(),
            ));
        }

        let balance_before = balance.available_balance;
        let balance_after = balance_before + amount;

        let update_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_balances SET available_balance = available_balance + $1, frozen_balance = frozen_balance - $1, updated_at = NOW() WHERE user_id = $2 RETURNING *"#,
            [amount.into(), user_id.into()],
        );
        let updated_balance = UserBalance::find_by_statement(update_stmt)
            .one(tx)
            .await?
            .ok_or_else(|| DbError::not_found("UserBalance", user_id.to_string()))?;

        let transaction = BalanceTransaction::create_internal(
            tx,
            balance.tenant_id,
            user_id,
            None,
            None,
            TransactionType::Unfreeze,
            amount,
            balance_before,
            balance_after,
            description,
        )
        .await?;

        Ok((updated_balance, transaction))
    }

    /// Atomically apply or replay an administrator's manual recharge,
    /// consumption, freeze, or unfreeze.
    ///
    /// The globally unique hashed key is claimed in the same transaction as
    /// the balance row lock, balance transaction, and result snapshot. A
    /// concurrent replica therefore either observes the completed snapshot or
    /// waits for the first transaction to roll back and performs the operation
    /// itself. The plaintext key is never persisted.
    #[allow(clippy::too_many_arguments)]
    pub async fn apply_admin_manual_operation(
        db: &(impl ConnectionTrait + TransactionTrait),
        kind: ManualBalanceOperationKind,
        tenant_id: Uuid,
        user_id: Uuid,
        actor_user_id: Uuid,
        amount: Decimal,
        reason: &str,
        idempotency_key: &str,
    ) -> Result<ManualBalanceOperationDecision, DbError> {
        if amount <= Decimal::ZERO {
            return Err(DbError::Other(
                "administrator balance operation amount must be greater than zero".to_string(),
            ));
        }
        let reason = reason.trim();
        if reason.is_empty() {
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
        let tx = db.begin().await?;

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
            "SELECT * FROM admin_balance_operations WHERE idempotency_key_hash = $1 FOR UPDATE",
            [key_hash.clone().into()],
        );
        let claim = AdminBalanceOperation::find_by_statement(claim_stmt)
            .one(&tx)
            .await?
            .ok_or_else(|| {
                DbError::Other("administrator balance idempotency claim disappeared".to_string())
            })?;
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
            tx.commit().await?;
            return Ok(ManualBalanceOperationDecision::Conflict);
        }
        if let Some(outcome) = claim.completed_outcome()? {
            tx.commit().await?;
            return Ok(ManualBalanceOperationDecision::Completed(outcome));
        }

        let operation = match kind {
            ManualBalanceOperationKind::Recharge => {
                Self::recharge_in_tx(&tx, user_id, tenant_id, amount, None, Some(reason)).await
            }
            ManualBalanceOperationKind::Consume => {
                Self::consume_in_tx(&tx, user_id, amount, None, Some(reason)).await
            }
            ManualBalanceOperationKind::Freeze => {
                Self::freeze_in_tx(&tx, user_id, amount, Some(reason)).await
            }
            ManualBalanceOperationKind::Unfreeze => {
                Self::unfreeze_in_tx(&tx, user_id, amount, Some(reason)).await
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
                    "DELETE FROM admin_balance_operations WHERE id = $1 AND completed_at IS NULL",
                    [claim.id.into()],
                ))
                .await?;
                tx.commit().await?;
                return Err(error);
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
                   WHERE id = $5 AND completed_at IS NULL
                   RETURNING *"#,
                [
                    balance_transaction.id.into(),
                    balance_transaction.balance_before.into(),
                    balance_transaction.balance_after.into(),
                    updated_balance.frozen_balance.into(),
                    claim.id.into(),
                ],
            ))
            .one(&tx)
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
        tx.commit().await?;
        Ok(ManualBalanceOperationDecision::Completed(outcome))
    }
}

/// 余额变动记录模型
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct BalanceTransaction {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub order_id: Option<Uuid>,
    pub usage_log_id: Option<Uuid>,
    pub transaction_type: String,
    pub amount: Decimal,
    pub balance_before: Decimal,
    pub balance_after: Decimal,
    pub currency: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl BalanceTransaction {
    async fn find_consumption_by_usage_log(
        db: &impl ConnectionTrait,
        usage_log_id: Uuid,
    ) -> Result<Option<BalanceTransaction>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM balance_transactions WHERE usage_log_id = $1 AND transaction_type = 'consume'",
            [usage_log_id.into()],
        );
        Ok(BalanceTransaction::find_by_statement(stmt).one(db).await?)
    }

    /// 内部创建交易记录
    #[allow(clippy::too_many_arguments)]
    async fn create_internal(
        db: &impl sea_orm::ConnectionTrait,
        tenant_id: Uuid,
        user_id: Uuid,
        order_id: Option<Uuid>,
        usage_log_id: Option<Uuid>,
        transaction_type: TransactionType,
        amount: Decimal,
        balance_before: Decimal,
        balance_after: Decimal,
        description: Option<&str>,
    ) -> Result<BalanceTransaction, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO balance_transactions (tenant_id, user_id, order_id, usage_log_id, transaction_type, amount, balance_before, balance_after, description) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING *"#,
            [
                tenant_id.into(),
                user_id.into(),
                order_id.into(),
                usage_log_id.into(),
                transaction_type.as_str().into(),
                amount.into(),
                balance_before.into(),
                balance_after.into(),
                description.map(String::from).into(),
            ],
        );
        let transaction = BalanceTransaction::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create transaction failed".to_string()))?;

        Ok(transaction)
    }

    /// 查找用户的交易记录
    pub async fn find_by_user(
        db: &impl ConnectionTrait,
        user_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<BalanceTransaction>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM balance_transactions WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
            [user_id.into(), limit.into(), offset.into()],
        );
        let transactions = BalanceTransaction::find_by_statement(stmt).all(db).await?;
        Ok(transactions)
    }

    /// 获取交易类型枚举
    pub fn get_transaction_type(&self) -> Option<TransactionType> {
        TransactionType::parse(&self.transaction_type)
    }
}

#[cfg(test)]
mod reservation_tests {
    use super::*;

    fn minimum() -> Decimal {
        Decimal::new(1, 1)
    }

    #[test]
    fn administrative_release_is_a_reactivation_tombstone() {
        assert!(BalanceReservation::is_administrative_release_tombstone(
            "released",
            Some("administrative")
        ));
        assert!(!BalanceReservation::is_administrative_release_tombstone(
            "released",
            Some("automatic")
        ));
        assert!(!BalanceReservation::is_administrative_release_tombstone(
            "active",
            Some("administrative")
        ));
    }

    #[test]
    fn explicit_reservation_cannot_succeed_with_zero_available_balance() {
        let error = BalanceReservation::amount_to_reserve(Decimal::ZERO, Decimal::ONE, minimum())
            .unwrap_err();
        assert!(error.is_insufficient_balance());
    }

    #[test]
    fn bounded_reservation_owns_the_minimum_admission_balance() {
        assert_eq!(
            BalanceReservation::amount_to_reserve(Decimal::ONE, Decimal::ZERO, minimum()).unwrap(),
            minimum()
        );
    }

    #[test]
    fn explicit_reservation_can_own_all_available_balance_after_reclamation() {
        let reclaimed_available = Decimal::new(25, 1);
        assert_eq!(
            BalanceReservation::amount_to_reserve(
                reclaimed_available,
                reclaimed_available,
                minimum(),
            )
            .unwrap(),
            reclaimed_available
        );
    }

    #[test]
    fn bounded_reservation_rejects_cost_above_available_balance() {
        let error =
            BalanceReservation::amount_to_reserve(Decimal::ONE, Decimal::from(2), minimum())
                .unwrap_err();
        assert!(error.is_insufficient_balance());
    }

    #[test]
    fn settlement_transaction_amount_matches_its_balance_delta() {
        let consumed = Decimal::from(3);
        let (before, after) = BalanceReservation::settlement_available_balances(
            Decimal::from(2),
            Decimal::from(8),
            consumed,
        );

        assert_eq!(before, Decimal::from(10));
        assert_eq!(after, Decimal::from(7));
        assert_eq!(after - before, -consumed);
    }

    #[test]
    fn reservation_page_size_is_bounded_in_the_data_layer() {
        assert!(validate_balance_reservation_page_size(1).is_ok());
        assert!(validate_balance_reservation_page_size(MAX_BALANCE_RESERVATION_PAGE_SIZE).is_ok());
        assert!(validate_balance_reservation_page_size(0).is_err());
        assert!(
            validate_balance_reservation_page_size(MAX_BALANCE_RESERVATION_PAGE_SIZE + 1).is_err()
        );
    }
}
