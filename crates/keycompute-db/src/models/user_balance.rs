//! 用户余额模型

use crate::DbError;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
struct ActiveReservationTotal {
    amount: Decimal,
}

impl BalanceReservation {
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
        requested: Option<Decimal>,
        minimum_available: Decimal,
    ) -> Result<Decimal, DbError> {
        if minimum_available < Decimal::ZERO {
            return Err(DbError::Other(
                "minimum available balance must not be negative".to_string(),
            ));
        }
        if requested.is_some_and(|amount| amount < Decimal::ZERO) {
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
        // same transaction as the reservation. In particular, an unbounded
        // request that follows another all-balance reservation sees zero here
        // and is rejected instead of creating a zero-valued reservation.
        let amount = requested
            .map(|amount| amount.max(minimum_available))
            .unwrap_or(available);
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

    /// Release expired reservations after their owning balance rows have been
    /// locked. Callers that lock more than one balance must do so in user ID
    /// order before entering this helper.
    async fn reclaim_expired_for_locked_balances(
        tx: &DatabaseTransaction,
        mut balances: Vec<UserBalance>,
    ) -> Result<Vec<UserBalance>, DbError> {
        if balances.is_empty() {
            return Ok(balances);
        }

        let user_ids = balances
            .iter()
            .map(|balance| balance.user_id)
            .collect::<Vec<_>>();
        let expired_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM balance_reservations WHERE user_id = ANY($1) AND status = 'active' AND expires_at <= NOW() ORDER BY user_id, id FOR UPDATE",
            [user_ids.into()],
        );
        let expired = Self::find_by_statement(expired_stmt).all(tx).await?;
        if expired.is_empty() {
            return Ok(balances);
        }

        let mut expired_by_user = std::collections::HashMap::new();
        for reservation in &expired {
            let amount = expired_by_user
                .entry(reservation.user_id)
                .or_insert(Decimal::ZERO);
            *amount += reservation.amount;
        }
        for balance in &balances {
            let expired_amount = expired_by_user
                .get(&balance.user_id)
                .copied()
                .unwrap_or(Decimal::ZERO);
            if balance.frozen_balance < expired_amount {
                return Err(DbError::Other(format!(
                    "expired balance reservations for user {} exceed frozen balance",
                    balance.user_id
                )));
            }
        }

        for balance in &mut balances {
            let expired_amount = expired_by_user
                .get(&balance.user_id)
                .copied()
                .unwrap_or(Decimal::ZERO);
            if expired_amount <= Decimal::ZERO {
                continue;
            }
            let update_balance = Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE user_balances SET available_balance = available_balance + $1, frozen_balance = frozen_balance - $1, updated_at = NOW() WHERE user_id = $2 RETURNING *",
                [expired_amount.into(), balance.user_id.into()],
            );
            *balance = UserBalance::find_by_statement(update_balance)
                .one(tx)
                .await?
                .ok_or_else(|| DbError::not_found("UserBalance", balance.user_id.to_string()))?;
        }

        // Update exactly the rows included in the amount calculation. Using a
        // second expires_at <= NOW() predicate could catch a reservation whose
        // deadline passed between the SELECT and UPDATE without refunding it.
        let expired_ids = expired
            .into_iter()
            .map(|reservation| reservation.id)
            .collect::<Vec<_>>();
        let expire_rows = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE balance_reservations SET status = 'expired', updated_at = NOW() WHERE id = ANY($1) AND status = 'active'",
            [expired_ids.into()],
        );
        tx.execute(expire_rows).await?;

        Ok(balances)
    }

    /// Atomically move an estimated request cost from available to frozen
    /// balance. `amount = None` reserves the whole currently available balance,
    /// which closes the concurrency overspend window for requests without an
    /// explicit output-token ceiling.
    pub async fn reserve(
        db: &(impl ConnectionTrait + TransactionTrait),
        tenant_id: Uuid,
        user_id: Uuid,
        request_id: Uuid,
        amount: Option<Decimal>,
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
        balance = Self::reclaim_expired_for_locked_balances(&tx, vec![balance])
            .await?
            .pop()
            .expect("the locked balance must be preserved during reclamation");

        let owner_token = Uuid::new_v4();
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
                "UPDATE balance_reservations SET owner_token = $1, amount = $2, status = 'active', usage_log_id = NULL, expires_at = $3, settled_at = NULL, updated_at = NOW() WHERE request_id = $4 RETURNING *",
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
            tx.commit().await?;
            return Ok(Some((balance, transaction)));
        }
        if reservation.status != "active" {
            tx.commit().await?;
            return Ok(None);
        }
        if balance.frozen_balance < reservation.amount {
            return Err(DbError::Other(format!(
                "balance reservation {request_id} exceeds frozen balance"
            )));
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
            "UPDATE balance_reservations SET status = 'released', updated_at = NOW() WHERE request_id = $1",
            [request_id.into()],
        );
        tx.execute(release_reservation).await?;
        tx.commit().await?;
        Ok(true)
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
        let balance = BalanceReservation::reclaim_expired_for_locked_balances(&tx, vec![balance])
            .await?
            .pop()
            .expect("the locked balance must be preserved during reclamation");
        tx.commit().await?;
        Ok(Some(balance))
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
        let balances =
            BalanceReservation::reclaim_expired_for_locked_balances(&tx, balances).await?;
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
        let tx = db.begin().await?;

        let lock_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [user_id.into()],
        );
        let balance = UserBalance::find_by_statement(lock_stmt).one(&tx).await?;

        let balance = match balance {
            Some(b) => b,
            None => return Err(DbError::not_found("UserBalance", user_id.to_string())),
        };

        // A usage log is the idempotency key for billable consumption. The
        // balance row lock serializes replays for this user; the partial
        // unique index remains the final guard against inconsistent callers.
        if let Some(usage_log_id) = usage_log_id {
            let existing =
                BalanceTransaction::find_consumption_by_usage_log(&tx, usage_log_id).await?;
            if let Some(transaction) = existing {
                if transaction.user_id != user_id || transaction.amount != -amount {
                    return Err(DbError::Other(format!(
                        "usage log {usage_log_id} is already bound to a different balance consumption"
                    )));
                }
                tx.commit().await?;
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
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::not_found("UserBalance", user_id.to_string()))?;

        let transaction = BalanceTransaction::create_internal(
            &tx,
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

        tx.commit().await?;

        Ok((updated_balance, transaction))
    }

    /// 冻结余额
    pub async fn freeze(
        db: &(impl ConnectionTrait + TransactionTrait),
        user_id: Uuid,
        amount: Decimal,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction), DbError> {
        let tx = db.begin().await?;

        let lock_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [user_id.into()],
        );
        let balance = UserBalance::find_by_statement(lock_stmt).one(&tx).await?;

        let balance = match balance {
            Some(b) => b,
            None => return Err(DbError::not_found("UserBalance", user_id.to_string())),
        };

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
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::not_found("UserBalance", user_id.to_string()))?;

        let transaction = BalanceTransaction::create_internal(
            &tx,
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

        tx.commit().await?;

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
        let tx = db.begin().await?;

        let lock_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
            [user_id.into()],
        );
        let balance = UserBalance::find_by_statement(lock_stmt).one(&tx).await?;

        let balance = match balance {
            Some(b) => b,
            None => return Err(DbError::not_found("UserBalance", user_id.to_string())),
        };

        let balance = BalanceReservation::reclaim_expired_for_locked_balances(&tx, vec![balance])
            .await?
            .pop()
            .expect("the locked balance must be preserved during reclamation");

        // After expiry reclamation, every row still marked active owns frozen
        // funds. Keep those request-owned funds separate from manual freezes.
        let reserved_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COALESCE(SUM(amount), 0) AS amount FROM balance_reservations WHERE user_id = $1 AND status = 'active'",
            [user_id.into()],
        );
        let active_reserved = ActiveReservationTotal::find_by_statement(reserved_stmt)
            .one(&tx)
            .await?
            .map(|total| total.amount)
            .unwrap_or(Decimal::ZERO);
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
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::not_found("UserBalance", user_id.to_string()))?;

        let transaction = BalanceTransaction::create_internal(
            &tx,
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

        tx.commit().await?;

        Ok((updated_balance, transaction))
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
    fn unbounded_reservation_cannot_succeed_with_zero_available_balance() {
        let error =
            BalanceReservation::amount_to_reserve(Decimal::ZERO, None, minimum()).unwrap_err();
        assert!(error.is_insufficient_balance());
    }

    #[test]
    fn bounded_reservation_owns_the_minimum_admission_balance() {
        assert_eq!(
            BalanceReservation::amount_to_reserve(Decimal::ONE, Some(Decimal::ZERO), minimum(),)
                .unwrap(),
            minimum()
        );
    }

    #[test]
    fn unbounded_reservation_owns_all_available_balance_after_reclamation() {
        let reclaimed_available = Decimal::new(25, 1);
        assert_eq!(
            BalanceReservation::amount_to_reserve(reclaimed_available, None, minimum(),).unwrap(),
            reclaimed_available
        );
    }

    #[test]
    fn bounded_reservation_rejects_cost_above_available_balance() {
        let error =
            BalanceReservation::amount_to_reserve(Decimal::ONE, Some(Decimal::from(2)), minimum())
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
}
