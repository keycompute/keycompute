//! 余额服务
//!
//! 封装用户余额操作的业务逻辑，提供统一的事务管理
//!
//! ## 架构定位
//! - 业务层：负责余额相关业务规则
//! - 数据层（keycompute-db）：负责数据库持久化

use keycompute_db::{BalanceTransaction, DbRouter, UserBalance};
use keycompute_types::{KeyComputeError, Result};
use rust_decimal::Decimal;
use std::sync::Arc;
use uuid::Uuid;

/// 余额不足阈值（元）
/// 当用户余额低于此值时，拒绝请求
pub fn min_balance_threshold() -> Decimal {
    Decimal::from_f64_retain(0.1).unwrap_or(Decimal::ONE / Decimal::from(10))
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
    /// 返回 `None` 表示用户没有余额记录
    pub async fn find_by_user(&self, user_id: Uuid) -> Result<Option<UserBalance>> {
        UserBalance::find_by_user(self.pool.as_ref(), user_id)
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
        UserBalance::find_by_users(self.pool.as_ref(), user_ids)
            .await
            .map_err(|e| KeyComputeError::DatabaseError(format!("Failed to find balances: {}", e)))
    }

    /// 检查用户余额是否足够
    ///
    /// 如果用户余额低于阈值（0.1元），返回错误
    /// 用于在请求处理前进行预检查，避免执行后才发现余额不足
    ///
    /// # 返回
    /// - `Ok(balance)`: 余额足够，返回当前余额
    /// - `Err(ValidationError)`: 余额不足或用户不存在
    pub async fn check_balance(&self, user_id: Uuid) -> Result<UserBalance> {
        let balance = self.find_by_user(user_id).await?;

        match balance {
            Some(b) => {
                if b.available_balance < min_balance_threshold() {
                    Err(KeyComputeError::ValidationError(format!(
                        "Insufficient balance: current balance {:.4} is below minimum threshold {:.4}",
                        b.available_balance,
                        min_balance_threshold()
                    )))
                } else {
                    Ok(b)
                }
            }
            None => {
                // 用户没有余额记录，视为余额为 0
                Err(KeyComputeError::ValidationError(
                    "Insufficient balance: no balance record found".to_string(),
                ))
            }
        }
    }

    /// 检查用户余额是否足够（带租户验证）
    ///
    /// 额外验证余额记录属于指定租户
    pub async fn check_balance_for_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<UserBalance> {
        let balance = self.check_balance(user_id).await?;

        if balance.tenant_id != tenant_id {
            return Err(KeyComputeError::ValidationError(
                "Balance record tenant mismatch".to_string(),
            ));
        }

        Ok(balance)
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
            Err(e) if e.is_insufficient_balance() => {
                Err(KeyComputeError::ValidationError(format!(
                    "Insufficient balance for user {}: required {}",
                    user_id, amount
                )))
            }
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
            Err(e) if e.is_insufficient_balance() => {
                Err(KeyComputeError::ValidationError(format!(
                    "Insufficient available balance for user {}: required {}",
                    user_id, amount
                )))
            }
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
            Err(e) if e.is_insufficient_balance() => {
                Err(KeyComputeError::ValidationError(format!(
                    "Insufficient frozen balance for user {}: required {}",
                    user_id, amount
                )))
            }
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
