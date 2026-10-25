//! Crypto Module
//!
//! 提供加密解密功能，主要用于上游 Provider API Key 的安全存储。
//!
//! ## 使用方式
//!
//! ```rust,ignore
//! use keycompute_runtime::crypto::{ApiKeyCrypto, EncryptedApiKey};
//!
//! // 生成密钥（只需执行一次，应安全存储）
//! let secret_key = ApiKeyCrypto::generate_key();
//!
//! // 创建加密器
//! let crypto = ApiKeyCrypto::new(&secret_key);
//!
//! // 加密 API Key
//! let encrypted = crypto.encrypt("sk-xxxxxxxxxxxxxxxx")?;
//!
//! // 解密 API Key
//! let decrypted = crypto.decrypt(&encrypted)?;
//! ```

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use hkdf::Hkdf;
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use std::fmt;
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

/// 加密相关错误
#[derive(Debug, Error)]
pub enum CryptoError {
    /// 加密失败
    #[error("Encryption failed: {0}")]
    EncryptionFailed(String),

    /// 解密失败
    #[error("Decryption failed: {0}")]
    DecryptionFailed(String),

    /// 无效的密钥
    #[error("Invalid key: {0}")]
    InvalidKey(String),

    /// 无效的密文格式
    #[error("Invalid ciphertext format")]
    InvalidCiphertextFormat,

    /// Base64 解码失败
    #[error("Base64 decode failed: {0}")]
    Base64DecodeError(#[from] base64::DecodeError),

    /// 密钥包装器与密文记录不匹配
    #[error("Key-encryption key mismatch")]
    KeyEncryptionKeyMismatch,

    /// 不支持的信封格式
    #[error("Unsupported envelope encryption version: {0}")]
    UnsupportedEnvelopeVersion(i16),
}

/// AES-256-GCM 密钥长度 (32 bytes)
const KEY_SIZE: usize = 32;

/// GCM Nonce 长度 (12 bytes, GCM 推荐值)
const NONCE_SIZE: usize = 12;

/// Produce AI Key 信封格式版本。版本参与 AAD，不能静默降级。
pub const PRODUCE_AI_KEY_ENVELOPE_VERSION: i16 = 1;

/// 单条 Produce AI Key 的不可变加密上下文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProduceAiKeyEnvelopeContext {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub key_id: Uuid,
}

impl ProduceAiKeyEnvelopeContext {
    fn validate(self) -> Result<Self, CryptoError> {
        if self.tenant_id.is_nil() || self.user_id.is_nil() || self.key_id.is_nil() {
            return Err(CryptoError::InvalidCiphertextFormat);
        }
        Ok(self)
    }

    fn aad(self, purpose: &str, version: i16) -> Vec<u8> {
        format!(
            "keycompute:produce-ai-key:{purpose}:v{version}:{}:{}:{}",
            self.tenant_id, self.user_id, self.key_id
        )
        .into_bytes()
    }
}

/// 数据库可持久化的单记录信封。两个 nonce 与密文分离保存，便于审计
/// 格式并为后续 KMS/Vault KEK 提供方保留稳定边界。
#[derive(Clone, PartialEq, Eq)]
pub struct ProduceAiKeyEnvelope {
    pub ciphertext: String,
    pub nonce: String,
    pub wrapped_dek: String,
    pub wrap_nonce: String,
    pub kek_key_id: String,
    pub encryption_version: i16,
}

impl fmt::Debug for ProduceAiKeyEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProduceAiKeyEnvelope")
            .field("ciphertext", &"[REDACTED]")
            .field("nonce", &"[REDACTED]")
            .field("wrapped_dek", &"[REDACTED]")
            .field("wrap_nonce", &"[REDACTED]")
            .field("kek_key_id", &self.kek_key_id)
            .field("encryption_version", &self.encryption_version)
            .finish()
    }
}

/// 解密后的短生命周期秘密。Debug/Display 均不输出内容，离开作用域时
/// 清零进程内持有的字符串缓冲区。
pub struct DecryptedProduceAiKey(Zeroizing<String>);

impl DecryptedProduceAiKey {
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for DecryptedProduceAiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***REDACTED***")
    }
}

impl fmt::Display for DecryptedProduceAiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***REDACTED***")
    }
}

/// 加密后的 API Key
///
/// 格式: `base64(nonce || ciphertext)`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedApiKey(String);

impl EncryptedApiKey {
    /// 从 Base64 字符串创建
    pub fn from_base64(s: &str) -> Result<Self, CryptoError> {
        // 验证是否为有效的 base64
        BASE64.decode(s)?;
        Ok(Self(s.to_string()))
    }

    /// 获取 Base64 编码的加密数据
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 获取内部字符串
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for EncryptedApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for EncryptedApiKey {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for EncryptedApiKey {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl serde::Serialize for EncryptedApiKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for EncryptedApiKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Self(s))
    }
}

/// API Key 加密器
///
/// 使用 AES-256-GCM 算法进行加密解密。
/// 每次加密使用随机 Nonce，保证相同明文产生不同密文。
#[derive(Clone)]
pub struct ApiKeyCrypto {
    cipher: Aes256Gcm,
    key_id: String,
}

impl fmt::Debug for ApiKeyCrypto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKeyCrypto")
            .field("algorithm", &"AES-256-GCM")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

impl ApiKeyCrypto {
    /// 创建新的加密器
    ///
    /// # 参数
    /// - `secret_key`: 32 字节的密钥，Base64 编码
    ///
    /// # 错误
    /// 如果密钥长度不正确或 Base64 解码失败，返回错误
    pub fn new(secret_key: &str) -> Result<Self, CryptoError> {
        let key_bytes = Zeroizing::new(BASE64.decode(secret_key)?);

        if key_bytes.len() != KEY_SIZE {
            return Err(CryptoError::InvalidKey(format!(
                "Key must be {} bytes, got {}",
                KEY_SIZE,
                key_bytes.len()
            )));
        }

        Self::from_bytes(key_bytes.as_ref())
    }

    /// 从原始字节创建加密器
    ///
    /// # 参数
    /// - `key_bytes`: 32 字节的原始密钥
    pub fn from_bytes(key_bytes: &[u8]) -> Result<Self, CryptoError> {
        if key_bytes.len() != KEY_SIZE {
            return Err(CryptoError::InvalidKey(format!(
                "Key must be {} bytes, got {}",
                KEY_SIZE,
                key_bytes.len()
            )));
        }

        let key = aes_gcm::Key::<Aes256Gcm>::from_slice(key_bytes);
        let cipher = Aes256Gcm::new(key);
        let fingerprint = Sha256::digest(key_bytes);
        let key_id = format!("local-sha256-{}", hex_prefix(&fingerprint, 16));

        Ok(Self { cipher, key_id })
    }

    /// 生成随机密钥
    ///
    /// 返回 Base64 编码的 32 字节密钥。
    /// **重要**: 此密钥应安全存储，丢失后将无法解密已加密的数据。
    pub fn generate_key() -> String {
        let mut key = Zeroizing::new([0u8; KEY_SIZE]);
        OsRng.fill_bytes(key.as_mut());
        BASE64.encode(key.as_ref())
    }

    /// 生成随机 Nonce
    fn generate_nonce() -> [u8; NONCE_SIZE] {
        let mut nonce = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce);
        nonce
    }

    /// 不泄露 KEK 本身的稳定标识，用于阻止使用错误主密钥尝试解密。
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    fn encrypt_bytes_with_aad(
        &self,
        plaintext: &[u8],
        aad: &[u8],
    ) -> Result<(String, String), CryptoError> {
        let nonce_bytes = Self::generate_nonce();
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|e| CryptoError::EncryptionFailed(e.to_string()))?;
        Ok((BASE64.encode(ciphertext), BASE64.encode(nonce_bytes)))
    }

    fn decrypt_bytes_with_aad(
        &self,
        ciphertext: &str,
        nonce: &str,
        aad: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let nonce_bytes = BASE64.decode(nonce)?;
        if nonce_bytes.len() != NONCE_SIZE {
            return Err(CryptoError::InvalidCiphertextFormat);
        }
        let ciphertext = BASE64.decode(ciphertext)?;
        if ciphertext.len() < 16 {
            return Err(CryptoError::InvalidCiphertextFormat);
        }
        self.cipher
            .decrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: &ciphertext,
                    aad,
                },
            )
            .map_err(|e| CryptoError::DecryptionFailed(e.to_string()))
    }

    /// 加密 API Key
    ///
    /// # 参数
    /// - `plaintext`: 明文 API Key
    ///
    /// # 返回
    /// Base64 编码的加密数据，格式为 `base64(nonce || ciphertext)`
    pub fn encrypt(&self, plaintext: &str) -> Result<EncryptedApiKey, CryptoError> {
        let nonce_bytes = Self::generate_nonce();
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| CryptoError::EncryptionFailed(e.to_string()))?;

        // 组合 nonce 和 ciphertext
        let mut combined = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);

        // Base64 编码
        let encoded = BASE64.encode(&combined);

        Ok(EncryptedApiKey(encoded))
    }

    /// 解密 API Key
    ///
    /// # 参数
    /// - `encrypted`: 加密的 API Key
    ///
    /// # 返回
    /// 解密后的明文 API Key
    pub fn decrypt(&self, encrypted: &EncryptedApiKey) -> Result<String, CryptoError> {
        // Base64 解码
        let combined = BASE64.decode(encrypted.as_str())?;

        // 检查最小长度
        if combined.len() < NONCE_SIZE + 16 {
            // GCM tag 是 16 bytes
            return Err(CryptoError::InvalidCiphertextFormat);
        }

        // 分离 nonce 和 ciphertext
        let nonce_bytes = &combined[..NONCE_SIZE];
        let ciphertext = &combined[NONCE_SIZE..];

        let nonce = Nonce::from_slice(nonce_bytes);

        // 解密
        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| CryptoError::DecryptionFailed(e.to_string()))?;

        // 转换为字符串
        String::from_utf8(plaintext).map_err(|e| {
            CryptoError::DecryptionFailed(format!("Invalid UTF-8 in decrypted text: {}", e))
        })
    }

    /// 创建明文 API Key 的预览
    ///
    /// 用于数据库存储和日志显示，格式如 `sk-****abc`
    ///
    /// 注意：按 Unicode 字符（而非字节）切片，避免对包含多字节字符的输入
    /// 在非字符边界切片时 panic。
    pub fn create_preview(plaintext: &str) -> String {
        let chars: Vec<char> = plaintext.chars().collect();
        let len = chars.len();
        if len <= 7 {
            // 长度 <= 7 时，显示全部用 * 替代
            return "*".repeat(len);
        }

        let prefix: String = chars[..3].iter().collect();
        let suffix: String = chars[len - 3..].iter().collect();

        format!("{}****{}", prefix, suffix)
    }
}

/// 全局加密密钥管理
///
/// 用于存储和访问全局加密密钥。
/// 密钥应在应用启动时设置一次。
struct GlobalCryptoState {
    provider: ApiKeyCrypto,
    produce_ai_key_kek: ApiKeyCrypto,
}

static GLOBAL_CRYPTO: std::sync::OnceLock<GlobalCryptoState> = std::sync::OnceLock::new();

/// 设置全局加密密钥
///
/// 应在应用启动时调用一次。
///
/// # 参数
/// - `secret_key`: Base64 编码的 32 字节密钥
pub fn set_global_crypto(secret_key: &str) -> Result<(), CryptoError> {
    let provider = ApiKeyCrypto::new(secret_key)?;
    let master = Zeroizing::new(BASE64.decode(secret_key)?);
    if master.len() != KEY_SIZE {
        return Err(CryptoError::InvalidKey(format!(
            "Key must be {KEY_SIZE} bytes, got {}",
            master.len()
        )));
    }
    let mut produce_ai_key_kek = Zeroizing::new([0u8; KEY_SIZE]);
    Hkdf::<Sha256>::new(None, master.as_ref())
        .expand(
            b"keycompute:produce-ai-key:kek:v1",
            produce_ai_key_kek.as_mut(),
        )
        .map_err(|_| CryptoError::InvalidKey("Unable to derive Produce AI Key KEK".into()))?;
    let state = GlobalCryptoState {
        provider,
        produce_ai_key_kek: ApiKeyCrypto::from_bytes(produce_ai_key_kek.as_ref())?,
    };
    let _ = GLOBAL_CRYPTO.set(state);
    Ok(())
}

/// 获取全局加密器
///
/// 如果全局密钥未设置，返回 None
pub fn global_crypto() -> Option<&'static ApiKeyCrypto> {
    GLOBAL_CRYPTO.get().map(|state| &state.provider)
}

fn produce_ai_key_kek() -> Option<&'static ApiKeyCrypto> {
    GLOBAL_CRYPTO.get().map(|state| &state.produce_ai_key_kek)
}

/// 使用全局密钥加密 API Key
pub fn encrypt_api_key(plaintext: &str) -> Result<EncryptedApiKey, CryptoError> {
    let crypto = global_crypto().ok_or_else(|| {
        CryptoError::InvalidKey("Global crypto key not set. Call set_global_crypto() first.".into())
    })?;
    crypto.encrypt(plaintext)
}

/// 使用全局密钥解密 API Key
pub fn decrypt_api_key(encrypted: &EncryptedApiKey) -> Result<String, CryptoError> {
    let crypto = global_crypto().ok_or_else(|| {
        CryptoError::InvalidKey("Global crypto key not set. Call set_global_crypto() first.".into())
    })?;
    crypto.decrypt(encrypted)
}

/// 使用每条记录随机 DEK 加密 Produce AI Key，再由全局 KEK 包装 DEK。
pub fn encrypt_produce_ai_key(
    plaintext: &str,
    context: ProduceAiKeyEnvelopeContext,
) -> Result<ProduceAiKeyEnvelope, CryptoError> {
    let context = context.validate()?;
    let kek = produce_ai_key_kek().ok_or_else(|| {
        CryptoError::InvalidKey("Produce AI Key encryption key is not configured".into())
    })?;
    let mut dek = Zeroizing::new([0u8; KEY_SIZE]);
    OsRng.fill_bytes(dek.as_mut());
    let data_cipher = ApiKeyCrypto::from_bytes(dek.as_ref())?;
    let version = PRODUCE_AI_KEY_ENVELOPE_VERSION;
    let (ciphertext, nonce) =
        data_cipher.encrypt_bytes_with_aad(plaintext.as_bytes(), &context.aad("data", version))?;
    let (wrapped_dek, wrap_nonce) =
        kek.encrypt_bytes_with_aad(dek.as_ref(), &context.aad("dek", version))?;
    Ok(ProduceAiKeyEnvelope {
        ciphertext,
        nonce,
        wrapped_dek,
        wrap_nonce,
        kek_key_id: kek.key_id().to_owned(),
        encryption_version: version,
    })
}

/// 解包记录 DEK 并按不可变租户/所有者/Key 上下文认证解密。
pub fn decrypt_produce_ai_key(
    envelope: &ProduceAiKeyEnvelope,
    context: ProduceAiKeyEnvelopeContext,
) -> Result<DecryptedProduceAiKey, CryptoError> {
    let context = context.validate()?;
    if envelope.encryption_version != PRODUCE_AI_KEY_ENVELOPE_VERSION {
        return Err(CryptoError::UnsupportedEnvelopeVersion(
            envelope.encryption_version,
        ));
    }
    let kek = produce_ai_key_kek().ok_or_else(|| {
        CryptoError::InvalidKey("Produce AI Key encryption key is not configured".into())
    })?;
    if envelope.kek_key_id != kek.key_id() {
        return Err(CryptoError::KeyEncryptionKeyMismatch);
    }
    let mut dek = Zeroizing::new(kek.decrypt_bytes_with_aad(
        &envelope.wrapped_dek,
        &envelope.wrap_nonce,
        &context.aad("dek", envelope.encryption_version),
    )?);
    if dek.len() != KEY_SIZE {
        dek.zeroize();
        return Err(CryptoError::InvalidCiphertextFormat);
    }
    let data_cipher = ApiKeyCrypto::from_bytes(dek.as_ref())?;
    let plaintext = Zeroizing::new(data_cipher.decrypt_bytes_with_aad(
        &envelope.ciphertext,
        &envelope.nonce,
        &context.aad("data", envelope.encryption_version),
    )?);
    // Validate before copying into the returned zeroizing String. On failure,
    // the original decrypted byte buffer is still zeroized on scope exit.
    let value = std::str::from_utf8(plaintext.as_ref())
        .map_err(|_| CryptoError::DecryptionFailed("Invalid UTF-8 in decrypted text".into()))?
        .to_owned();
    Ok(DecryptedProduceAiKey(Zeroizing::new(value)))
}

fn hex_prefix(bytes: &[u8], chars: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(chars.min(bytes.len() * 2));
    for byte in bytes {
        if out.len() >= chars {
            break;
        }
        out.push(HEX[(byte >> 4) as usize] as char);
        if out.len() < chars {
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_key() {
        let key = ApiKeyCrypto::generate_key();
        assert!(!key.is_empty());

        // 验证可以创建加密器
        let crypto = ApiKeyCrypto::new(&key).expect("Failed to create crypto");
        let _ = format!("{:?}", crypto);
    }

    #[test]
    fn test_encrypt_decrypt() {
        let key = ApiKeyCrypto::generate_key();
        let crypto = ApiKeyCrypto::new(&key).expect("Failed to create crypto");

        let plaintext = "sk-proj-1234567890abcdefghijklmnopqrstuvwxyz";
        let encrypted = crypto.encrypt(plaintext).expect("Failed to encrypt");

        // 验证加密后的数据与明文不同
        assert_ne!(encrypted.as_str(), plaintext);

        // 验证可以解密
        let decrypted = crypto.decrypt(&encrypted).expect("Failed to decrypt");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encrypt_produces_different_ciphertext() {
        let key = ApiKeyCrypto::generate_key();
        let crypto = ApiKeyCrypto::new(&key).expect("Failed to create crypto");

        let plaintext = "sk-test-key";
        let encrypted1 = crypto.encrypt(plaintext).expect("Failed to encrypt");
        let encrypted2 = crypto.encrypt(plaintext).expect("Failed to encrypt");

        // 相同明文应产生不同密文（因为随机 nonce）
        assert_ne!(encrypted1.as_str(), encrypted2.as_str());

        // 但两个都能正确解密
        let decrypted1 = crypto.decrypt(&encrypted1).expect("Failed to decrypt");
        let decrypted2 = crypto.decrypt(&encrypted2).expect("Failed to decrypt");
        assert_eq!(decrypted1, plaintext);
        assert_eq!(decrypted2, plaintext);
    }

    #[test]
    fn test_create_preview() {
        // 正常长度的 API Key (18 字符)
        assert_eq!(
            ApiKeyCrypto::create_preview("sk-1234567890abcdef"),
            "sk-****def"
        );
        // 边界情况：长度正好 8
        assert_eq!(ApiKeyCrypto::create_preview("sk-abcde"), "sk-****cde");
        // 长度 <= 7 时，全部用 * 替代
        assert_eq!(ApiKeyCrypto::create_preview("sk-abc"), "******");
        assert_eq!(ApiKeyCrypto::create_preview("short"), "*****");
        assert_eq!(ApiKeyCrypto::create_preview("abc"), "***");
    }

    #[test]
    fn test_create_preview_multibyte_no_panic() {
        // 含多字节字符：必须按字符（而非字节）切片，绝不能在非字符边界 panic。
        // "🔑" 占 4 字节，若按字节切片 [..3] 会 panic。
        assert_eq!(ApiKeyCrypto::create_preview("🔑sk1234567"), "🔑sk****567");
        // 全中文，字符数 > 7（共 8 个字符）。
        assert_eq!(
            ApiKeyCrypto::create_preview("你好世界密钥请求"),
            "你好世****钥请求"
        );
        // 字符数 <= 7：全部用 * 替代（按字符计数，而非字节）。
        assert_eq!(ApiKeyCrypto::create_preview("🔑🔑🔑"), "***");
    }

    #[test]
    fn test_invalid_key_length() {
        let short_key = BASE64.encode([0u8; 16]);
        let result = ApiKeyCrypto::new(&short_key);
        assert!(result.is_err());
    }

    #[test]
    fn test_decrypt_invalid_ciphertext() {
        let key = ApiKeyCrypto::generate_key();
        let crypto = ApiKeyCrypto::new(&key).expect("Failed to create crypto");

        let invalid = EncryptedApiKey::from("invalid-base64!!!");
        let result = crypto.decrypt(&invalid);
        assert!(result.is_err());
    }

    #[test]
    fn test_encrypted_api_key_serde() {
        let encrypted = EncryptedApiKey::from("dGVzdC1lbmNyeXB0ZWQtZGF0YQ==");

        // Serialize
        let json = serde_json::to_string(&encrypted).expect("Failed to serialize");
        assert_eq!(json, "\"dGVzdC1lbmNyeXB0ZWQtZGF0YQ==\"");

        // Deserialize
        let deserialized: EncryptedApiKey =
            serde_json::from_str(&json).expect("Failed to deserialize");
        assert_eq!(deserialized, encrypted);
    }

    #[test]
    fn test_global_crypto() {
        let key = ApiKeyCrypto::generate_key();
        set_global_crypto(&key).expect("Failed to set global crypto");

        let plaintext = "sk-global-test-key";
        let encrypted = encrypt_api_key(plaintext).expect("Failed to encrypt");
        let decrypted = decrypt_api_key(&encrypted).expect("Failed to decrypt");
        assert_eq!(decrypted, plaintext);
        assert_ne!(
            global_crypto().unwrap().key_id(),
            produce_ai_key_kek().unwrap().key_id(),
            "Provider encryption and Produce AI Key wrapping require separate key domains"
        );
    }

    #[test]
    fn produce_key_envelope_round_trips_with_unique_record_keys() {
        set_global_crypto(&ApiKeyCrypto::generate_key()).unwrap();
        let context = ProduceAiKeyEnvelopeContext {
            tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            key_id: Uuid::new_v4(),
        };
        let plaintext = "sk-envelope-test-123456789012345678901234567890123";
        let first = encrypt_produce_ai_key(plaintext, context).unwrap();
        let second = encrypt_produce_ai_key(plaintext, context).unwrap();

        assert_ne!(first.ciphertext, second.ciphertext);
        assert_ne!(first.wrapped_dek, second.wrapped_dek);
        assert_ne!(first.nonce, second.nonce);
        assert_ne!(first.wrap_nonce, second.wrap_nonce);
        assert_eq!(
            decrypt_produce_ai_key(&first, context).unwrap().expose(),
            plaintext
        );
        assert!(!format!("{first:?}").contains(plaintext));
    }

    #[test]
    fn produce_key_envelope_rejects_tampering_and_context_reuse() {
        set_global_crypto(&ApiKeyCrypto::generate_key()).unwrap();
        let context = ProduceAiKeyEnvelopeContext {
            tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            key_id: Uuid::new_v4(),
        };
        let envelope = encrypt_produce_ai_key("sk-context-bound-secret", context).unwrap();
        let wrong_context = ProduceAiKeyEnvelopeContext {
            key_id: Uuid::new_v4(),
            ..context
        };
        assert!(decrypt_produce_ai_key(&envelope, wrong_context).is_err());

        let mut tampered = envelope.clone();
        let replacement = if tampered.ciphertext.starts_with('A') {
            "B"
        } else {
            "A"
        };
        tampered.ciphertext.replace_range(..1, replacement);
        assert!(decrypt_produce_ai_key(&tampered, context).is_err());

        let mut unsupported = envelope;
        unsupported.encryption_version += 1;
        assert!(matches!(
            decrypt_produce_ai_key(&unsupported, context),
            Err(CryptoError::UnsupportedEnvelopeVersion(_))
        ));
    }
}
