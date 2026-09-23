//! 节点会话模型
//!
//! 节点会话管理表的 ORM 模型

use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use uuid::Uuid;

/// 节点会话模型
#[derive(Clone, FromQueryResult)]
pub struct NodeSession {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub owner_user_id: Uuid,
    pub node_id: Uuid,
    pub session_token_hash: String,
    pub accepted_models_json: serde_json::Value,
    pub registered_models_json: serde_json::Value,
    pub native_operations_json: serde_json::Value,
    pub native_profiles_json: serde_json::Value,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub accepting_tasks: bool,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Resource identity derived from a verified node/session. This is NOT a
/// console permission; registration and completion enforce their own grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeSessionScope {
    tenant_id: Uuid,
    node_id: Uuid,
    owner_user_id: Uuid,
}
impl NodeSessionScope {
    pub fn for_node(node: &super::node::Node) -> Self {
        Self {
            tenant_id: node.tenant_id,
            node_id: node.id,
            owner_user_id: node.owner_user_id,
        }
    }
    pub fn checked(tenant_id: Uuid, node_id: Uuid, owner_user_id: Uuid) -> Result<Self, DbError> {
        if tenant_id.is_nil() || node_id.is_nil() || owner_user_id.is_nil() {
            return Err(DbError::Other(
                "node session scope requires real identities".into(),
            ));
        }
        Ok(Self {
            tenant_id,
            node_id,
            owner_user_id,
        })
    }
    fn values(self) -> [sea_orm::Value; 3] {
        [
            self.tenant_id.into(),
            self.node_id.into(),
            self.owner_user_id.into(),
        ]
    }
}

/// 创建节点会话请求
#[derive(Clone)]
pub struct CreateNodeSessionRequest {
    pub scope: NodeSessionScope,
    pub session_token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub accepted_models_json: serde_json::Value,
    pub native_operations_json: serde_json::Value,
    pub native_profiles_json: serde_json::Value,
}

impl NodeSession {
    /// 创建新会话
    pub async fn create(
        db: &impl ConnectionTrait,
        req: &CreateNodeSessionRequest,
    ) -> Result<NodeSession, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO node_sessions (tenant_id,node_id,owner_user_id,session_token_hash,
                accepted_models_json,registered_models_json,native_operations_json,native_profiles_json,expires_at)
            SELECT n.tenant_id,n.id,n.owner_user_id,$4,$5,$5,$6,$7,$8
            FROM nodes n WHERE n.tenant_id=$1 AND n.id=$2 AND n.owner_user_id=$3
            RETURNING *
            "#,
            req.scope.values().into_iter().chain([
                req.session_token_hash.as_str().into(),
                req.accepted_models_json.clone().into(),
                req.native_operations_json.clone().into(),
                req.native_profiles_json.clone().into(),
                req.expires_at.into(),
            ]),
        );
        let session = NodeSession::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(session)
    }

    /// 根据 token hash 查询会话
    pub async fn find_credential_candidate(
        db: &impl ConnectionTrait,
        session_token_hash: &str,
    ) -> Result<Option<NodeSession>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT s.* FROM node_sessions s JOIN nodes n ON n.id=s.node_id AND n.tenant_id=s.tenant_id AND n.owner_user_id=s.owner_user_id WHERE s.session_token_hash=$1",
            [session_token_hash.into()],
        );
        let session = NodeSession::find_by_statement(stmt).one(db).await?;

        Ok(session)
    }

    /// 根据 ID 查询会话
    pub async fn find_in_scope(
        db: &impl ConnectionTrait,
        scope: NodeSessionScope,
        id: Uuid,
    ) -> Result<Option<NodeSession>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_sessions WHERE tenant_id=$1 AND node_id=$2 AND owner_user_id=$3 AND id=$4",
            scope.values().into_iter().chain([id.into()]),
        );
        let session = NodeSession::find_by_statement(stmt).one(db).await?;

        Ok(session)
    }

    /// Revoke only the exact stored tenant/node/owner session. Runtime renewals
    /// live in the gateway transaction, not unrestricted public row helpers.
    pub async fn revoke_in_scope(
        db: &impl ConnectionTrait,
        scope: NodeSessionScope,
        id: Uuid,
    ) -> Result<NodeSession, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE node_sessions SET revoked_at=COALESCE(revoked_at,clock_timestamp()) WHERE tenant_id=$1 AND node_id=$2 AND owner_user_id=$3 AND id=$4 RETURNING *",
            scope.values().into_iter().chain([id.into()]),
        );
        Self::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("NodeSession", id.to_string()))
    }
    pub fn scope(&self) -> NodeSessionScope {
        NodeSessionScope {
            tenant_id: self.tenant_id,
            node_id: self.node_id,
            owner_user_id: self.owner_user_id,
        }
    }

    /// 检查会话是否已撤销
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// 检查会话是否已过期
    pub fn is_expired(&self) -> bool {
        self.expires_at <= Utc::now()
    }

    /// 检查会话是否有效（未撤销且未过期）
    pub fn is_valid(&self) -> bool {
        !self.is_revoked() && !self.is_expired()
    }
}

impl std::fmt::Debug for NodeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeSession")
            .field("id", &self.id)
            .field("scope", &self.scope())
            .field("expires_at", &self.expires_at)
            .field("accepting_tasks", &self.accepting_tasks)
            .field("session_token_hash", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for CreateNodeSessionRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateNodeSessionRequest")
            .field("scope", &self.scope)
            .field("session_token_hash", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}
