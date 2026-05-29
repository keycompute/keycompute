//! 用户节点网关注册令牌模型
//!
//! 审批制 + HMAC 签名 + 一次性使用：
//!   1. 用户申请 → status='pending'（仅创建记录，不生成 token）
//!   2. Admin 审批 → status='approved'（生成 HMAC 签名 token）
//!   3. 用户查看 → GET 返回 token 明文（始终可重建，is_revealed 仅标记首次查看）
//!   4. 节点注册 → status='consumed'（一次性消耗）

use crate::DbError;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use subtle::ConstantTimeEq;
use uuid::Uuid;

/// Token 状态常量
pub const TOKEN_STATUS_PENDING: &str = "pending";
pub const TOKEN_STATUS_APPROVED: &str = "approved";
pub const TOKEN_STATUS_REJECTED: &str = "rejected";
pub const TOKEN_STATUS_CONSUMED: &str = "consumed";

/// 用户节点网关注册令牌
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
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

/// 待审批 token + 用户邮箱（JOIN 查询结果，避免 N+1）
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
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
    // =========================================================================
    // HMAC 签名相关方法
    // =========================================================================

    /// Token 格式前缀
    pub const TOKEN_PREFIX: &'static str = "kcng";

    /// 计算 token 的 SHA-256 hash（公共工具方法，供 node-gateway store 和 server handler 复用）
    pub fn hash_token(token: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let result = hasher.finalize();
        format!("{:x}", result)
    }

    /// 使用 HMAC-SHA256 对 token_id 签名
    ///
    /// 取 HMAC 输出的后 16 字节（128 bit）作为签名 →  32 个十六进制字符。
    ///
    /// 设计决策：完整 HMAC-SHA256 输出为 256 bit，截取后 128 bit 可缩短 token 长度
    /// 同时保持足够的熵值。对于一次性注册 token 场景，128 bit 安全强度已足够
    /// （碰撞概率 ≈ 2^-128，远超 UUID v4 的 122 bit）。
    ///
    /// # Panics
    ///
    /// 在 `Hmac<Sha256>` 输出 < 16 字节时 panic（正常情况下 SHA-256 固定 32 字节，
    /// 此 panic 仅会在泛型参数被错误替换时触发，属于编译期/测试期可发现的 bug）。
    fn hmac_sign(secret: &[u8], token_id_str: &str) -> String {
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC key can be any length");
        mac.update(token_id_str.as_bytes());
        let result = mac.finalize();
        let code_bytes = result.into_bytes();
        // 取后 16 字节 → 32 个十六进制字符
        // 防御性边界检查：HMAC-SHA256 固定输出 32 字节，此断言仅在泛型误用时失败
        hex::encode(
            code_bytes
                .get(16..)
                .expect("HMAC-SHA256 output is always 32 bytes; slice [16..] is always valid"),
        )
    }

    /// 生成 HMAC 签名的 token
    ///
    /// 格式: `kcng-{token_id}-{signature}`
    /// - token_id = UUID v4 去连字符（32 个十六进制字符）
    /// - signature = HMAC-SHA256(secret, token_id) 后 16 字节 → 32 个十六进制字符
    ///
    /// 返回: (token_id_uuid, full_token, token_hash, token_preview)
    pub fn generate_hmac_token(secret: &[u8]) -> (Uuid, String, String, String) {
        let token_id = Uuid::new_v4();
        let token_id_str = token_id.to_string().replace('-', "");
        let signature = Self::hmac_sign(secret, &token_id_str);
        let token = format!("{}-{}-{}", Self::TOKEN_PREFIX, token_id_str, signature);

        let hash = Self::hash_token(&token);
        // 前 16 位预览（例如 "kcng-a1b2c3d4e5f6"）
        let preview = token.chars().take(16).collect::<String>();

        (token_id, token, hash, preview)
    }

    /// 验证 token 的 HMAC 签名
    ///
    /// 解析格式 `kcng-{token_id}-{signature}`，重新计算 HMAC 并比对。
    /// 使用常量时间比较防止时序攻击。
    ///
    /// 成功返回 token_id (UUID)，失败返回错误描述。
    pub fn validate_hmac_token(token: &str, secret: &[u8]) -> Result<Uuid, &'static str> {
        // 1. 解析格式
        let rest = token.strip_prefix("kcng-").ok_or("Invalid token prefix")?;

        let (token_id_str, signature) = rest.rsplit_once('-').ok_or("Invalid token format")?;

        if token_id_str.len() != 32 || signature.len() != 32 {
            return Err("Invalid token format");
        }

        // 2. 恢复 UUID（重新插入连字符）
        let uuid_str = format!(
            "{}-{}-{}-{}-{}",
            &token_id_str[..8],
            &token_id_str[8..12],
            &token_id_str[12..16],
            &token_id_str[16..20],
            &token_id_str[20..32]
        );
        let token_id = Uuid::parse_str(&uuid_str).map_err(|_| "Invalid token_id UUID")?;

        // 3. 验证 HMAC 签名（常量时间比较，防止时序攻击）
        let expected = Self::hmac_sign(secret, token_id_str);
        if !bool::from(expected.as_bytes().ct_eq(signature.as_bytes())) {
            return Err("Invalid token signature");
        }

        Ok(token_id)
    }

    /// 根据 token_id 重建完整 token 明文
    ///
    /// 用于审批后返回给用户（从 DB 中的 id 重新计算）。
    pub fn reconstruct_token(secret: &[u8], token_id: Uuid) -> String {
        let token_id_str = token_id.to_string().replace('-', "");
        let signature = Self::hmac_sign(secret, &token_id_str);
        format!("{}-{}-{}", Self::TOKEN_PREFIX, token_id_str, signature)
    }

    // =========================================================================
    // 数据库 CRUD 方法
    // =========================================================================

    /// 创建新的令牌记录（使用指定的 token_id，即 HMAC 签名中的 UUID）
    pub async fn create_with_id(
        pool: &sqlx::PgPool,
        token_id: Uuid,
        user_id: Uuid,
        token_hash: &str,
        token_preview: &str,
    ) -> Result<UserNodeGatewayToken, DbError> {
        let token = sqlx::query_as::<_, UserNodeGatewayToken>(
            r#"
            INSERT INTO user_node_gateway_tokens (id, user_id, token_hash, token_preview)
            VALUES ($1, $2, $3, $4)
            RETURNING *
            "#,
        )
        .bind(token_id)
        .bind(user_id)
        .bind(token_hash)
        .bind(token_preview)
        .fetch_one(pool)
        .await?;

        Ok(token)
    }

    /// 根据 token_id 查找（主键查询）
    pub async fn find_by_id(
        pool: &sqlx::PgPool,
        id: Uuid,
    ) -> Result<Option<UserNodeGatewayToken>, DbError> {
        let token = sqlx::query_as::<_, UserNodeGatewayToken>(
            "SELECT * FROM user_node_gateway_tokens WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;

        Ok(token)
    }

    /// 查找用户最近一次申请的 token（任意状态）
    pub async fn find_latest_by_user(
        pool: &sqlx::PgPool,
        user_id: Uuid,
    ) -> Result<Option<UserNodeGatewayToken>, DbError> {
        let token = sqlx::query_as::<_, UserNodeGatewayToken>(
            "SELECT * FROM user_node_gateway_tokens WHERE user_id = $1 ORDER BY issued_at DESC LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(pool)
        .await?;

        Ok(token)
    }

    /// 检查用户是否已有阻止新申请的令牌
    /// 阻止状态: pending(待审批) / approved(已通过) / consumed(已使用) / rejected+revoke_reason(已吊销)
    /// 返回 Option<status> 当存在阻止令牌时返回其状态，否则返回 None
    pub async fn find_blocking_token(
        pool: &sqlx::PgPool,
        user_id: Uuid,
    ) -> Result<Option<UserNodeGatewayToken>, DbError> {
        let token = sqlx::query_as::<_, UserNodeGatewayToken>(
            r#"
            SELECT * FROM user_node_gateway_tokens
            WHERE user_id = $1
              AND (
                status IN ('pending', 'approved', 'consumed')
                OR (status = 'rejected' AND revoke_reason IS NOT NULL)
              )
            ORDER BY issued_at DESC
            LIMIT 1
            "#,
        )
        .bind(user_id)
        .fetch_optional(pool)
        .await?;

        Ok(token)
    }

    /// 查找用户所有历史 token（按 issued_at 倒序）
    pub async fn find_all_by_user(
        pool: &sqlx::PgPool,
        user_id: Uuid,
    ) -> Result<Vec<UserNodeGatewayToken>, DbError> {
        let tokens = sqlx::query_as::<_, UserNodeGatewayToken>(
            "SELECT * FROM user_node_gateway_tokens WHERE user_id = $1 ORDER BY issued_at DESC",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await?;

        Ok(tokens)
    }

    /// 列出待审批 token 并附带用户邮箱（Admin 使用，单次 JOIN 查询避免 N+1）
    pub async fn list_pending_with_users(
        pool: &sqlx::PgPool,
    ) -> Result<Vec<PendingTokenWithUser>, DbError> {
        let rows = sqlx::query_as::<_, PendingTokenWithUser>(
            r#"
            SELECT t.id, t.user_id, t.token_preview, t.status, t.issued_at,
                   COALESCE(u.email, 'deleted_user') AS user_email
            FROM user_node_gateway_tokens t
            LEFT JOIN users u ON t.user_id = u.id
            WHERE t.status = 'pending'
            ORDER BY t.issued_at ASC
            LIMIT 100
            "#,
        )
        .fetch_all(pool)
        .await?;

        Ok(rows)
    }

    /// 审批通过 token
    /// 返回 true 表示审批成功，false 表示 token 状态已变化（并发冲突）
    pub async fn approve(&self, pool: &sqlx::PgPool, approved_by: Uuid) -> Result<bool, DbError> {
        let result = sqlx::query(
            r#"
            UPDATE user_node_gateway_tokens
            SET status = 'approved',
                approved_by = $1,
                actioned_at = NOW(),
                updated_at = NOW()
            WHERE id = $2 AND status = 'pending'
            "#,
        )
        .bind(approved_by)
        .bind(self.id)
        .execute(pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// 拒绝 token 申请
    ///
    /// 返回 true 表示拒绝成功，false 表示 token 状态已变化（并发冲突）
    pub async fn reject(&self, pool: &sqlx::PgPool, approved_by: Uuid) -> Result<bool, DbError> {
        let result = sqlx::query(
            r#"
            UPDATE user_node_gateway_tokens
            SET status = 'rejected',
                approved_by = $1,
                actioned_at = NOW(),
                updated_at = NOW()
            WHERE id = $2 AND status = 'pending'
            "#,
        )
        .bind(approved_by)
        .bind(self.id)
        .execute(pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// 吊销 token（用户主动删除，仅限未消费的 token）
    /// 返回 true 表示吊销成功，false 表示 token 不存在或已消费
    pub async fn revoke(&self, pool: &sqlx::PgPool) -> Result<bool, DbError> {
        let result = sqlx::query(
            r#"
            UPDATE user_node_gateway_tokens
            SET status = 'rejected',
                updated_at = NOW()
            WHERE id = $1 AND status != 'consumed'
            "#,
        )
        .bind(self.id)
        .execute(pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// 标记 token 已被用户查看（用于安全提醒，不影响 token 明文重建）
    pub async fn mark_revealed(&self, pool: &sqlx::PgPool) -> Result<(), DbError> {
        sqlx::query(
            r#"
            UPDATE user_node_gateway_tokens
            SET is_revealed = TRUE,
                updated_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(self.id)
        .execute(pool)
        .await?;

        Ok(())
    }

    /// 消费 token（节点注册时调用，一次性使用）
    /// 在事务中调用，确保原子性。
    /// 接受 `Executor` 以便同时支持 `&PgPool` 和 `&mut Transaction`。
    pub async fn consume<'e, E>(executor: E, token_id: Uuid, node_id: Uuid) -> Result<bool, DbError>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        let result = sqlx::query(
            r#"
            UPDATE user_node_gateway_tokens
            SET status = 'consumed',
                consumed_at = NOW(),
                consumed_node_id = $1,
                updated_at = NOW()
            WHERE id = $2 AND status = 'approved'
            "#,
        )
        .bind(node_id)
        .bind(token_id)
        .execute(executor)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// 检查 token 是否可被消费（状态为 approved 且未被消费）
    pub fn is_consumable(&self) -> bool {
        self.status == TOKEN_STATUS_APPROVED
    }

    /// Admin 吊销令牌并记录原因
    /// 将 token 状态改为 rejected 并设置 revoke_reason
    pub async fn revoke_with_reason(
        pool: &sqlx::PgPool,
        token_id: Uuid,
        reason: &str,
    ) -> Result<bool, DbError> {
        let result = sqlx::query(
            r#"
            UPDATE user_node_gateway_tokens
            SET status = 'rejected',
                revoke_reason = $1,
                actioned_at = NOW(),
                updated_at = NOW()
            WHERE id = $2
            "#,
        )
        .bind(reason)
        .bind(token_id)
        .execute(pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Admin 恢复节点时同步恢复被吊销的令牌
    /// 将 status='rejected' 且存在 revoke_reason 的 token 恢复为 'approved',
    /// 清除 revoke_reason 并更新 actioned_at。
    /// 若该用户已有另一个活跃 token（pending/approved），则跳过恢复返回 false。
    pub async fn restore_from_revoked(
        pool: &sqlx::PgPool,
        token_id: Uuid,
    ) -> Result<bool, DbError> {
        let result = sqlx::query(
            r#"
            UPDATE user_node_gateway_tokens
            SET status = 'approved',
                revoke_reason = NULL,
                actioned_at = NOW(),
                updated_at = NOW()
            WHERE id = $1
              AND status = 'rejected'
              AND revoke_reason IS NOT NULL
              AND NOT EXISTS (
                SELECT 1 FROM user_node_gateway_tokens t2
                WHERE t2.user_id = (
                    SELECT user_id FROM user_node_gateway_tokens WHERE id = $1
                )
                  AND t2.status IN ('pending', 'approved')
                  AND t2.id != $1
              )
            "#,
        )
        .bind(token_id)
        .execute(pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// 按 consumed_node_id 反查 token
    ///
    /// 接受泛型 `Executor` 以同时支持 `&PgPool` 和 `&mut Transaction`，
    /// 避免在事务场景下出现 TOCTOU 竞态条件。
    pub async fn find_by_consumed_node_id<'e, E>(
        executor: E,
        node_id: Uuid,
    ) -> Result<Option<UserNodeGatewayToken>, DbError>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        let token = sqlx::query_as::<_, UserNodeGatewayToken>(
            "SELECT * FROM user_node_gateway_tokens WHERE consumed_node_id = $1 LIMIT 1",
        )
        .bind(node_id)
        .fetch_optional(executor)
        .await?;

        Ok(token)
    }

    /// 硬删除 token 记录
    pub async fn delete_by_id(pool: &sqlx::PgPool, id: Uuid) -> Result<bool, DbError> {
        let result = sqlx::query("DELETE FROM user_node_gateway_tokens WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }

    /// 用户删除自己被管理员拒绝的令牌记录
    /// 仅当 status='rejected' 且 revoke_reason IS NULL（管理员拒绝申请）时才允许删除
    pub async fn delete_if_rejected_no_reason(
        pool: &sqlx::PgPool,
        token_id: Uuid,
        user_id: Uuid,
    ) -> Result<bool, DbError> {
        let result = sqlx::query(
            r#"
            DELETE FROM user_node_gateway_tokens
            WHERE id = $1
              AND user_id = $2
              AND status = 'rejected'
              AND revoke_reason IS NULL
            "#,
        )
        .bind(token_id)
        .bind(user_id)
        .execute(pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }
}
