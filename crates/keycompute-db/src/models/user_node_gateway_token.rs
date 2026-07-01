//! 用户节点网关注册令牌模型

use crate::DbError;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

/// Token 状态常量
pub const TOKEN_STATUS_PENDING: &str = "pending";
pub const TOKEN_STATUS_APPROVED: &str = "approved";
pub const TOKEN_STATUS_REJECTED: &str = "rejected";
pub const TOKEN_STATUS_CONSUMED: &str = "consumed";

/// 用户节点网关注册令牌
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct UserNodeGatewayToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    pub token_preview: String,
    pub status: String,
    pub is_revealed: bool,
    pub approved_by: Option<Uuid>,
    pub actioned_at: Option<DateTime<Utc>>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub consumed_node_id: Option<Uuid>,
    pub revoke_reason: Option<String>,
    pub issued_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 待审批 token + 用户邮箱（JOIN 查询结果）
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct PendingTokenWithUser {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_preview: String,
    pub status: String,
    pub issued_at: DateTime<Utc>,
    pub user_email: String,
}

/// 令牌响应（不含 hash，安全）
#[derive(Debug, Clone, Serialize)]
pub struct UserNodeGatewayTokenResponse {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_preview: String,
    pub status: String,
    pub is_revealed: bool,
    pub actioned_at: Option<DateTime<Utc>>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
    pub issued_at: DateTime<Utc>,
}

impl From<UserNodeGatewayToken> for UserNodeGatewayTokenResponse {
    fn from(t: UserNodeGatewayToken) -> Self {
        Self {
            id: t.id,
            user_id: t.user_id,
            token_preview: t.token_preview,
            status: t.status,
            is_revealed: t.is_revealed,
            actioned_at: t.actioned_at,
            consumed_at: t.consumed_at,
            revoke_reason: t.revoke_reason,
            issued_at: t.issued_at,
        }
    }
}

impl UserNodeGatewayToken {
    pub const TOKEN_PREFIX: &'static str = "kcng";

    /// 计算 token 的 SHA-256 hash
    pub fn hash_token(token: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let result = hasher.finalize();
        format!("{:x}", result)
    }

    /// 使用 HMAC-SHA256 对 token_id 签名
    fn hmac_sign(secret: &[u8], token_id_str: &str) -> String {
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC key can be any length");
        mac.update(token_id_str.as_bytes());
        let result = mac.finalize();
        let code_bytes = result.into_bytes();
        hex::encode(
            code_bytes
                .get(16..)
                .expect("HMAC-SHA256 output is always 32 bytes; slice [16..] is always valid"),
        )
    }

    /// 生成 HMAC 签名的 token
    pub fn generate_hmac_token(secret: &[u8]) -> (Uuid, String, String, String) {
        let token_id = Uuid::new_v4();
        let token_id_str = token_id.to_string().replace('-', "");
        let signature = Self::hmac_sign(secret, &token_id_str);
        let token = format!("{}-{}-{}", Self::TOKEN_PREFIX, token_id_str, signature);
        let hash = Self::hash_token(&token);
        let preview = token.chars().take(16).collect::<String>();
        (token_id, token, hash, preview)
    }

    /// 验证 token 的 HMAC 签名
    pub fn validate_hmac_token(token: &str, secret: &[u8]) -> Result<Uuid, &'static str> {
        let rest = token.strip_prefix("kcng-").ok_or("Invalid token prefix")?;
        let (token_id_str, signature) = rest.rsplit_once('-').ok_or("Invalid token format")?;
        if token_id_str.len() != 32 || signature.len() != 32 {
            return Err("Invalid token format");
        }
        let uuid_str = format!(
            "{}-{}-{}-{}-{}",
            &token_id_str[..8],
            &token_id_str[8..12],
            &token_id_str[12..16],
            &token_id_str[16..20],
            &token_id_str[20..32]
        );
        let token_id = Uuid::parse_str(&uuid_str).map_err(|_| "Invalid token_id UUID")?;
        let expected = Self::hmac_sign(secret, token_id_str);
        if !bool::from(expected.as_bytes().ct_eq(signature.as_bytes())) {
            return Err("Invalid token signature");
        }
        Ok(token_id)
    }

    /// 根据 token_id 重建完整 token 明文
    pub fn reconstruct_token(secret: &[u8], token_id: Uuid) -> String {
        let token_id_str = token_id.to_string().replace('-', "");
        let signature = Self::hmac_sign(secret, &token_id_str);
        format!("{}-{}-{}", Self::TOKEN_PREFIX, token_id_str, signature)
    }

    // =========================================================================
    // 数据库 CRUD 方法
    // =========================================================================

    /// 创建新的令牌记录
    pub async fn create_with_id(
        db: &DatabaseConnection,
        token_id: Uuid,
        user_id: Uuid,
        token_hash: &str,
        token_preview: &str,
    ) -> Result<UserNodeGatewayToken, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO user_node_gateway_tokens (id, user_id, token_hash, token_preview) VALUES ($1, $2, $3, $4) RETURNING *"#,
            [
                token_id.into(),
                user_id.into(),
                token_hash.into(),
                token_preview.into(),
            ],
        );
        let token = UserNodeGatewayToken::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(token)
    }

    /// 根据 token_id 查找（主键查询）
    pub async fn find_by_id(
        db: &DatabaseConnection,
        id: Uuid,
    ) -> Result<Option<UserNodeGatewayToken>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_node_gateway_tokens WHERE id = $1",
            [id.into()],
        );
        let token = UserNodeGatewayToken::find_by_statement(stmt)
            .one(db)
            .await?;

        Ok(token)
    }

    /// 查找用户最近一次申请的 token（任意状态）
    pub async fn find_latest_by_user(
        db: &DatabaseConnection,
        user_id: Uuid,
    ) -> Result<Option<UserNodeGatewayToken>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_node_gateway_tokens WHERE user_id = $1 ORDER BY issued_at DESC LIMIT 1",
            [user_id.into()],
        );
        let token = UserNodeGatewayToken::find_by_statement(stmt)
            .one(db)
            .await?;

        Ok(token)
    }

    /// 检查用户是否已有阻止新申请的令牌
    pub async fn find_blocking_token(
        db: &DatabaseConnection,
        user_id: Uuid,
    ) -> Result<Option<UserNodeGatewayToken>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT * FROM user_node_gateway_tokens WHERE user_id = $1 AND (status IN ('pending', 'approved', 'consumed') OR (status = 'rejected' AND revoke_reason IS NOT NULL)) ORDER BY issued_at DESC LIMIT 1"#,
            [user_id.into()],
        );
        let token = UserNodeGatewayToken::find_by_statement(stmt)
            .one(db)
            .await?;

        Ok(token)
    }

    /// 查找用户所有历史 token
    pub async fn find_all_by_user(
        db: &DatabaseConnection,
        user_id: Uuid,
    ) -> Result<Vec<UserNodeGatewayToken>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_node_gateway_tokens WHERE user_id = $1 ORDER BY issued_at DESC",
            [user_id.into()],
        );
        let tokens = UserNodeGatewayToken::find_by_statement(stmt)
            .all(db)
            .await?;

        Ok(tokens)
    }

    /// 列出待审批 token 并附带用户邮箱
    pub async fn list_pending_with_users(
        db: &DatabaseConnection,
    ) -> Result<Vec<PendingTokenWithUser>, DbError> {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            r#"SELECT t.id, t.user_id, t.token_preview, t.status, t.issued_at, COALESCE(u.email, 'deleted_user') AS user_email FROM user_node_gateway_tokens t LEFT JOIN users u ON t.user_id = u.id WHERE t.status = 'pending' ORDER BY t.issued_at ASC LIMIT 100"#.to_string(),
        );
        let rows = PendingTokenWithUser::find_by_statement(stmt)
            .all(db)
            .await?;

        Ok(rows)
    }

    /// 审批通过 token
    pub async fn approve(
        &self,
        db: &DatabaseConnection,
        approved_by: Uuid,
    ) -> Result<bool, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_node_gateway_tokens SET status = 'approved', approved_by = $1, actioned_at = NOW(), updated_at = NOW() WHERE id = $2 AND status = 'pending'"#,
            [approved_by.into(), self.id.into()],
        );
        let result = db.execute(stmt).await?;

        Ok(result.rows_affected() > 0)
    }

    /// 拒绝 token 申请
    pub async fn reject(
        &self,
        db: &DatabaseConnection,
        approved_by: Uuid,
    ) -> Result<bool, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_node_gateway_tokens SET status = 'rejected', approved_by = $1, actioned_at = NOW(), updated_at = NOW() WHERE id = $2 AND status = 'pending'"#,
            [approved_by.into(), self.id.into()],
        );
        let result = db.execute(stmt).await?;

        Ok(result.rows_affected() > 0)
    }

    /// 吊销 token（用户主动删除，仅限未消费的 token）
    pub async fn revoke(&self, db: &DatabaseConnection) -> Result<bool, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_node_gateway_tokens SET status = 'rejected', updated_at = NOW() WHERE id = $1 AND status != 'consumed'"#,
            [self.id.into()],
        );
        let result = db.execute(stmt).await?;

        Ok(result.rows_affected() > 0)
    }

    /// 标记 token 已被用户查看
    pub async fn mark_revealed(&self, db: &DatabaseConnection) -> Result<(), DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_node_gateway_tokens SET is_revealed = TRUE, updated_at = NOW() WHERE id = $1"#,
            [self.id.into()],
        );
        db.execute(stmt).await?;

        Ok(())
    }

    /// 消费 token（节点注册时调用，一次性使用）
    pub async fn consume(
        db: &DatabaseConnection,
        token_id: Uuid,
        node_id: Uuid,
    ) -> Result<bool, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_node_gateway_tokens SET status = 'consumed', consumed_at = NOW(), consumed_node_id = $1, updated_at = NOW() WHERE id = $2 AND status = 'approved'"#,
            [node_id.into(), token_id.into()],
        );
        let result = db.execute(stmt).await?;

        Ok(result.rows_affected() > 0)
    }

    /// 检查 token 是否可被消费
    pub fn is_consumable(&self) -> bool {
        self.status == TOKEN_STATUS_APPROVED
    }

    /// Admin 吊销令牌并记录原因
    pub async fn revoke_with_reason(
        db: &DatabaseConnection,
        token_id: Uuid,
        reason: &str,
    ) -> Result<bool, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_node_gateway_tokens SET status = 'rejected', revoke_reason = $1, actioned_at = NOW(), updated_at = NOW() WHERE id = $2"#,
            [reason.into(), token_id.into()],
        );
        let result = db.execute(stmt).await?;

        Ok(result.rows_affected() > 0)
    }

    /// Admin 恢复节点时同步恢复被吊销的令牌
    pub async fn restore_from_revoked(
        db: &DatabaseConnection,
        token_id: Uuid,
    ) -> Result<bool, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_node_gateway_tokens SET status = 'approved', revoke_reason = NULL, actioned_at = NOW(), updated_at = NOW() WHERE id = $1 AND status = 'rejected' AND revoke_reason IS NOT NULL AND NOT EXISTS (SELECT 1 FROM user_node_gateway_tokens t2 WHERE t2.user_id = (SELECT user_id FROM user_node_gateway_tokens WHERE id = $1) AND t2.status IN ('pending', 'approved') AND t2.id != $1)"#,
            [token_id.into()],
        );
        let result = db.execute(stmt).await?;

        Ok(result.rows_affected() > 0)
    }

    /// 按 consumed_node_id 反查 token
    pub async fn find_by_consumed_node_id(
        db: &impl ConnectionTrait,
        node_id: Uuid,
    ) -> Result<Option<UserNodeGatewayToken>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_node_gateway_tokens WHERE consumed_node_id = $1 LIMIT 1",
            [node_id.into()],
        );
        let token = UserNodeGatewayToken::find_by_statement(stmt)
            .one(db)
            .await?;

        Ok(token)
    }

    /// 硬删除 token 记录
    pub async fn delete_by_id(db: &DatabaseConnection, id: Uuid) -> Result<bool, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM user_node_gateway_tokens WHERE id = $1",
            [id.into()],
        );
        let result = db.execute(stmt).await?;

        Ok(result.rows_affected() > 0)
    }

    /// 用户删除自己被管理员拒绝的令牌记录
    pub async fn delete_if_rejected_no_reason(
        db: &DatabaseConnection,
        token_id: Uuid,
        user_id: Uuid,
    ) -> Result<bool, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"DELETE FROM user_node_gateway_tokens WHERE id = $1 AND user_id = $2 AND status = 'rejected' AND revoke_reason IS NULL"#,
            [token_id.into(), user_id.into()],
        );
        let result = db.execute(stmt).await?;

        Ok(result.rows_affected() > 0)
    }
}
