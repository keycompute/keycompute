//! 用户余额模型

use crate::DbError;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
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
}

impl TransactionType {
    pub fn as_str(&self) -> &'static str {
        match self {
            TransactionType::Recharge => "recharge",
            TransactionType::Consume => "consume",
            TransactionType::Freeze => "freeze",
            TransactionType::Unfreeze => "unfreeze",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "recharge" => Some(TransactionType::Recharge),
            "consume" => Some(TransactionType::Consume),
            "freeze" => Some(TransactionType::Freeze),
            "unfreeze" => Some(TransactionType::Unfreeze),
            _ => None,
        }
    }
}

/// 用户余额模型
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
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
    ///
    /// 使用 ON CONFLICT DO NOTHING 保证原子性，避免竞态条件
    /// 如果记录已存在，直接返回；否则创建新记录
    pub async fn get_or_create(
        pool: &sqlx::PgPool,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<UserBalance, DbError> {
        // 使用单个 upsert 查询，避免 TOCTOU 竞态条件
        let balance = sqlx::query_as::<_, UserBalance>(
            r#"
            INSERT INTO user_balances (tenant_id, user_id)
            VALUES ($1, $2)
            ON CONFLICT (user_id) DO UPDATE SET
                updated_at = NOW()
            RETURNING *
            "#,
        )
        .bind(tenant_id)
        .bind(user_id)
        .fetch_one(pool)
        .await?;

        Ok(balance)
    }

    /// 根据用户ID查找余额
    pub async fn find_by_user(
        pool: &sqlx::PgPool,
        user_id: Uuid,
    ) -> Result<Option<UserBalance>, DbError> {
        let balance =
            sqlx::query_as::<_, UserBalance>("SELECT * FROM user_balances WHERE user_id = $1")
                .bind(user_id)
                .fetch_optional(pool)
                .await?;
        Ok(balance)
    }

    /// 批量根据用户ID查找余额
    ///
    /// 返回 HashMap<user_id, UserBalance>，用于避免 N+1 查询
    pub async fn find_by_users(
        pool: &sqlx::PgPool,
        user_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, UserBalance>, DbError> {
        if user_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let balances =
            sqlx::query_as::<_, UserBalance>("SELECT * FROM user_balances WHERE user_id = ANY($1)")
                .bind(user_ids)
                .fetch_all(pool)
                .await?;

        Ok(balances.into_iter().map(|b| (b.user_id, b)).collect())
    }

    /// 充值（事务内执行）
    ///
    /// # 注意
    /// 如果用户余额记录不存在，会使用传入的 tenant_id 创建新记录
    pub async fn recharge(
        pool: &mut sqlx::PgConnection,
        user_id: Uuid,
        tenant_id: Uuid,
        amount: Decimal,
        order_id: Option<Uuid>,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction), DbError> {
        // 获取当前余额（加锁）
        let balance = sqlx::query_as::<_, UserBalance>(
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *pool)
        .await?;

        let balance_before = balance
            .as_ref()
            .map(|b| b.available_balance)
            .unwrap_or(Decimal::ZERO);
        let balance_after = balance_before + amount;

        // 使用已有记录的 tenant_id 或传入的 tenant_id
        let effective_tenant_id = balance.as_ref().map(|b| b.tenant_id).unwrap_or(tenant_id);

        // 更新或创建余额
        let updated_balance = sqlx::query_as::<_, UserBalance>(
            r#"
            INSERT INTO user_balances (user_id, tenant_id, available_balance, total_recharged)
            VALUES ($1, $2, $3, $3)
            ON CONFLICT (user_id) DO UPDATE SET
                available_balance = user_balances.available_balance + $3,
                total_recharged = user_balances.total_recharged + $3,
                updated_at = NOW()
            RETURNING *
            "#,
        )
        .bind(user_id)
        .bind(effective_tenant_id)
        .bind(amount)
        .fetch_one(&mut *pool)
        .await?;

        // 记录交易
        let transaction = BalanceTransaction::create_internal(
            &mut *pool,
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
    ///
    /// # Errors
    /// - `DbError::NotFound` - 用户余额记录不存在
    /// - `DbError::InsufficientBalance` - 余额不足
    pub async fn consume(
        pool: &mut sqlx::PgConnection,
        user_id: Uuid,
        amount: Decimal,
        usage_log_id: Option<Uuid>,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction), DbError> {
        // 获取当前余额（加锁）
        let balance = sqlx::query_as::<_, UserBalance>(
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *pool)
        .await?;

        // 检查余额是否存在
        let balance = match balance {
            Some(b) => b,
            None => return Err(DbError::not_found("UserBalance", user_id.to_string())),
        };

        // 检查余额是否足够
        if balance.available_balance < amount {
            return Err(DbError::insufficient_balance(
                amount.to_string(),
                balance.available_balance.to_string(),
            ));
        }

        let balance_before = balance.available_balance;
        let balance_after = balance_before - amount;

        // 更新余额
        let updated_balance = sqlx::query_as::<_, UserBalance>(
            r#"
            UPDATE user_balances
            SET available_balance = available_balance - $1,
                total_consumed = total_consumed + $1,
                updated_at = NOW()
            WHERE user_id = $2
            RETURNING *
            "#,
        )
        .bind(amount)
        .bind(user_id)
        .fetch_one(&mut *pool)
        .await?;

        // 记录交易
        let transaction = BalanceTransaction::create_internal(
            &mut *pool,
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
    ///
    /// # Errors
    /// - `DbError::NotFound` - 用户余额记录不存在
    /// - `DbError::InsufficientBalance` - 可用余额不足
    pub async fn freeze(
        pool: &mut sqlx::PgConnection,
        user_id: Uuid,
        amount: Decimal,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction), DbError> {
        let balance = sqlx::query_as::<_, UserBalance>(
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *pool)
        .await?;

        // 检查余额是否存在
        let balance = match balance {
            Some(b) => b,
            None => return Err(DbError::not_found("UserBalance", user_id.to_string())),
        };

        // 检查可用余额是否足够
        if balance.available_balance < amount {
            return Err(DbError::insufficient_balance(
                amount.to_string(),
                balance.available_balance.to_string(),
            ));
        }

        let balance_before = balance.available_balance;
        let balance_after = balance_before - amount;

        let updated_balance = sqlx::query_as::<_, UserBalance>(
            r#"
            UPDATE user_balances
            SET available_balance = available_balance - $1,
                frozen_balance = frozen_balance + $1,
                updated_at = NOW()
            WHERE user_id = $2
            RETURNING *
            "#,
        )
        .bind(amount)
        .bind(user_id)
        .fetch_one(&mut *pool)
        .await?;

        let transaction = BalanceTransaction::create_internal(
            &mut *pool,
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

    /// 解冻余额
    ///
    /// 将冻结余额转回可用余额
    ///
    /// # Errors
    /// - `DbError::NotFound` - 用户余额记录不存在
    /// - `DbError::InsufficientBalance` - 冻结余额不足
    pub async fn unfreeze(
        pool: &mut sqlx::PgConnection,
        user_id: Uuid,
        amount: Decimal,
        description: Option<&str>,
    ) -> Result<(UserBalance, BalanceTransaction), DbError> {
        let balance = sqlx::query_as::<_, UserBalance>(
            "SELECT * FROM user_balances WHERE user_id = $1 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *pool)
        .await?;

        // 检查余额是否存在
        let balance = match balance {
            Some(b) => b,
            None => return Err(DbError::not_found("UserBalance", user_id.to_string())),
        };

        // 检查冻结余额是否足够
        if balance.frozen_balance < amount {
            return Err(DbError::insufficient_balance(
                amount.to_string(),
                balance.frozen_balance.to_string(),
            ));
        }

        let balance_before = balance.available_balance;
        let balance_after = balance_before + amount;

        let updated_balance = sqlx::query_as::<_, UserBalance>(
            r#"
            UPDATE user_balances
            SET available_balance = available_balance + $1,
                frozen_balance = frozen_balance - $1,
                updated_at = NOW()
            WHERE user_id = $2
            RETURNING *
            "#,
        )
        .bind(amount)
        .bind(user_id)
        .fetch_one(&mut *pool)
        .await?;

        let transaction = BalanceTransaction::create_internal(
            &mut *pool,
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
}

/// 余额变动记录模型
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
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
    /// 内部创建交易记录
    #[allow(clippy::too_many_arguments)]
    async fn create_internal(
        pool: &mut sqlx::PgConnection,
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
        let transaction = sqlx::query_as::<_, BalanceTransaction>(
            r#"
            INSERT INTO balance_transactions (
                tenant_id, user_id, order_id, usage_log_id,
                transaction_type, amount, balance_before, balance_after, description
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING *
            "#,
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(order_id)
        .bind(usage_log_id)
        .bind(transaction_type.as_str())
        .bind(amount)
        .bind(balance_before)
        .bind(balance_after)
        .bind(description)
        .fetch_one(&mut *pool)
        .await?;

        Ok(transaction)
    }

    /// 查找用户的交易记录
    pub async fn find_by_user(
        pool: &sqlx::PgPool,
        user_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<BalanceTransaction>, DbError> {
        let transactions = sqlx::query_as::<_, BalanceTransaction>(
            r#"
            SELECT * FROM balance_transactions
            WHERE user_id = $1
            ORDER BY created_at DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(user_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
        Ok(transactions)
    }

    /// 获取交易类型枚举
    pub fn get_transaction_type(&self) -> Option<TransactionType> {
        TransactionType::parse(&self.transaction_type)
    }
}
