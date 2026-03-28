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
        let msg = message.into();
        match status {
            401 => ClientError::Unauthorized(msg),
            403 => ClientError::Forbidden(msg),
            404 => ClientError::NotFound(msg),
            429 => ClientError::RateLimited(msg),
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

    /// 判断是否为网络错误
    pub fn is_network_error(&self) -> bool {
        matches!(self, ClientError::Network(_) | ClientError::Http(_))
    }
}

impl From<reqwest::Error> for ClientError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_connect() || err.is_timeout() {
            ClientError::Network(err.to_string())
        } else if err.is_status() {
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
