use crate::{DbError, Tenant, TenantMembership, User};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Produce AI Key 模型（用户访问系统的 API Key）
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct ProduceAiKey {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    #[serde(skip_serializing)]
    pub produce_ai_key_hash: String,
    pub produce_ai_key_preview: String,
    pub revoked: bool,
    pub revoked_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 创建 Produce AI Key 请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateProduceAiKeyRequest {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub produce_ai_key_hash: String,
    pub produce_ai_key_preview: String,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Produce AI Key 响应（不包含敏感信息）
#[derive(Debug, Clone, Serialize, FromQueryResult)]
pub struct ProduceAiKeyResponse {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub produce_ai_key_preview: String,
    pub revoked: bool,
    pub revoked_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<ProduceAiKey> for ProduceAiKeyResponse {
    fn from(key: ProduceAiKey) -> Self {
        Self {
            id: key.id,
            tenant_id: key.tenant_id,
            user_id: key.user_id,
            name: key.name,
            produce_ai_key_preview: key.produce_ai_key_preview,
            revoked: key.revoked,
            revoked_at: key.revoked_at,
            expires_at: key.expires_at,
            last_used_at: key.last_used_at,
            created_at: key.created_at,
            updated_at: key.updated_at,
        }
    }
}

impl ProduceAiKey {
    /// Pre-authentication candidate discovery only. The validator must recheck
    /// the current tenant/member/key under locks before granting authority.
    pub async fn find_by_hash(
        db: &impl ConnectionTrait,
        produce_ai_key_hash: &str,
    ) -> Result<Option<ProduceAiKey>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM produce_ai_keys WHERE produce_ai_key_hash = $1",
            [produce_ai_key_hash.into()],
        );
        let key = ProduceAiKey::find_by_statement(stmt).one(db).await?;

        Ok(key)
    }

    /// 检查密钥是否有效（未撤销且未过期）
    pub fn is_valid(&self) -> bool {
        if self.revoked {
            return false;
        }

        if let Some(expires_at) = self.expires_at
            && expires_at < Utc::now()
        {
            return false;
        }

        true
    }
}

#[path = "api_key_scope.rs"]
mod scope;
pub use scope::KeyRemoval;
