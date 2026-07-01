//! 节点会话模型
//!
//! 节点会话管理表的 ORM 模型

use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 节点会话模型
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
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
        db: &DatabaseConnection,
        req: &CreateNodeSessionRequest,
    ) -> Result<NodeSession, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO node_sessions (node_id, session_token_hash, accepted_models_json, expires_at)
            VALUES ($1, $2, $3, $4)
            RETURNING *
            "#,
            [
                req.node_id.into(),
                req.session_token_hash.as_str().into(),
                req.accepted_models_json.clone().into(),
                req.expires_at.into(),
            ],
        );
        let session = NodeSession::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(session)
    }

    /// 根据 token hash 查询会话
    pub async fn find_by_token_hash(
        db: &DatabaseConnection,
        session_token_hash: &str,
    ) -> Result<Option<NodeSession>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_sessions WHERE session_token_hash = $1",
            [session_token_hash.into()],
        );
        let session = NodeSession::find_by_statement(stmt).one(db).await?;

        Ok(session)
    }

    /// 根据 ID 查询会话
    pub async fn find_by_id(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<NodeSession>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_sessions WHERE id = $1",
            [id.into()],
        );
        let session = NodeSession::find_by_statement(stmt).one(db).await?;

        Ok(session)
    }

    /// 更新会话的最后看到时间和过期时间
    pub async fn update_seen_and_expiry(
        db: &DatabaseConnection,
        id: Uuid,
        expires_at: DateTime<Utc>,
    ) -> Result<NodeSession, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE node_sessions SET last_seen_at = NOW(), expires_at = $1 WHERE id = $2 RETURNING *",
            [expires_at.into(), id.into()],
        );
        let session = NodeSession::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("NodeSession", id.to_string()))?;

        Ok(session)
    }

    /// 更新接受的模型列表
    pub async fn update_accepted_models(
        db: &DatabaseConnection,
        id: Uuid,
        accepted_models_json: &serde_json::Value,
    ) -> Result<NodeSession, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE node_sessions SET accepted_models_json = $1, last_seen_at = NOW() WHERE id = $2 RETURNING *",
            [accepted_models_json.clone().into(), id.into()],
        );
        let session = NodeSession::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("NodeSession", id.to_string()))?;

        Ok(session)
    }

    /// 撤销会话
    pub async fn revoke(db: &DatabaseConnection, id: Uuid) -> Result<NodeSession, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE node_sessions SET revoked_at = NOW() WHERE id = $1 RETURNING *",
            [id.into()],
        );
        let session = NodeSession::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("NodeSession", id.to_string()))?;

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
