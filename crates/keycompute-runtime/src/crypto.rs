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
    aead::{Aead, KeyInit},
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use rand::RngCore;
use std::fmt;
use thiserror::Error;

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
}

/// AES-256-GCM 密钥长度 (32 bytes)
const KEY_SIZE: usize = 32;

/// GCM Nonce 长度 (12 bytes, GCM 推荐值)
const NONCE_SIZE: usize = 12;

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
}

impl fmt::Debug for ApiKeyCrypto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKeyCrypto")
            .field("algorithm", &"AES-256-GCM")
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
        let key_bytes = BASE64.decode(secret_key)?;

        if key_bytes.len() != KEY_SIZE {
            return Err(CryptoError::InvalidKey(format!(
                "Key must be {} bytes, got {}",
                KEY_SIZE,
                key_bytes.len()
            )));
        }

        let key = aes_gcm::Key::<Aes256Gcm>::from_slice(&key_bytes);
        let cipher = Aes256Gcm::new(key);

        Ok(Self { cipher })
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

        Ok(Self { cipher })
    }

    /// 生成随机密钥
    ///
    /// 返回 Base64 编码的 32 字节密钥。
    /// **重要**: 此密钥应安全存储，丢失后将无法解密已加密的数据。
    pub fn generate_key() -> String {
        let mut key = [0u8; KEY_SIZE];
        rand::thread_rng().fill_bytes(&mut key);
        BASE64.encode(key)
    }

    /// 生成随机 Nonce
    fn generate_nonce() -> [u8; NONCE_SIZE] {
        let mut nonce = [0u8; NONCE_SIZE];
        rand::thread_rng().fill_bytes(&mut nonce);
        nonce
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
static GLOBAL_CRYPTO: std::sync::OnceLock<ApiKeyCrypto> = std::sync::OnceLock::new();

/// 设置全局加密密钥
///
/// 应在应用启动时调用一次。
///
/// # 参数
/// - `secret_key`: Base64 编码的 32 字节密钥
pub fn set_global_crypto(secret_key: &str) -> Result<(), CryptoError> {
    let crypto = ApiKeyCrypto::new(secret_key)?;
    let _ = GLOBAL_CRYPTO.set(crypto);
    Ok(())
}

/// 获取全局加密器
///
/// 如果全局密钥未设置，返回 None
pub fn global_crypto() -> Option<&'static ApiKeyCrypto> {
    GLOBAL_CRYPTO.get()
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
    }
}
