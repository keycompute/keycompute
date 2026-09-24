//! 公共响应类型
//!
//! 供多个 API 模块复用的通用数据结构

use serde::Deserialize;

/// 通用消息响应
///
/// 后端返回 `{ "message": "..." }` 格式的接口统一使用此类型。
#[derive(Debug, Clone, Deserialize)]
pub struct MessageResponse {
    pub message: String,
}

/// 对 URL 查询参数字符串进行百分比编码
///
/// 使用 `percent-encoding` 标准对参数内容编码，防止特殊字符导致 URL 解析错误。
pub fn encode_query_value(value: &str) -> String {
    // 对除字母、数字、“-_.~” 之外的字符进行百分比编码
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            b => {
                encoded.push('%');
                encoded.push_str(&format!("{:02X}", b));
            }
        }
    }
    encoded
}

/// Never return reflected secret-bearing failures to a client application.
pub(crate) fn one_time_key_error(error: crate::ClientError) -> crate::ClientError {
    use crate::ClientError;
    let message="The one-time key operation could not be confirmed. Refresh issuance and key records before requesting another key.".to_owned();
    match error {
        ClientError::Unauthorized(_) => ClientError::Unauthorized(message),
        ClientError::Forbidden(_) => ClientError::Forbidden(message),
        ClientError::NotFound(_) => ClientError::NotFound(message),
        ClientError::RateLimited(mut info) => {
            info.message = message;
            info.scope = None;
            ClientError::RateLimited(info)
        }
        ClientError::ServiceUnavailable(_) => ClientError::ServiceUnavailable(message),
        ClientError::ServerError(_) => ClientError::ServerError(message),
        ClientError::Network(_) => ClientError::Network(message),
        _ => ClientError::Other(message),
    }
}

pub(crate) fn valid_one_time_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}
