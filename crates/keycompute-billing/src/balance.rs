//! 余额服务
//!
//! 封装用户余额操作的业务逻辑，提供统一的事务管理
//!
//! ## 架构定位
//! - 业务层：负责余额相关业务规则
//! - 数据层（keycompute-db）：负责数据库持久化

pub use keycompute_db::models::user_balance::{
    AdminRequestReservationRelease, BalanceReservationPageCursor,
    MAX_ADMIN_BALANCE_IDEMPOTENCY_KEY_BYTES, MAX_ADMIN_BALANCE_OPERATION_REASON_CHARS,
    MAX_BALANCE_RESERVATION_PAGE_SIZE, MAX_BALANCE_RESERVATION_RELEASE_REASON_CHARS,
    ManualBalanceOperationDecision, ManualBalanceOperationKind, ManualBalanceOperationOutcome,
    UserBalanceBreakdown, UserBalanceBreakdownPage,
};
use keycompute_db::{BalanceReservation, BalanceTransaction, DbRouter, UserBalance};
use keycompute_types::{KeyComputeError, Result};
use rust_decimal::Decimal;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

/// 余额不足阈值（元）
/// 当用户余额低于此值时，拒绝请求
pub fn min_balance_threshold() -> Decimal {
    // 使用精确构造 0.1（1 × 10^-1），避免 f64 中间表示引入精度误差
    Decimal::new(1, 1)
}

/// 余额服务
///
/// 封装用户余额操作的业务逻辑，提供：
/// - 查询余额
/// - 充值
/// - 消费扣款
/// - 冻结/解冻
#[derive(Clone)]
pub struct BalanceService {
    pool: Arc<DbRouter>,
}

impl std::fmt::Debug for BalanceService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BalanceService")
            .field("pool", &"DatabaseConnection")
            .finish()
    }
}

impl BalanceService {
    /// 创建新的余额服务
    pub fn new(pool: Arc<DbRouter>) -> Self {
        Self { pool }
    }

    /// 获取或创建用户余额记录
    ///
    /// 如果记录不存在，会自动创建
    pub async fn get_or_create(&self, tenant_id: Uuid, user_id: Uuid) -> Result<UserBalance> {
        UserBalance::get_or_create(self.pool.as_ref(), tenant_id, user_id)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to get or create balance: {}", e))
            })
    }

    /// 查询用户余额
    ///
    /// 返回 `None` 表示用户没有余额记录。查询前会原子回收已过期的请求预留，
    /// 因而调用方看到的是当前可用余额。
    pub async fn find_by_user(&self, user_id: Uuid) -> Result<Option<UserBalance>> {
        UserBalance::find_by_user_reclaiming_expired(self.pool.as_ref(), user_id)
            .await
            .map_err(|e| KeyComputeError::DatabaseError(format!("Failed to find balance: {}", e)))
    }

    /// 批量查询用户余额
    ///
    /// 返回 HashMap<user_id, UserBalance>，用于避免 N+1 查询
    pub async fn find_by_users(
        &self,
        user_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, UserBalance>> {
        UserBalance::find_by_users_reclaiming_expired(self.pool.as_ref(), user_ids)
            .await
            .map_err(|e| KeyComputeError::DatabaseError(format!("Failed to find balances: {}", e)))
    }

    /// Query one user's balance with request-owned and manual frozen funds
    /// separated. Expired request reservations are reclaimed first.
    pub async fn find_breakdown_by_user(
        &self,
        user_id: Uuid,
    ) -> Result<Option<UserBalanceBreakdown>> {
        UserBalance::find_breakdown_by_user(self.pool.as_ref(), user_id)
            .await
            .map_err(|error| {
                KeyComputeError::DatabaseError(format!("Failed to find balance breakdown: {error}"))
            })
    }

    /// Query one user's exact balance breakdown together with one bounded,
    /// keyset-paginated page of active request reservations.
    pub async fn find_breakdown_page_by_user(
        &self,
        user_id: Uuid,
        cursor: Option<BalanceReservationPageCursor>,
        limit: u64,
    ) -> Result<Option<UserBalanceBreakdownPage>> {
        UserBalance::find_breakdown_page_by_user(self.pool.as_ref(), user_id, cursor, limit)
            .await
            .map_err(|error| {
                KeyComputeError::DatabaseError(format!(
                    "Failed to find balance reservation page: {error}"
                ))
            })
    }

    /// Reclaim expired reservations without requiring a balance read or a new
    /// request. Returns the number of reservation rows reclaimed.
    pub async fn reclaim_expired_request_reservations(&self, batch_size: u64) -> Result<u64> {
        BalanceReservation::reclaim_expired(self.pool.as_ref(), batch_size)
            .await
            .map_err(|error| {
                KeyComputeError::DatabaseError(format!(
                    "Failed to reclaim expired request balance reservations: {error}"
                ))
            })
    }

    /// Persistently reserve the estimated maximum charge before dispatch.
    ///
    /// The estimate is mandatory. Callers must calculate an explicit bounded
    /// amount from their configured output/input risk limits; there is no
    /// sentinel value for an implicit reservation size.
    pub async fn reserve_request(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
        request_id: Uuid,
        amount: Decimal,
        reservation_ttl: Duration,
    ) -> Result<BalanceReservation> {
        self.reserve_request_with_owner_token(
            user_id,
            tenant_id,
            request_id,
            Uuid::new_v4(),
            amount,
            reservation_ttl,
        )
        .await
    }

    /// Persistently reserve a request using an ownership token generated by
    /// the caller before the database await begins. This lets an RAII guard
    /// compensate an ambiguous commit if its task is cancelled before the
    /// successful result is delivered.
    pub async fn reserve_request_with_owner_token(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
        request_id: Uuid,
        owner_token: Uuid,
        amount: Decimal,
        reservation_ttl: Duration,
    ) -> Result<BalanceReservation> {
        let reservation_ttl = chrono::Duration::from_std(reservation_ttl).map_err(|error| {
            KeyComputeError::ValidationError(format!(
                "Balance reservation TTL is out of range: {error}"
            ))
        })?;
        let expires_at = chrono::Utc::now()
            .checked_add_signed(reservation_ttl)
            .ok_or_else(|| {
                KeyComputeError::ValidationError(
                    "Balance reservation expiry is out of range".to_string(),
                )
            })?;
        BalanceReservation::reserve(
            self.pool.as_ref(),
            tenant_id,
            user_id,
            request_id,
            owner_token,
            amount,
            min_balance_threshold(),
            expires_at,
        )
        .await
        .map_err(|error| {
            if error.is_insufficient_balance() {
                KeyComputeError::ValidationError(format!(
                    "Insufficient balance for request reservation: {error}"
                ))
            } else {
                KeyComputeError::DatabaseError(format!(
                    "Failed to reserve request balance: {error}"
                ))
            }
        })
    }

    /// Release a reservation when dispatch never transferred ownership to the
    /// normal durable billing settlement path.
    pub async fn release_request_reservation(
        &self,
        request_id: Uuid,
        owner_token: Uuid,
    ) -> Result<bool> {
        BalanceReservation::release(self.pool.as_ref(), request_id, owner_token)
            .await
            .map_err(|error| {
                KeyComputeError::DatabaseError(format!(
                    "Failed to release request balance reservation: {error}"
                ))
            })
    }

    /// Release one request-owned reservation under an administrator's audit
    /// identity. The user ID guard prevents cross-user release by request ID.
    /// A retry must reuse the same owner token, actor, and normalized reason;
    /// an exact retry replays successfully without releasing funds twice.
    pub async fn admin_release_request_reservation(
        &self,
        user_id: Uuid,
        request_id: Uuid,
        expected_owner_token: Uuid,
        released_by: Uuid,
        reason: &str,
    ) -> Result<Option<AdminRequestReservationRelease>> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(KeyComputeError::ValidationError(
                "Balance reservation release reason must not be empty".to_string(),
            ));
        }
        if reason.chars().count() > MAX_BALANCE_RESERVATION_RELEASE_REASON_CHARS {
            return Err(KeyComputeError::ValidationError(format!(
                "Balance reservation release reason must not exceed {MAX_BALANCE_RESERVATION_RELEASE_REASON_CHARS} characters"
            )));
        }
        BalanceReservation::admin_release(
            self.pool.as_ref(),
            user_id,
            request_id,
            expected_owner_token,
            released_by,
            reason,
        )
        .await
        .map_err(|error| {
            KeyComputeError::DatabaseError(format!(
                "Failed to administratively release request balance reservation: {error}"
            ))
        })
    }

    /// Apply a usage-ledger charge to a matching active reservation.
    pub async fn settle_request_reservation(
        &self,
        request_id: Uuid,
        expected_owner_token: Option<Uuid>,
        amount: Decimal,
        usage_log_id: Uuid,
        description: Option<&str>,
    ) -> Result<Option<(UserBalance, BalanceTransaction)>> {
        BalanceReservation::settle(
            self.pool.as_ref(),
            request_id,
            expected_owner_token,
            amount,
            usage_log_id,
            description,
        )
        .await
        .map_err(|error| {
            KeyComputeError::DatabaseError(format!(
                "Failed to settle request balance reservation: {error}"
            ))
        })
    }

    /// 充值
    ///
    /// # 参数
    /// - `user_id`: 用户 ID
    /// - `tenant_id`: 租户 ID（用于创建新记录时）
    /// - `amount`: 充值金额（必须为正数）
    /// - `order_id`: 关联的支付订单 ID
    /// - `description`: 交易描述
    ///
    /// # 返回
    /// - 更新后的余额
    /// - 交易记录
    pub async fn recharge(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
        amount: Decimal,
        order_id: Option<Uuid>,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction)> {
        if amount <= Decimal::ZERO {
            return Err(KeyComputeError::ValidationError(
                "Balance recharge amount must be greater than zero".to_string(),
            ));
        }
        let result = UserBalance::recharge(
            self.pool.as_ref(),
            user_id,
            tenant_id,
            amount,
            order_id,
            description,
        )
        .await;

        match result {
            Ok((balance, transaction)) => {
                tracing::info!(
                    user_id = %user_id,
                    amount = %amount,
                    new_balance = %balance.available_balance,
                    "Balance recharged successfully"
                );
                Ok((balance, transaction))
            }
            Err(e) => Err(KeyComputeError::DatabaseError(format!(
                "Failed to recharge balance: {}",
                e
            ))),
        }
    }

    /// 消费扣款
    ///
    /// # 参数
    /// - `user_id`: 用户 ID
    /// - `amount`: 消费金额（必须为正数）
    /// - `usage_log_id`: 关联的用量日志 ID
    /// - `description`: 交易描述
    ///
    /// # 返回
    /// - 更新后的余额
    /// - 交易记录
    ///
    /// # 错误
    /// - `ValidationError`: 余额不足或用户不存在
    pub async fn consume(
        &self,
        user_id: Uuid,
        amount: Decimal,
        usage_log_id: Option<Uuid>,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction)> {
        // Zero-cost terminal usage still needs an idempotent ledger-backed
        // balance side effect. Only that internal path may consume zero.
        if amount < Decimal::ZERO || (amount == Decimal::ZERO && usage_log_id.is_none()) {
            return Err(KeyComputeError::ValidationError(
                "Balance consumption amount must be greater than zero unless it records a zero-cost usage ledger entry"
                    .to_string(),
            ));
        }
        let result = UserBalance::consume(
            self.pool.as_ref(),
            user_id,
            amount,
            usage_log_id,
            description,
        )
        .await;

        match result {
            Ok((balance, transaction)) => {
                tracing::info!(
                    user_id = %user_id,
                    amount = %amount,
                    new_balance = %balance.available_balance,
                    "Balance consumed successfully"
                );
                Ok((balance, transaction))
            }
            Err(e) if e.is_insufficient_balance() => Err(KeyComputeError::ValidationError(
                format!("Insufficient balance for user {user_id}: {e}"),
            )),
            Err(e) if e.is_not_found() => Err(KeyComputeError::ValidationError(format!(
                "User balance not found for user {}",
                user_id
            ))),
            Err(e) => Err(KeyComputeError::DatabaseError(format!(
                "Failed to consume balance: {}",
                e
            ))),
        }
    }

    /// 冻结余额
    ///
    /// 将可用余额转移到冻结余额
    pub async fn freeze(
        &self,
        user_id: Uuid,
        amount: Decimal,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction)> {
        if amount <= Decimal::ZERO {
            return Err(KeyComputeError::ValidationError(
                "Balance freeze amount must be greater than zero".to_string(),
            ));
        }
        let result = UserBalance::freeze(self.pool.as_ref(), user_id, amount, description).await;

        match result {
            Ok((balance, transaction)) => {
                tracing::info!(
                    user_id = %user_id,
                    amount = %amount,
                    frozen_balance = %balance.frozen_balance,
                    "Balance frozen successfully"
                );
                Ok((balance, transaction))
            }
            Err(e) if e.is_insufficient_balance() => Err(KeyComputeError::ValidationError(
                format!("Insufficient available balance for user {user_id}: {e}"),
            )),
            Err(e) if e.is_not_found() => Err(KeyComputeError::ValidationError(format!(
                "User balance not found for user {}",
                user_id
            ))),
            Err(e) => Err(KeyComputeError::DatabaseError(format!(
                "Failed to freeze balance: {}",
                e
            ))),
        }
    }

    /// 解冻余额
    ///
    /// 将冻结余额转回可用余额
    ///
    /// # 参数
    /// - `user_id`: 用户 ID
    /// - `amount`: 解冻金额（必须为正数）
    /// - `description`: 交易描述
    ///
    /// # 返回
    /// - 更新后的余额
    /// - 交易记录
    ///
    /// # 错误
    /// - `ValidationError`: 冻结余额不足或用户不存在
    pub async fn unfreeze(
        &self,
        user_id: Uuid,
        amount: Decimal,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction)> {
        if amount <= Decimal::ZERO {
            return Err(KeyComputeError::ValidationError(
                "Balance unfreeze amount must be greater than zero".to_string(),
            ));
        }
        let result = UserBalance::unfreeze(self.pool.as_ref(), user_id, amount, description).await;

        match result {
            Ok((balance, transaction)) => {
                tracing::info!(
                    user_id = %user_id,
                    amount = %amount,
                    available_balance = %balance.available_balance,
                    "Balance unfrozen successfully"
                );
                Ok((balance, transaction))
            }
            Err(e) if e.is_insufficient_balance() => Err(KeyComputeError::ValidationError(
                format!("Insufficient manually frozen balance for user {user_id}: {e}"),
            )),
            Err(e) if e.is_not_found() => Err(KeyComputeError::ValidationError(format!(
                "User balance not found for user {}",
                user_id
            ))),
            Err(e) => Err(KeyComputeError::DatabaseError(format!(
                "Failed to unfreeze balance: {}",
                e
            ))),
        }
    }

    /// Atomically apply or replay a durable administrator recharge,
    /// consumption, freeze, or unfreeze.
    /// Matching retries receive the original immutable result; reuse with any
    /// different normalized payload returns a conflict decision.
    #[allow(clippy::too_many_arguments)]
    pub async fn apply_admin_manual_operation(
        &self,
        kind: ManualBalanceOperationKind,
        tenant_id: Uuid,
        user_id: Uuid,
        actor_user_id: Uuid,
        amount: Decimal,
        reason: &str,
        idempotency_key: &str,
    ) -> Result<ManualBalanceOperationDecision> {
        if amount <= Decimal::ZERO {
            return Err(KeyComputeError::ValidationError(
                "Administrator balance operation amount must be greater than zero".to_string(),
            ));
        }
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(KeyComputeError::ValidationError(
                "Administrator balance operation reason must not be empty".to_string(),
            ));
        }
        if reason.chars().count() > MAX_ADMIN_BALANCE_OPERATION_REASON_CHARS {
            return Err(KeyComputeError::ValidationError(format!(
                "Administrator balance operation reason must not exceed {MAX_ADMIN_BALANCE_OPERATION_REASON_CHARS} characters"
            )));
        }
        if idempotency_key.is_empty()
            || idempotency_key.len() > MAX_ADMIN_BALANCE_IDEMPOTENCY_KEY_BYTES
            || !idempotency_key
                .bytes()
                .all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return Err(KeyComputeError::ValidationError(format!(
                "Administrator balance Idempotency-Key must contain between 1 and {MAX_ADMIN_BALANCE_IDEMPOTENCY_KEY_BYTES} visible ASCII bytes"
            )));
        }
        UserBalance::apply_admin_manual_operation(
            self.pool.as_ref(),
            kind,
            tenant_id,
            user_id,
            actor_user_id,
            amount,
            reason,
            idempotency_key,
        )
        .await
        .map_err(|error| {
            if error.is_insufficient_balance() {
                KeyComputeError::ValidationError(format!(
                    "Insufficient balance for administrator {} operation on user {user_id}: {error}",
                    kind.as_str()
                ))
            } else if error.is_not_found() {
                KeyComputeError::ValidationError(format!(
                    "User balance not found for user {user_id}"
                ))
            } else {
                KeyComputeError::DatabaseError(format!(
                    "Failed to apply administrator {} operation: {error}",
                    kind.as_str()
                ))
            }
        })
    }

    /// 查询用户交易记录
    ///
    /// # 参数
    /// - `user_id`: 用户 ID
    /// - `limit`: 返回数量限制
    /// - `offset`: 偏移量（用于分页）
    pub async fn list_transactions(
        &self,
        user_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<BalanceTransaction>> {
        BalanceTransaction::find_by_user(self.pool.as_ref(), user_id, limit, offset)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to list transactions: {}", e))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_balance_service_creation() {
        // 仅测试类型是否正确导出
        fn _assert_balance_service(_: BalanceService) {}
    }
}
