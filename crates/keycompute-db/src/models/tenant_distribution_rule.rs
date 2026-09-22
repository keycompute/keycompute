use crate::DbError;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeneficiaryScope {
    Everyone,
    TenantMember,
}
impl BeneficiaryScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Everyone => "everyone",
            Self::TenantMember => "tenant_member",
        }
    }
}
impl std::str::FromStr for BeneficiaryScope {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "everyone" => Ok(Self::Everyone),
            "tenant_member" => Ok(Self::TenantMember),
            other => Err(format!("unknown beneficiary scope: {other}")),
        }
    }
}
impl sea_orm::TryGetable for BeneficiaryScope {
    fn try_get_by<I: sea_orm::ColIdx>(
        res: &sea_orm::QueryResult,
        idx: I,
    ) -> Result<Self, sea_orm::TryGetError> {
        let value: String = res.try_get_by(idx)?;
        value
            .parse()
            .map_err(|_| sea_orm::TryGetError::Null("invalid beneficiary scope".into()))
    }
}

/// 租户分销规则模型
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct TenantDistributionRule {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub beneficiary_scope: BeneficiaryScope,
    pub beneficiary_id: Option<Uuid>,
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
    pub beneficiary_scope: BeneficiaryScope,
    pub beneficiary_id: Option<Uuid>,
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
    /// Tenant-wide default override priority; never denotes a platform resource.
    pub const GLOBAL_OVERRIDE_PRIORITY: i32 = 100;
    /// Internal accepted-work rule resolution. Not a console authorization path.
    pub async fn find_effective_for_settlement(
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
            ORDER BY priority DESC, created_at ASC, id ASC
            "#,
            [tenant_id.into()],
        );
        let rules = TenantDistributionRule::find_by_statement(stmt)
            .all(db)
            .await?;

        Ok(rules)
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
