use crate::{DbError, Tenant, TenantMembership, User};
use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Produce AI Key 模型（用户访问系统的 API Key）
#[derive(Clone, FromQueryResult, Serialize, Deserialize)]
pub struct ProduceAiKey {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    #[serde(skip_serializing)]
    pub produce_ai_key_hash: String,
    #[serde(skip)]
    pub produce_ai_key_ciphertext: String,
    #[serde(skip)]
    pub produce_ai_key_nonce: String,
    #[serde(skip)]
    pub produce_ai_key_wrapped_dek: String,
    #[serde(skip)]
    pub produce_ai_key_wrap_nonce: String,
    #[serde(skip)]
    pub produce_ai_key_kek_id: String,
    #[serde(skip)]
    pub produce_ai_key_encryption_version: i16,
    pub produce_ai_key_preview: String,
    pub revoked: bool,
    pub revoked_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for ProduceAiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProduceAiKey")
            .field("id", &self.id)
            .field("tenant_id", &self.tenant_id)
            .field("user_id", &self.user_id)
            .field("name", &self.name)
            .field("produce_ai_key_hash", &"[REDACTED]")
            .field("produce_ai_key_ciphertext", &"[REDACTED]")
            .field("produce_ai_key_nonce", &"[REDACTED]")
            .field("produce_ai_key_wrapped_dek", &"[REDACTED]")
            .field("produce_ai_key_wrap_nonce", &"[REDACTED]")
            .field("produce_ai_key_kek_id", &self.produce_ai_key_kek_id)
            .field(
                "produce_ai_key_encryption_version",
                &self.produce_ai_key_encryption_version,
            )
            .field("produce_ai_key_preview", &self.produce_ai_key_preview)
            .field("revoked", &self.revoked)
            .field("revoked_at", &self.revoked_at)
            .field("expires_at", &self.expires_at)
            .field("last_used_at", &self.last_used_at)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
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

const REVEAL_TENANT_QUERY: &str =
    "SELECT id FROM tenants WHERE id=$1 AND status='active' FOR SHARE";
const REVEAL_USER_QUERY: &str = "SELECT id FROM users WHERE id=$1 AND status='active' FOR SHARE";
const REVEAL_MEMBERSHIP_QUERY: &str = "SELECT tenant_id FROM tenant_memberships
             WHERE tenant_id=$1 AND user_id=$2 AND status='active' FOR SHARE";
const REVEAL_KEY_QUERY: &str = "SELECT produce_ai_key_ciphertext,produce_ai_key_nonce,
                    produce_ai_key_wrapped_dek,produce_ai_key_wrap_nonce,
                    produce_ai_key_kek_id,produce_ai_key_encryption_version
             FROM produce_ai_keys
             WHERE id=$1 AND tenant_id=$2 AND user_id=$3
               AND NOT revoked
               AND (expires_at IS NULL OR expires_at > clock_timestamp())
             FOR SHARE";

/// Serialize creation of a replacement intent with permanent deletion of its
/// source key. A row lock cannot protect the "no pending intent" case, so both
/// paths take the same transaction-scoped advisory lane before inspecting
/// intents or locking the credential row. Hash collisions only serialize
/// unrelated keys; they cannot weaken correctness.
pub(crate) async fn lock_key_rotation_lane(
    tx: &DatabaseTransaction,
    tenant_id: Uuid,
    key_id: Uuid,
) -> Result<(), DbError> {
    if tenant_id.is_nil() || key_id.is_nil() {
        return Err(DbError::Other("invalid key rotation lock identity".into()));
    }
    tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        [format!("keycompute:key-rotation:{tenant_id}:{key_id}").into()],
    ))
    .await?;
    Ok(())
}

impl ProduceAiKey {
    /// Read the recoverable secret only through the owner-scoped path. The
    /// regular metadata response intentionally never includes this column.
    ///
    /// All authority rows and the key are locked in the same deterministic
    /// order used by mutations, so a reveal cannot race a committed identity,
    /// membership, tenant-lifecycle, expiry, revocation or deletion change.
    pub async fn reveal_owned(
        tx: &DatabaseTransaction,
        scope: keycompute_types::TenantScope,
        id: Uuid,
    ) -> Result<Option<keycompute_runtime::ProduceAiKeyEnvelope>, DbError> {
        if scope.tenant_id().is_nil() || scope.user_id().is_nil() || id.is_nil() {
            return Err(DbError::Other("key management authorization denied".into()));
        }
        super::tenant_audit_event::lock_identity_admin(tx).await?;
        for (query, values) in [
            (REVEAL_TENANT_QUERY, vec![scope.tenant_id().into()]),
            (REVEAL_USER_QUERY, vec![scope.user_id().into()]),
            (
                REVEAL_MEMBERSHIP_QUERY,
                vec![scope.tenant_id().into(), scope.user_id().into()],
            ),
        ] {
            if tx
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    query,
                    values,
                ))
                .await?
                .is_none()
            {
                return Ok(None);
            }
        }
        #[derive(FromQueryResult)]
        struct Secret {
            produce_ai_key_ciphertext: String,
            produce_ai_key_nonce: String,
            produce_ai_key_wrapped_dek: String,
            produce_ai_key_wrap_nonce: String,
            produce_ai_key_kek_id: String,
            produce_ai_key_encryption_version: i16,
        }
        let row = Secret::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            REVEAL_KEY_QUERY,
            [id.into(), scope.tenant_id().into(), scope.user_id().into()],
        ))
        .one(tx)
        .await?;
        Ok(row.map(|secret| keycompute_runtime::ProduceAiKeyEnvelope {
            ciphertext: secret.produce_ai_key_ciphertext,
            nonce: secret.produce_ai_key_nonce,
            wrapped_dek: secret.produce_ai_key_wrapped_dek,
            wrap_nonce: secret.produce_ai_key_wrap_nonce,
            kek_key_id: secret.produce_ai_key_kek_id,
            encryption_version: secret.produce_ai_key_encryption_version,
        }))
    }

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

#[cfg(test)]
mod tests {
    use super::{
        REVEAL_KEY_QUERY, REVEAL_MEMBERSHIP_QUERY, REVEAL_TENANT_QUERY, REVEAL_USER_QUERY,
    };

    #[test]
    fn reveal_queries_lock_every_authority_row_and_reject_inactive_keys() {
        for query in [
            REVEAL_TENANT_QUERY,
            REVEAL_USER_QUERY,
            REVEAL_MEMBERSHIP_QUERY,
            REVEAL_KEY_QUERY,
        ] {
            assert!(query.contains("FOR SHARE"));
        }
        assert!(REVEAL_KEY_QUERY.contains("AND NOT revoked"));
        assert!(REVEAL_KEY_QUERY.contains("expires_at > clock_timestamp()"));
    }
}

#[path = "api_key_scope.rs"]
mod scope;
pub use scope::KeyRemoval;
