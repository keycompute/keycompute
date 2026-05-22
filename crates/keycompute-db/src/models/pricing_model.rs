use crate::DbError;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// 全局默认定价的租户 ID（nil UUID）
pub const GLOBAL_DEFAULT_TENANT_ID: Uuid = Uuid::nil();

/// 计费维度解析错误
#[derive(Debug, thiserror::Error)]
#[error("Invalid billing dimension: '{0}'. Must be 'node' or 'provideraccount'")]
pub struct BillingDimensionError(pub String);

/// 计费维度枚举
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BillingDimension {
    /// Node 路径（node: 前缀的模型）
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

impl sqlx::Type<sqlx::Postgres> for BillingDimension {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name("varchar")
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for BillingDimension {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <String as sqlx::Encode<sqlx::Postgres>>::encode(self.as_str().to_string(), buf)
    }
}

impl sqlx::Decode<'_, sqlx::Postgres> for BillingDimension {
    fn decode(
        value: sqlx::postgres::PgValueRef<'_>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        s.parse().map_err(|e: BillingDimensionError| {
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
                as Box<dyn std::error::Error + Send + Sync>
        })
    }
}

/// 定价模型
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PricingModel {
    pub id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub model_name: String,
    pub billing_dimension: BillingDimension,
    pub currency: String,
    pub input_price_per_1k: BigDecimal,
    pub output_price_per_1k: BigDecimal,
    pub is_default: bool,
    pub effective_from: DateTime<Utc>,
    pub effective_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 创建定价请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingRequest {
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
}

impl PricingModel {
    /// 创建新定价
    pub async fn create(
        pool: &sqlx::PgPool,
        req: &CreatePricingRequest,
    ) -> Result<PricingModel, DbError> {
        let pricing = sqlx::query_as::<_, PricingModel>(
            r#"
            INSERT INTO pricing_models (
                tenant_id, model_name, billing_dimension, currency,
                input_price_per_1k, output_price_per_1k,
                is_default, effective_from, effective_until
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING *
            "#,
        )
        .bind(req.tenant_id)
        .bind(&req.model_name)
        .bind(&req.billing_dimension)
        .bind(req.currency.as_deref().unwrap_or("CNY"))
        .bind(&req.input_price_per_1k)
        .bind(&req.output_price_per_1k)
        .bind(req.is_default.unwrap_or(false))
        .bind(req.effective_from.unwrap_or_else(Utc::now))
        .bind(req.effective_until)
        .fetch_one(pool)
        .await?;

        Ok(pricing)
    }

    /// 根据 ID 查找定价
    pub async fn find_by_id(
        pool: &sqlx::PgPool,
        id: Uuid,
    ) -> Result<Option<PricingModel>, DbError> {
        let pricing =
            sqlx::query_as::<_, PricingModel>("SELECT * FROM pricing_models WHERE id = $1")
                .bind(id)
                .fetch_optional(pool)
                .await?;

        Ok(pricing)
    }

    /// 查找租户的所有定价
    pub async fn find_by_tenant(
        pool: &sqlx::PgPool,
        tenant_id: Uuid,
    ) -> Result<Vec<PricingModel>, DbError> {
        let pricing = sqlx::query_as::<_, PricingModel>(
            r#"
            SELECT * FROM pricing_models
            WHERE tenant_id = $1
               OR (tenant_id IS NULL AND is_default = TRUE)
            ORDER BY model_name, tenant_id NULLS LAST
            "#,
        )
        .bind(tenant_id)
        .fetch_all(pool)
        .await?;

        Ok(pricing)
    }

    /// 查找特定模型的定价（优先租户定价，其次默认定价）
    pub async fn find_by_model(
        pool: &sqlx::PgPool,
        tenant_id: Uuid,
        model_name: &str,
        billing_dimension: &str,
    ) -> Result<Option<PricingModel>, DbError> {
        let pricing = sqlx::query_as::<_, PricingModel>(
            r#"
            SELECT * FROM pricing_models
            WHERE model_name = $1
              AND billing_dimension = $2
              AND effective_from <= NOW()
              AND (effective_until IS NULL OR effective_until > NOW())
              AND (
                  tenant_id = $3
                  OR (tenant_id = $4 AND is_default = TRUE)
              )
            ORDER BY 
                CASE WHEN tenant_id = $4 THEN 1 ELSE 0 END,
                CASE WHEN is_default = TRUE THEN 0 ELSE 1 END
            LIMIT 1
            "#,
        )
        .bind(model_name)
        .bind(billing_dimension)
        .bind(tenant_id)
        .bind(GLOBAL_DEFAULT_TENANT_ID)
        .fetch_optional(pool)
        .await?;

        Ok(pricing)
    }

    /// 查找所有默认定价
    pub async fn find_defaults(pool: &sqlx::PgPool) -> Result<Vec<PricingModel>, DbError> {
        let pricing = sqlx::query_as::<_, PricingModel>(
            r#"
            SELECT * FROM pricing_models
            WHERE is_default = TRUE
              AND effective_from <= NOW()
              AND (effective_until IS NULL OR effective_until > NOW())
            ORDER BY model_name
            "#,
        )
        .fetch_all(pool)
        .await?;

        Ok(pricing)
    }

    /// 更新定价
    pub async fn update(
        &self,
        pool: &sqlx::PgPool,
        req: &UpdatePricingRequest,
    ) -> Result<PricingModel, DbError> {
        let pricing = sqlx::query_as::<_, PricingModel>(
            r#"
            UPDATE pricing_models
            SET input_price_per_1k = COALESCE($1, input_price_per_1k),
                output_price_per_1k = COALESCE($2, output_price_per_1k),
                effective_until = COALESCE($3, effective_until),
                updated_at = NOW()
            WHERE id = $4
            RETURNING *
            "#,
        )
        .bind(&req.input_price_per_1k)
        .bind(&req.output_price_per_1k)
        .bind(req.effective_until)
        .bind(self.id)
        .fetch_one(pool)
        .await?;

        Ok(pricing)
    }

    /// 删除定价
    pub async fn delete(&self, pool: &sqlx::PgPool) -> Result<(), DbError> {
        sqlx::query("DELETE FROM pricing_models WHERE id = $1")
            .bind(self.id)
            .execute(pool)
            .await?;

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
    /// 全局默认定价使用 tenant_id = NULL，表示全局级别。
    pub async fn init_default_pricing(pool: &sqlx::PgPool) -> Result<(), DbError> {
        // 查询全局默认定价是否已存在（tenant_id = GLOBAL_DEFAULT_TENANT_ID）
        let existing = sqlx::query_as::<_, PricingModel>(
            r#"
            SELECT * FROM pricing_models
            WHERE model_name = $1
              AND billing_dimension = $2
              AND tenant_id = $3
            "#,
        )
        .bind("model-empty")
        .bind(BillingDimension::ProviderAccount.as_str())
        .bind(GLOBAL_DEFAULT_TENANT_ID)
        .fetch_optional(pool)
        .await?;

        if existing.is_some() {
            tracing::debug!("Default pricing for model-empty already exists, skipping init");
            return Ok(());
        }

        tracing::info!(
            model_name = "model-empty",
            "Creating global default pricing"
        );

        // 使用字符串解析 BigDecimal
        let input_price_per_1k = "0.1".parse().unwrap_or_default();
        let output_price_per_1k = "0.3".parse().unwrap_or_default();

        // 创建 model-empty 模型的全局默认定价（tenant_id = GLOBAL_DEFAULT_TENANT_ID）
        let db_req = CreatePricingRequest {
            tenant_id: Some(GLOBAL_DEFAULT_TENANT_ID), // 全局默认：nil UUID
            model_name: "model-empty".to_string(),
            billing_dimension: BillingDimension::ProviderAccount,
            currency: Some("CNY".to_string()),
            input_price_per_1k,
            output_price_per_1k,
            is_default: Some(true),
            effective_from: None,
            effective_until: None,
        };

        Self::create(pool, &db_req).await?;
        tracing::info!(
            model_name = "model-empty",
            "Global default pricing created successfully"
        );
        Ok(())
    }
}
