use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Produce AI Key 模型（用户访问系统的 API Key）
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct ProduceAiKey {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub name: String,
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
#[derive(Debug, Clone, Serialize)]
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
        }
    }
}

impl ProduceAiKey {
    /// 创建新 Produce AI Key
    pub async fn create(
        db: &impl ConnectionTrait,
        req: &CreateProduceAiKeyRequest,
    ) -> Result<ProduceAiKey, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO produce_ai_keys (tenant_id, user_id, name, produce_ai_key_hash, produce_ai_key_preview, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING *
            "#,
            [
                req.tenant_id.into(),
                req.user_id.into(),
                req.name.as_str().into(),
                req.produce_ai_key_hash.as_str().into(),
                req.produce_ai_key_preview.as_str().into(),
                req.expires_at.into(),
            ],
        );
        let key = ProduceAiKey::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(key)
    }

    /// 根据 ID 查找 Produce AI Key
    pub async fn find_by_id(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<ProduceAiKey>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM produce_ai_keys WHERE id = $1",
            [id.into()],
        );
        let key = ProduceAiKey::find_by_statement(stmt).one(db).await?;

        Ok(key)
    }

    /// 根据 produce_ai_key_hash 查找 Produce AI Key
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

    /// 查找用户的所有 Produce AI Key
    pub async fn find_by_user(
        db: &impl ConnectionTrait,
        user_id: Uuid,
    ) -> Result<Vec<ProduceAiKey>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM produce_ai_keys WHERE user_id = $1 ORDER BY created_at DESC",
            [user_id.into()],
        );
        let keys = ProduceAiKey::find_by_statement(stmt).all(db).await?;

        Ok(keys)
    }

    /// 查找用户的活跃 Produce AI Key（未撤销的）
    pub async fn find_active_by_user(
        db: &impl ConnectionTrait,
        user_id: Uuid,
    ) -> Result<Vec<ProduceAiKey>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM produce_ai_keys WHERE user_id = $1 AND revoked = FALSE ORDER BY created_at DESC",
            [user_id.into()],
        );
        let keys = ProduceAiKey::find_by_statement(stmt).all(db).await?;

        Ok(keys)
    }

    /// 查找租户的所有 Produce AI Key
    pub async fn find_by_tenant(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
    ) -> Result<Vec<ProduceAiKey>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM produce_ai_keys WHERE tenant_id = $1 ORDER BY created_at DESC",
            [tenant_id.into()],
        );
        let keys = ProduceAiKey::find_by_statement(stmt).all(db).await?;

        Ok(keys)
    }

    /// 撤销 Produce AI Key
    pub async fn revoke(&self, db: &impl ConnectionTrait) -> Result<ProduceAiKey, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE produce_ai_keys
            SET revoked = TRUE,
                revoked_at = NOW(),
                updated_at = NOW()
            WHERE id = $1
            RETURNING *
            "#,
            [self.id.into()],
        );
        let key = ProduceAiKey::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("ProduceAiKey", self.id.to_string()))?;

        Ok(key)
    }

    /// 物理删除 Produce AI Key
    pub async fn delete(&self, db: &impl ConnectionTrait) -> Result<(), DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM produce_ai_keys WHERE id = $1",
            [self.id.into()],
        );
        db.execute(stmt).await?;

        Ok(())
    }

    /// 更新最后使用时间
    pub async fn update_last_used(&self, db: &impl ConnectionTrait) -> Result<(), DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE produce_ai_keys SET last_used_at = NOW(), updated_at = NOW() WHERE id = $1",
            [self.id.into()],
        );
        db.execute(stmt).await?;

        Ok(())
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
