//! 提取器
//!
//! 自定义 Axum 提取器，用于从请求中提取认证信息等

use crate::{
    error::{ApiError, Result},
    state::AppState,
};
use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, request::Parts},
};
use keycompute_auth::{AuthContext, Permission};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::Arc;
use uuid::Uuid;

/// 认证提取器
///
/// 从请求头中提取 API Key 并解析用户信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthExtractor {
    /// 用户 ID
    pub user_id: Uuid,
    /// 租户 ID
    pub tenant_id: Uuid,
    /// Produce AI Key ID
    pub produce_ai_key_id: Uuid,
    /// 用户角色
    pub role: String,
    /// 用户权限列表
    pub permissions: Vec<Permission>,
}

impl AuthExtractor {
    /// 创建新的认证提取器（用于测试）
    pub fn new(
        user_id: Uuid,
        tenant_id: Uuid,
        produce_ai_key_id: Uuid,
        role: impl Into<String>,
    ) -> Self {
        Self {
            user_id,
            tenant_id,
            produce_ai_key_id,
            role: role.into(),
            permissions: Vec::new(),
        }
    }

    /// 创建带权限的认证提取器（用于测试）
    pub fn with_permissions(mut self, permissions: Vec<Permission>) -> Self {
        self.permissions = permissions;
        self
    }

    /// 从 Authorization 或 x-api-key 头和 AuthService 解析
    ///
    /// 支持两种认证方式：
    /// 1. Authorization: Bearer <token>  (OpenAI 风格)
    /// 2. x-api-key: <token>             (Anthropic 风格)
    pub async fn from_header_with_auth(
        headers: &HeaderMap,
        auth_service: &keycompute_auth::AuthService,
    ) -> Result<Self> {
        // 优先尝试 x-api-key (Anthropic 风格)
        let token = if let Some(api_key) = headers.get("x-api-key") {
            api_key
                .to_str()
                .map_err(|_| ApiError::Auth("Invalid x-api-key format".to_string()))?
                .to_string()
        } else if let Some(auth_header) = headers.get("Authorization") {
            // 回退到 Authorization: Bearer (OpenAI 风格)
            let auth_str = auth_header
                .to_str()
                .map_err(|_| ApiError::Auth("Invalid Authorization format".to_string()))?;

            auth_str
                .strip_prefix("Bearer ")
                .ok_or_else(|| ApiError::Auth("Invalid Authorization format, expected 'Bearer <token>'".to_string()))?
                .to_string()
        } else {
            return Err(ApiError::Auth(
                "Missing authentication header. Provide either 'Authorization: Bearer <token>' or 'x-api-key: <token>'".to_string()
            ));
        };

        // 使用 AuthService 验证 Token（自动检测 JWT 或 API Key）
        let auth_context = auth_service
            .verify_token(&token)
            .await
            .map_err(|e| ApiError::Auth(format!("Authentication failed: {}", e)))?;

        Ok(Self::from_auth_context(auth_context))
    }

    /// 从 AuthContext 创建
    pub fn from_auth_context(ctx: AuthContext) -> Self {
        Self {
            user_id: ctx.user_id,
            tenant_id: ctx.tenant_id,
            produce_ai_key_id: ctx.produce_ai_key_id,
            role: ctx.role,
            permissions: ctx.permissions,
        }
    }

    /// 检查是否是管理员
    pub fn is_admin(&self) -> bool {
        self.role == "admin" || self.role == "system"
    }
}

impl FromRequestParts<AppState> for AuthExtractor {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> impl Future<Output = std::result::Result<Self, Self::Rejection>> + Send {
        let auth_service = Arc::clone(&state.auth);
        let headers = parts.headers.clone();

        async move { Self::from_header_with_auth(&headers, &auth_service).await }
    }
}

/// 请求 ID 提取器
#[derive(Debug, Clone)]
pub struct RequestId(pub Uuid);

impl RequestId {
    /// 创建新的请求 ID
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> FromRequestParts<S> for RequestId
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        // 尝试从 X-Request-ID 头获取，否则生成新的
        let id = parts
            .headers
            .get("X-Request-ID")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(Uuid::new_v4);

        Ok(Self(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[tokio::test]
    async fn test_auth_extractor_from_header_valid_format() {
        // 测试格式正确的 API Key（无数据库连接时会失败）
        // 这是预期行为：生产环境需要数据库连接
        let mut headers = HeaderMap::new();
        let api_key = keycompute_auth::ProduceAiKeyValidator::generate_key();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", api_key)).unwrap(),
        );

        let auth_service =
            keycompute_auth::AuthService::new(keycompute_auth::ProduceAiKeyValidator::default());
        let result = AuthExtractor::from_header_with_auth(&headers, &auth_service).await;

        // 无数据库连接时应该返回配置错误
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ApiError::Auth(_)));
        assert!(err.to_string().contains("not properly configured"));
    }

    #[test]
    fn test_api_key_format_validation() {
        // 测试 API Key 格式验证
        let api_key = keycompute_auth::ProduceAiKeyValidator::generate_key();
        assert!(keycompute_auth::ProduceAiKeyValidator::is_valid_format(
            &api_key
        ));

        // 测试带前缀的格式
        let prefixed_key = keycompute_auth::ProduceAiKeyValidator::generate_key_with_prefix("proj");
        assert!(keycompute_auth::ProduceAiKeyValidator::is_valid_format(
            &prefixed_key
        ));

        // 测试无效格式
        assert!(!keycompute_auth::ProduceAiKeyValidator::is_valid_format(
            "invalid-key"
        ));
        assert!(!keycompute_auth::ProduceAiKeyValidator::is_valid_format(
            "sk-short"
        ));
    }

    #[tokio::test]
    async fn test_auth_extractor_from_header_missing() {
        let headers = HeaderMap::new();
        let auth_service =
            keycompute_auth::AuthService::new(keycompute_auth::ProduceAiKeyValidator::default());
        let result = AuthExtractor::from_header_with_auth(&headers, &auth_service).await;
        assert!(matches!(result, Err(ApiError::Auth(_))));
    }

    #[tokio::test]
    async fn test_auth_extractor_from_header_invalid_format() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        );

        let auth_service =
            keycompute_auth::AuthService::new(keycompute_auth::ProduceAiKeyValidator::default());
        let result = AuthExtractor::from_header_with_auth(&headers, &auth_service).await;
        assert!(matches!(result, Err(ApiError::Auth(_))));
    }

    #[test]
    fn test_request_id_new() {
        let id = RequestId::new();
        assert_ne!(id.0, Uuid::nil());
    }
}
