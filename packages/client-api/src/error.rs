//! 客户端错误类型
//!
//! 定义 client-api 使用的统一错误类型，处理 HTTP 请求、JSON 解析、网络等错误

use thiserror::Error;

/// Client API 错误类型
#[derive(Error, Debug, Clone)]
pub enum ClientError {
    /// HTTP 请求错误
    #[error("HTTP request failed: {0}")]
    Http(String),

    /// 序列化/反序列化错误
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// 网络连接错误
    #[error("Network error: {0}")]
    Network(String),

    /// 未认证 (401)
    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    /// 禁止访问 (403)
    #[error("Forbidden: {0}")]
    Forbidden(String),

    /// 资源不存在 (404)
    #[error("Not found: {0}")]
    NotFound(String),

    /// 请求过多，触发限流 (429)
    #[error("Rate limited: {0}")]
    RateLimited(String),

    /// 验证流程错误 (422)
    #[error("Verification failed: {0}")]
    Verification(String),

    /// 服务维护中 (503)
    #[error("Service unavailable: {0}")]
    ServiceUnavailable(String),

    /// 服务器内部错误 (500)
    #[error("Server error: {0}")]
    ServerError(String),

    /// 无效响应
    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    /// 配置错误
    #[error("Configuration error: {0}")]
    Config(String),

    /// 其他错误
    #[error("Other error: {0}")]
    Other(String),
}

/// Client API 结果类型
pub type Result<T> = std::result::Result<T, ClientError>;

impl ClientError {
    /// 根据 HTTP 状态码创建对应的错误
    pub fn from_status(status: u16, message: impl Into<String>) -> Self {
        let msg = extract_error_message(message.into());
        match status {
            401 => ClientError::Unauthorized(msg),
            403 => ClientError::Forbidden(msg),
            404 => ClientError::NotFound(msg),
            429 => ClientError::RateLimited(msg),
            422 => ClientError::Verification(msg),
            503 => ClientError::ServiceUnavailable(msg),
            500..=599 => ClientError::ServerError(msg),
            _ => ClientError::Http(format!("HTTP {}: {}", status, msg)),
        }
    }

    /// 判断是否为认证相关错误
    pub fn is_auth_error(&self) -> bool {
        matches!(self, ClientError::Unauthorized(_))
    }

    /// 判断是否为限流错误
    pub fn is_rate_limited(&self) -> bool {
        matches!(self, ClientError::RateLimited(_))
    }

    /// 判断是否为验证码/验证流程错误
    pub fn is_verification_error(&self) -> bool {
        matches!(self, ClientError::Verification(_))
    }

    /// 判断是否为网络错误
    pub fn is_network_error(&self) -> bool {
        matches!(self, ClientError::Network(_) | ClientError::Http(_))
    }

    /// 获取适合直接展示给用户的错误消息文本。
    ///
    /// 该方法会移除枚举 `Display` 中的英文包装层，优先保留后端返回的业务消息。
    pub fn message(&self) -> String {
        match self {
            ClientError::Http(msg) => strip_http_prefix(msg),
            ClientError::Serialization(msg)
            | ClientError::Network(msg)
            | ClientError::Unauthorized(msg)
            | ClientError::Forbidden(msg)
            | ClientError::NotFound(msg)
            | ClientError::RateLimited(msg)
            | ClientError::Verification(msg)
            | ClientError::ServiceUnavailable(msg)
            | ClientError::ServerError(msg)
            | ClientError::InvalidResponse(msg)
            | ClientError::Config(msg)
            | ClientError::Other(msg) => msg.clone(),
        }
    }
}

impl From<reqwest::Error> for ClientError {
    fn from(err: reqwest::Error) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        if err.is_connect() || err.is_timeout() {
            return ClientError::Network(err.to_string());
        }
        if err.is_status() {
            if let Some(status) = err.status() {
                ClientError::from_status(status.as_u16(), err.to_string())
            } else {
                ClientError::Http(err.to_string())
            }
        } else {
            ClientError::Http(err.to_string())
        }
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(err: serde_json::Error) -> Self {
        ClientError::Serialization(err.to_string())
    }
}

impl From<std::io::Error> for ClientError {
    fn from(err: std::io::Error) -> Self {
        ClientError::Network(err.to_string())
    }
}

fn extract_error_message(raw: String) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return raw;
    };

    value
        .get("error")
        .and_then(|error| match error {
            serde_json::Value::String(msg) => Some(msg.as_str()),
            serde_json::Value::Object(map) => map.get("message").and_then(|msg| msg.as_str()),
            _ => None,
        })
        .or_else(|| value.get("message").and_then(|msg| msg.as_str()))
        .map(ToString::to_string)
        .unwrap_or(raw)
}

fn strip_http_prefix(msg: &str) -> String {
    if let Some((_, rest)) = msg.split_once(": ") {
        rest.to_string()
    } else {
        msg.to_string()
    }
}
