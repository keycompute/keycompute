use crate::DbError;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 租户分销规则模型
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct TenantDistributionRule {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub beneficiary_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub commission_rate: BigDecimal,
    pub priority: i32,
    pub is_active: bool,
    pub effective_from: DateTime<Utc>,
    pub effective_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 创建分销规则请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateDistributionRuleRequest {
    pub tenant_id: Uuid,
    pub beneficiary_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub commission_rate: BigDecimal,
    pub priority: Option<i32>,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_until: Option<DateTime<Utc>>,
}

/// 更新分销规则请求
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateDistributionRuleRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub commission_rate: Option<BigDecimal>,
    pub priority: Option<i32>,
    pub is_active: Option<bool>,
    pub effective_until: Option<DateTime<Utc>>,
}

impl TenantDistributionRule {
    /// 创建新分销规则
    pub async fn create(
        db: &impl ConnectionTrait,
        req: &CreateDistributionRuleRequest,
    ) -> Result<TenantDistributionRule, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO tenant_distribution_rules (
                tenant_id, beneficiary_id, name, description, commission_rate,
                priority, effective_from, effective_until
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING *
            "#,
            [
                req.tenant_id.into(),
                req.beneficiary_id.into(),
                req.name.as_str().into(),
                req.description.clone().into(),
                req.commission_rate.clone().into(),
                req.priority.unwrap_or(0).into(),
                req.effective_from.unwrap_or_else(Utc::now).into(),
                req.effective_until.into(),
            ],
        );
        let rule = TenantDistributionRule::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(rule)
    }

    /// 根据 ID 查找规则
    pub async fn find_by_id(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<TenantDistributionRule>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM tenant_distribution_rules WHERE id = $1",
            [id.into()],
        );
        let rule = TenantDistributionRule::find_by_statement(stmt)
            .one(db)
            .await?;

        Ok(rule)
    }

    /// 查找租户的所有有效规则
    pub async fn find_by_tenant(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
    ) -> Result<Vec<TenantDistributionRule>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT * FROM tenant_distribution_rules
            WHERE tenant_id = $1
              AND is_active = TRUE
              AND effective_from <= NOW()
              AND (effective_until IS NULL OR effective_until > NOW())
            ORDER BY priority DESC, created_at ASC
            "#,
            [tenant_id.into()],
        );
        let rules = TenantDistributionRule::find_by_statement(stmt)
            .all(db)
            .await?;

        Ok(rules)
    }

    /// 查找租户的所有规则（包括已禁用）
    pub async fn find_all_by_tenant(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
    ) -> Result<Vec<TenantDistributionRule>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM tenant_distribution_rules WHERE tenant_id = $1 ORDER BY priority DESC",
            [tenant_id.into()],
        );
        let rules = TenantDistributionRule::find_by_statement(stmt)
            .all(db)
            .await?;

        Ok(rules)
    }

    /// 更新规则
    pub async fn update(
        &self,
        db: &impl ConnectionTrait,
        req: &UpdateDistributionRuleRequest,
    ) -> Result<TenantDistributionRule, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE tenant_distribution_rules
            SET name = COALESCE($1, name),
                description = COALESCE($2, description),
                commission_rate = COALESCE($3, commission_rate),
                priority = COALESCE($4, priority),
                is_active = COALESCE($5, is_active),
                effective_until = COALESCE($6, effective_until),
                updated_at = NOW()
            WHERE id = $7
            RETURNING *
            "#,
            [
                req.name.clone().into(),
                req.description.clone().into(),
                req.commission_rate.clone().into(),
                req.priority.into(),
                req.is_active.into(),
                req.effective_until.into(),
                self.id.into(),
            ],
        );
        let rule = TenantDistributionRule::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("update failed to return row".to_string()))?;

        Ok(rule)
    }

    /// 删除规则
    pub async fn delete(&self, db: &impl ConnectionTrait) -> Result<(), DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM tenant_distribution_rules WHERE id = $1",
            [self.id.into()],
        );
        db.execute(stmt).await?;

        Ok(())
    }

    /// 检查规则是否有效
    pub fn is_effective(&self) -> bool {
        if !self.is_active {
            return false;
        }

        let now = Utc::now();

        if self.effective_from > now {
            return false;
        }

        if let Some(effective_until) = self.effective_until
            && effective_until <= now
        {
            return false;
        }

        true
    }
}
