//! 节点会话模型
//!
//! 节点会话管理表的 ORM 模型

use crate::DbError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// 节点会话模型
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct NodeSession {
    pub id: Uuid,
    pub node_id: Uuid,
    pub session_token_hash: String,
    pub accepted_models_json: serde_json::Value,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// 创建节点会话请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateNodeSessionRequest {
    pub node_id: Uuid,
    pub session_token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub accepted_models_json: serde_json::Value,
}

impl NodeSession {
    /// 创建新会话
    pub async fn create(
        pool: &sqlx::PgPool,
        req: &CreateNodeSessionRequest,
    ) -> Result<NodeSession, DbError> {
        let session = sqlx::query_as::<_, NodeSession>(
            r#"
            INSERT INTO node_sessions (node_id, session_token_hash, accepted_models_json, expires_at)
            VALUES ($1, $2, $3, $4)
            RETURNING *
            "#,
        )
        .bind(req.node_id)
        .bind(&req.session_token_hash)
        .bind(&req.accepted_models_json)
        .bind(req.expires_at)
        .fetch_one(pool)
        .await?;

        Ok(session)
    }

    /// 根据 token hash 查询会话
    pub async fn find_by_token_hash(
        pool: &sqlx::PgPool,
        session_token_hash: &str,
    ) -> Result<Option<NodeSession>, DbError> {
        let session = sqlx::query_as::<_, NodeSession>(
            r#"
            SELECT * FROM node_sessions
            WHERE session_token_hash = $1
            "#,
        )
        .bind(session_token_hash)
        .fetch_optional(pool)
        .await?;

        Ok(session)
    }

    /// 根据 ID 查询会话
    pub async fn find_by_id(pool: &sqlx::PgPool, id: Uuid) -> Result<Option<NodeSession>, DbError> {
        let session = sqlx::query_as::<_, NodeSession>("SELECT * FROM node_sessions WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;

        Ok(session)
    }

    /// 更新会话的最后看到时间和过期时间
    pub async fn update_seen_and_expiry(
        pool: &sqlx::PgPool,
        id: Uuid,
        expires_at: DateTime<Utc>,
    ) -> Result<NodeSession, DbError> {
        let session = sqlx::query_as::<_, NodeSession>(
            r#"
            UPDATE node_sessions
            SET last_seen_at = NOW(), expires_at = $1
            WHERE id = $2
            RETURNING *
            "#,
        )
        .bind(expires_at)
        .bind(id)
        .fetch_one(pool)
        .await?;

        Ok(session)
    }

    /// 更新接受的模型列表
    pub async fn update_accepted_models(
        pool: &sqlx::PgPool,
        id: Uuid,
        accepted_models_json: &serde_json::Value,
    ) -> Result<NodeSession, DbError> {
        let session = sqlx::query_as::<_, NodeSession>(
            r#"
            UPDATE node_sessions
            SET accepted_models_json = $1, last_seen_at = NOW()
            WHERE id = $2
            RETURNING *
            "#,
        )
        .bind(accepted_models_json)
        .bind(id)
        .fetch_one(pool)
        .await?;

        Ok(session)
    }

    /// 撤销会话
    pub async fn revoke(pool: &sqlx::PgPool, id: Uuid) -> Result<NodeSession, DbError> {
        let session = sqlx::query_as::<_, NodeSession>(
            r#"
            UPDATE node_sessions
            SET revoked_at = NOW()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_one(pool)
        .await?;

        Ok(session)
    }

    /// 检查会话是否已撤销
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// 检查会话是否已过期
    pub fn is_expired(&self) -> bool {
        self.expires_at < Utc::now()
    }

    /// 检查会话是否有效（未撤销且未过期）
    pub fn is_valid(&self) -> bool {
        !self.is_revoked() && !self.is_expired()
    }
}
