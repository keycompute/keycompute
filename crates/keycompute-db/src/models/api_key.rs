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
        db: &(impl ConnectionTrait + TransactionTrait),
        req: &CreateProduceAiKeyRequest,
    ) -> Result<ProduceAiKey, DbError> {
        // Lock the tenant parent before its user/key children. Besides
        // preventing a key insert from racing tenant deletion, this keeps
        // direct model callers on the same parent-first order as lifecycle
        // operations. The user lock below still serializes reassignment and
        // lets us re-check the authoritative tenant before inserting.
        let tx = db.begin().await?;
        Tenant::find_by_id_for_key_share(&tx, req.tenant_id)
            .await?
            .ok_or_else(|| DbError::not_found("tenant", req.tenant_id))?;
        User::find_by_id_for_update(&tx, req.user_id)
            .await?
            .ok_or_else(|| DbError::not_found("user", req.user_id))?;
        TenantMembership::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT m.* FROM tenant_memberships m JOIN tenants t ON t.id=m.tenant_id JOIN users u ON u.id=m.user_id WHERE m.tenant_id=$1 AND m.user_id=$2 AND m.status='active' AND t.status='active' AND u.status='active' FOR SHARE OF m",
            [req.tenant_id.into(),req.user_id.into()],
        )).one(&tx).await?
            .ok_or_else(||DbError::not_found("active membership",format!("{}/{}",req.tenant_id,req.user_id)))?;

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
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        tx.commit().await?;

        Ok(key)
    }

    /// Personal lists never widen for a tenant administrator.
    pub async fn list_owned(
        db: &impl ConnectionTrait,
        scope: keycompute_types::TenantScope,
        include_revoked: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT * FROM produce_ai_keys WHERE tenant_id=$1 AND user_id=$2 AND ($3 OR NOT revoked) ORDER BY created_at DESC,id DESC LIMIT $4 OFFSET $5",
            [scope.tenant_id().into(),scope.user_id().into(),include_revoked.into(),limit.clamp(1,1000).into(),offset.max(0).into()],
        )).all(db).await?)
    }
    pub async fn count_owned(
        db: &impl ConnectionTrait,
        scope: keycompute_types::TenantScope,
        include_revoked: bool,
    ) -> Result<i64, DbError> {
        let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT COUNT(*) FROM produce_ai_keys WHERE tenant_id=$1 AND user_id=$2 AND ($3 OR NOT revoked)",
            [scope.tenant_id().into(),scope.user_id().into(),include_revoked.into()],
        )).await?.ok_or_else(||DbError::Other("key count returned no row".into()))?;
        Ok(row.try_get_by_index(0)?)
    }
    pub async fn find_owned(
        db: &impl ConnectionTrait,
        scope: keycompute_types::TenantScope,
        id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM produce_ai_keys WHERE tenant_id=$1 AND user_id=$2 AND id=$3",
            [scope.tenant_id().into(), scope.user_id().into(), id.into()],
        ))
        .one(db)
        .await?)
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

    /// 根据 ID 查找并锁定 Produce AI Key。
    ///
    /// API key authentication acquires the owning user lock before this key
    /// lock.  Keeping the lock-capable lookup in the model makes that order
    /// explicit and prevents a concurrent tenant move (which revokes keys
    /// after locking the user) from returning a snapshot that was revoked
    /// before authentication completed.
    pub async fn find_by_id_for_update(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<ProduceAiKey>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM produce_ai_keys WHERE id = $1 FOR UPDATE",
            [id.into()],
        );
        Ok(ProduceAiKey::find_by_statement(stmt).one(db).await?)
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
            "SELECT * FROM produce_ai_keys WHERE user_id = $1 ORDER BY created_at DESC, id DESC",
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
            "SELECT * FROM produce_ai_keys WHERE user_id = $1 AND revoked = FALSE ORDER BY created_at DESC, id DESC",
            [user_id.into()],
        );
        let keys = ProduceAiKey::find_by_statement(stmt).all(db).await?;

        Ok(keys)
    }

    /// 分页查找用户的 Produce AI Key。
    pub async fn find_by_user_page(
        db: &impl ConnectionTrait,
        user_id: Uuid,
        include_revoked: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ProduceAiKey>, DbError> {
        let condition = if include_revoked {
            ""
        } else {
            " AND revoked = FALSE"
        };
        let sql = format!(
            "SELECT * FROM produce_ai_keys WHERE user_id = $1{condition} ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3"
        );
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            &sql,
            [user_id.into(), limit.into(), offset.into()],
        );
        Ok(ProduceAiKey::find_by_statement(stmt).all(db).await?)
    }

    /// 获取用户 API Key 总数。
    pub async fn count_by_user(
        db: &impl ConnectionTrait,
        user_id: Uuid,
        include_revoked: bool,
    ) -> Result<i64, DbError> {
        let condition = if include_revoked {
            ""
        } else {
            " AND revoked = FALSE"
        };
        let sql = format!("SELECT COUNT(*) FROM produce_ai_keys WHERE user_id = $1{condition}");
        let stmt = Statement::from_sql_and_values(DbBackend::Postgres, &sql, [user_id.into()]);
        let result = db
            .query_one(stmt)
            .await?
            .ok_or_else(|| DbError::Other("count query failed".to_string()))?;
        result.try_get_by_index(0).map_err(DbError::DatabaseError)
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

    /// 租户迁移时撤销用户现有 API Key，避免旧租户凭据继续被展示或使用。
    pub async fn revoke_all_for_user(
        db: &impl ConnectionTrait,
        user_id: Uuid,
    ) -> Result<u64, DbError> {
        let result = db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE produce_ai_keys SET revoked = TRUE, revoked_at = COALESCE(revoked_at, NOW()), updated_at = NOW() WHERE user_id = $1 AND revoked = FALSE",
                [user_id.into()],
            ))
            .await?;
        Ok(result.rows_affected())
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
