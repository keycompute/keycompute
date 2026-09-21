use super::query::escape_like_pattern;
use crate::DbError;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Explicit pricing scope. Platform rows have no tenant identifier; tenant
/// rows always identify their owning tenant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PricingScopeType {
    Platform,
    Tenant,
}
impl PricingScopeType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Tenant => "tenant",
        }
    }
}
impl std::str::FromStr for PricingScopeType {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "platform" => Ok(Self::Platform),
            "tenant" => Ok(Self::Tenant),
            other => Err(format!("unknown pricing scope: {other}")),
        }
    }
}
impl std::fmt::Display for PricingScopeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
impl sea_orm::TryGetable for PricingScopeType {
    fn try_get_by<I: sea_orm::ColIdx>(
        res: &sea_orm::QueryResult,
        idx: I,
    ) -> Result<Self, sea_orm::TryGetError> {
        let value: String = res.try_get_by(idx)?;
        value
            .parse()
            .map_err(|_| sea_orm::TryGetError::Null("invalid pricing scope".into()))
    }
}

/// 计费维度解析错误
#[derive(Debug, thiserror::Error)]
#[error("Invalid billing dimension: '{0}'. Must be 'node' or 'provideraccount'")]
pub struct BillingDimensionError(pub String);

/// 计费维度枚举
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BillingDimension {
    /// NodeDispatch 路径（/nt/v1 入口）
    #[serde(rename = "node")]
    Node,
    /// Provider Account 路径（所有非 Node 模型）
    #[serde(rename = "provideraccount")]
    ProviderAccount,
}

impl BillingDimension {
    /// 转换为字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            BillingDimension::Node => "node",
            BillingDimension::ProviderAccount => "provideraccount",
        }
    }
}

impl std::str::FromStr for BillingDimension {
    type Err = BillingDimensionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "node" => Ok(BillingDimension::Node),
            "provideraccount" => Ok(BillingDimension::ProviderAccount),
            _ => Err(BillingDimensionError(s.to_string())),
        }
    }
}

impl std::fmt::Display for BillingDimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl sea_orm::TryGetable for BillingDimension {
    fn try_get_by<I: sea_orm::ColIdx>(
        res: &sea_orm::QueryResult,
        idx: I,
    ) -> Result<Self, sea_orm::TryGetError> {
        let s: String = res.try_get_by(idx)?;
        s.parse()
            .map_err(|_: BillingDimensionError| sea_orm::TryGetError::Null("".to_string()))
    }
}

/// 定价模型
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct PricingModel {
    pub id: Uuid,
    pub scope_type: PricingScopeType,
    pub tenant_id: Option<Uuid>,
    pub model_name: String,
    pub billing_dimension: BillingDimension,
    pub currency: String,
    pub input_price_per_1k: BigDecimal,
    pub output_price_per_1k: BigDecimal,
    pub is_default: bool,
    pub version: i64,
    pub effective_from: DateTime<Utc>,
    pub effective_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
struct PricingCount {
    total: i64,
}

/// 创建定价请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingRequest {
    pub scope_type: PricingScopeType,
    pub tenant_id: Option<Uuid>,
    pub model_name: String,
    pub billing_dimension: BillingDimension,
    pub currency: Option<String>,
    pub input_price_per_1k: BigDecimal,
    pub output_price_per_1k: BigDecimal,
    pub is_default: Option<bool>,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_until: Option<DateTime<Utc>>,
}

/// 更新定价请求
#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePricingRequest {
    pub input_price_per_1k: Option<BigDecimal>,
    pub output_price_per_1k: Option<BigDecimal>,
    pub effective_until: Option<DateTime<Utc>>,
    pub expected_version: i64,
}

impl PricingModel {
    fn validate_scope(
        scope_type: PricingScopeType,
        tenant_id: Option<Uuid>,
    ) -> Result<(), DbError> {
        match (scope_type, tenant_id) {
            (PricingScopeType::Platform, None) => Ok(()),
            (PricingScopeType::Tenant, Some(id)) if !id.is_nil() => Ok(()),
            _ => Err(DbError::Other(
                "pricing scope and tenant_id do not match".into(),
            )),
        }
    }

    /// 管理端按条件分页查询定价。
    pub async fn find_all_filtered(
        db: &impl ConnectionTrait,
        scope_type: PricingScopeType,
        tenant_id: Option<Uuid>,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PricingModel>, DbError> {
        Self::validate_scope(scope_type, tenant_id)?;
        let search_pattern = search
            .filter(|value| !value.trim().is_empty())
            .map(|value| format!("%{}%", escape_like_pattern(&value.trim().to_lowercase())));
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT * FROM pricing_models
            WHERE scope_type = $1
              AND (($2::UUID IS NULL AND tenant_id IS NULL) OR ($2::UUID IS NOT NULL AND tenant_id = $2))
              AND ($3::TEXT IS NULL
               OR LOWER(model_name) LIKE $3 ESCAPE '\'
               OR LOWER(billing_dimension) LIKE $3 ESCAPE '\'
               OR LOWER(id::TEXT) LIKE $3 ESCAPE '\'
               OR LOWER(COALESCE(tenant_id::TEXT, 'platform')) LIKE $3 ESCAPE '\')
            ORDER BY model_name, tenant_id, created_at DESC, id
            LIMIT $4 OFFSET $5
            "#,
            [
                scope_type.as_str().into(),
                tenant_id.into(),
                search_pattern.clone().into(),
                limit.into(),
                offset.into(),
            ],
        );
        Ok(PricingModel::find_by_statement(stmt).all(db).await?)
    }

    /// 统计管理端筛选后的定价数量。
    pub async fn count_all_filtered(
        db: &impl ConnectionTrait,
        scope_type: PricingScopeType,
        tenant_id: Option<Uuid>,
        search: Option<&str>,
    ) -> Result<i64, DbError> {
        Self::validate_scope(scope_type, tenant_id)?;
        let search_pattern = search
            .filter(|value| !value.trim().is_empty())
            .map(|value| format!("%{}%", escape_like_pattern(&value.trim().to_lowercase())));
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT COUNT(*)::BIGINT AS total
            FROM pricing_models
            WHERE scope_type = $1
              AND (($2::UUID IS NULL AND tenant_id IS NULL) OR ($2::UUID IS NOT NULL AND tenant_id = $2))
              AND ($3::TEXT IS NULL
               OR LOWER(model_name) LIKE $3 ESCAPE '\'
               OR LOWER(billing_dimension) LIKE $3 ESCAPE '\'
               OR LOWER(id::TEXT) LIKE $3 ESCAPE '\'
               OR LOWER(COALESCE(tenant_id::TEXT, 'platform')) LIKE $3 ESCAPE '\')
            "#,
            [
                scope_type.as_str().into(),
                tenant_id.into(),
                search_pattern.into(),
            ],
        );
        let count = PricingCount::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("pricing count query returned no row".to_string()))?;
        Ok(count.total.max(0))
    }

    /// 创建新定价
    pub async fn create(
        db: &impl ConnectionTrait,
        req: &CreatePricingRequest,
    ) -> Result<PricingModel, DbError> {
        let tenant_id = match (req.scope_type, req.tenant_id) {
            (PricingScopeType::Platform, None) => None,
            (PricingScopeType::Tenant, Some(id)) if !id.is_nil() => Some(id),
            _ => {
                return Err(DbError::Other(
                    "pricing scope and tenant_id do not match".into(),
                ));
            }
        };
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO pricing_models (
                scope_type, tenant_id, model_name, billing_dimension, currency,
                input_price_per_1k, output_price_per_1k,
                is_default, effective_from, effective_until
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            RETURNING *
            "#,
            [
                req.scope_type.as_str().into(),
                tenant_id.into(),
                req.model_name.as_str().into(),
                req.billing_dimension.as_str().into(),
                req.currency.as_deref().unwrap_or("CNY").into(),
                req.input_price_per_1k.clone().into(),
                req.output_price_per_1k.clone().into(),
                req.is_default.unwrap_or(false).into(),
                req.effective_from.unwrap_or_else(Utc::now).into(),
                req.effective_until.into(),
            ],
        );
        let pricing = PricingModel::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(pricing)
    }

    /// 根据 ID 查找定价
    pub async fn find_by_id(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<PricingModel>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM pricing_models WHERE id = $1",
            [id.into()],
        );
        let pricing = PricingModel::find_by_statement(stmt).one(db).await?;

        Ok(pricing)
    }

    /// 根据 ID 查找并锁定定价，供需要保持默认标记和版本一致性的管理事务使用。
    pub async fn find_by_id_for_update(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<PricingModel>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM pricing_models WHERE id = $1 FOR UPDATE",
            [id.into()],
        );
        Ok(PricingModel::find_by_statement(stmt).one(db).await?)
    }

    /// 查找租户的所有定价
    pub async fn find_by_tenant(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
    ) -> Result<Vec<PricingModel>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT * FROM pricing_models
            WHERE (scope_type = 'tenant' AND tenant_id = $1)
               OR (scope_type = 'platform' AND tenant_id IS NULL AND is_default = TRUE)
            ORDER BY model_name, scope_type, tenant_id
            "#,
            [tenant_id.into()],
        );
        let pricing = PricingModel::find_by_statement(stmt).all(db).await?;

        Ok(pricing)
    }

    /// 查找特定模型的定价（优先租户定价，其次默认定价）
    pub async fn find_by_model(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        model_name: &str,
        billing_dimension: &str,
    ) -> Result<Option<PricingModel>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT * FROM pricing_models
            WHERE model_name = $1
              AND billing_dimension = $2
              AND effective_from <= NOW()
              AND (effective_until IS NULL OR effective_until > NOW())
              AND (
                  (scope_type = 'tenant' AND tenant_id = $3)
                  OR (scope_type = 'platform' AND tenant_id IS NULL AND is_default = TRUE)
              )
            ORDER BY 
                CASE WHEN scope_type = 'platform' THEN 1 ELSE 0 END,
                CASE WHEN is_default = TRUE THEN 0 ELSE 1 END
            LIMIT 1
            "#,
            [
                model_name.into(),
                billing_dimension.into(),
                tenant_id.into(),
            ],
        );
        let pricing = PricingModel::find_by_statement(stmt).one(db).await?;

        Ok(pricing)
    }

    /// Find default rows in an explicitly selected scope.
    pub async fn find_defaults(
        db: &impl ConnectionTrait,
        scope_type: PricingScopeType,
        tenant_id: Option<Uuid>,
    ) -> Result<Vec<PricingModel>, DbError> {
        Self::validate_scope(scope_type, tenant_id)?;
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT * FROM pricing_models
            WHERE scope_type = $1
              AND (($2::UUID IS NULL AND tenant_id IS NULL) OR ($2::UUID IS NOT NULL AND tenant_id = $2))
              AND is_default = TRUE
              AND effective_from <= NOW()
              AND (effective_until IS NULL OR effective_until > NOW())
            ORDER BY model_name
            "#,
            [scope_type.as_str().into(), tenant_id.into()],
        );
        let pricing = PricingModel::find_by_statement(stmt).all(db).await?;

        Ok(pricing)
    }

    /// 更新定价
    pub async fn update(
        &self,
        db: &impl ConnectionTrait,
        req: &UpdatePricingRequest,
    ) -> Result<PricingModel, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE pricing_models
            SET input_price_per_1k = COALESCE($1, input_price_per_1k),
                output_price_per_1k = COALESCE($2, output_price_per_1k),
                effective_until = COALESCE($3, effective_until),
                version = version + 1,
                updated_at = NOW()
            WHERE id = $4 AND version = $5
            RETURNING *
            "#,
            [
                req.input_price_per_1k.clone().into(),
                req.output_price_per_1k.clone().into(),
                req.effective_until.into(),
                self.id.into(),
                req.expected_version.into(),
            ],
        );
        let pricing = PricingModel::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::OptimisticConflict {
                entity: "pricing model".to_string(),
                id: self.id.to_string(),
            })?;

        Ok(pricing)
    }

    /// 删除定价
    pub async fn delete(&self, db: &impl ConnectionTrait) -> Result<(), DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM pricing_models WHERE id = $1",
            [self.id.into()],
        );
        db.execute(stmt).await?;

        Ok(())
    }

    /// 检查定价是否有效
    pub fn is_effective(&self) -> bool {
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

    /// 初始化系统默认定价
    ///
    /// 系统启动时调用，如果 model-empty 模型的全局默认定价不存在则创建。
    /// 平台默认定价使用 scope_type=platform 且 tenant_id=NULL。
    pub async fn init_default_pricing(db: &impl ConnectionTrait) -> Result<(), DbError> {
        // 使用字符串解析 BigDecimal
        let input_price_per_1k: BigDecimal = "0.1".parse().unwrap_or_default();
        let output_price_per_1k: BigDecimal = "0.3".parse().unwrap_or_default();

        // 单条 INSERT + ON CONFLICT 同时覆盖首次启动、重复启动和多副本并发启动。
        // 这样初始化不会因两个实例同时发现“尚不存在”而互相报唯一键冲突。
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO pricing_models (
                scope_type, tenant_id, model_name, billing_dimension, currency,
                input_price_per_1k, output_price_per_1k, is_default
            )
            VALUES ('platform', NULL, $1, $2, $3, $4, $5, $6, TRUE)
            ON CONFLICT (tenant_id, model_name, billing_dimension) DO NOTHING
            RETURNING *
            "#,
            [
                "model-empty".into(),
                BillingDimension::ProviderAccount.as_str().into(),
                "CNY".into(),
                input_price_per_1k.into(),
                output_price_per_1k.into(),
            ],
        );
        let inserted = PricingModel::find_by_statement(stmt).one(db).await?;
        if inserted.is_some() {
            tracing::info!(
                model_name = "model-empty",
                "Global default pricing created successfully"
            );
        } else {
            tracing::debug!("Default pricing for model-empty already exists, skipping init");
        }
        Ok(())
    }
}
