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
use chrono::{DateTime, Utc};
use keycompute_auth::{AuthContext, Permission, PermissionChecker};
use keycompute_types::{CredentialKind, PlatformRole, TenantRole};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::future::Future;
use std::sync::Arc;
use uuid::Uuid;

/// Authentication storage failures are temporary unavailability, not invalid
/// credentials, and must not reveal database errors to an unauthenticated caller.
pub(crate) fn authentication_error(error: keycompute_types::KeyComputeError) -> ApiError {
    match error {
        keycompute_types::KeyComputeError::DatabaseError(_)
        | keycompute_types::KeyComputeError::ServiceUnavailable(_) => {
            ApiError::ServiceUnavailable("Authentication service is temporarily unavailable".into())
        }
        error => ApiError::from(error),
    }
}

const ACTIVE_NODE_SESSION_TOKEN_QUERY: &str = "SELECT ns.node_id, ns.id FROM node_sessions ns \
     INNER JOIN nodes n ON n.id = ns.node_id \
     INNER JOIN tenants t ON t.id = n.tenant_id \
     INNER JOIN tenant_memberships m ON m.tenant_id=n.tenant_id AND m.user_id=n.owner_user_id AND m.status='active' \
     INNER JOIN users u ON u.id=n.owner_user_id AND u.status='active' \
     WHERE ns.session_token_hash = $1 \
       AND ns.revoked_at IS NULL \
       AND ns.expires_at > NOW() AND ns.accepting_tasks=TRUE \
       AND t.status = 'active'";

// Completion of a task that was already leased is allowed to drain after an
// administrator closes the owning tenant.  The task/lease/session checks in
// NodeGatewayStore still enforce the authenticated node identity and expiry;
// this query only omits the lifecycle gate that would otherwise reject the
// in-flight result before the handler can apply those checks.
const NODE_SESSION_COMPLETION_TOKEN_QUERY: &str = "SELECT ns.node_id, ns.id FROM node_sessions ns \
     INNER JOIN nodes n ON n.id = ns.node_id \
     INNER JOIN tenants t ON t.id = n.tenant_id \
     WHERE ns.session_token_hash = $1 \
       AND ns.revoked_at IS NULL \
       AND ns.expires_at > NOW()";

/// 认证提取器
///
/// 从请求头中提取 JWT 或 API Key，并解析用户信息与权限
#[derive(Debug, Clone, Serialize)]
pub struct AuthExtractor {
    /// 用户 ID
    pub user_id: Uuid,
    /// 租户 ID
    pub tenant_id: Uuid,
    /// Verified platform scope.
    pub platform_role: PlatformRole,
    /// Verified role in tenant_id.
    pub tenant_role: Option<TenantRole>,
    /// Credential kind used for this request.
    pub credential_kind: CredentialKind,
    /// Produce AI Key ID
    pub produce_ai_key_id: Uuid,
    /// 用户权限列表
    pub permissions: Vec<Permission>,
    pub token_version: i32,
    pub membership_authz_version: i64,
    pub authz_version: i64,
    /// A resource permit, not an authorization decision; never serialized.
    #[serde(skip)]
    pub generation_permit: Option<keycompute_runtime::admission::AdmissionPermit>,
}

impl AuthExtractor {
    /// 创建新的认证提取器（用于测试）
    pub fn new(
        user_id: Uuid,
        tenant_id: Uuid,
        produce_ai_key_id: Uuid,
        credential_kind: CredentialKind,
    ) -> Self {
        Self {
            user_id,
            tenant_id,
            platform_role: PlatformRole::None,
            tenant_role: None,
            credential_kind,
            produce_ai_key_id,
            permissions: Vec::new(),
            token_version: 0,
            membership_authz_version: 1,
            authz_version: 1,
            generation_permit: None,
        }
    }

    /// 创建带权限的认证提取器（用于测试）
    pub fn with_permissions(mut self, permissions: Vec<Permission>) -> Self {
        self.permissions = permissions;
        self
    }

    /// 从 `Authorization: Bearer` 头和 `AuthService` 解析。
    ///
    /// HTTP 提取器另外支持 Anthropic 的 `x-api-key` 约定，但只允许在
    /// `/v1/messages` 路径使用；这个无路径辅助函数保持 Bearer 调用方兼容。
    pub async fn from_header_with_auth(
        headers: &HeaderMap,
        auth_service: &keycompute_auth::AuthService,
    ) -> Result<Self> {
        Self::from_header_with_auth_for_path(headers, auth_service, None).await
    }

    /// Parse authentication for an HTTP request path.
    ///
    /// Anthropic clients send API keys in `x-api-key`; that transport-level
    /// convention must not accidentally grant access to the dashboard and
    /// user-management APIs, which also use this extractor. Keep the legacy
    /// header-only helper available for callers that already pass Bearer
    /// tokens, while the Axum extractor supplies the actual request path.
    async fn from_header_with_auth_for_path(
        headers: &HeaderMap,
        auth_service: &keycompute_auth::AuthService,
        path: Option<&str>,
    ) -> Result<Self> {
        let token = if let Some(auth_header) =
            headers.get("Authorization").and_then(|h| h.to_str().ok())
        {
            auth_header
                .strip_prefix("Bearer ")
                .ok_or_else(|| ApiError::Auth("Invalid Authorization format".to_string()))?
        } else {
            if !matches!(
                path,
                Some("/v1/messages" | "/pt/v1/messages" | "/nt/v1/messages")
            ) {
                return Err(ApiError::Auth(
                    "x-api-key authentication is only valid for Messages API paths".to_string(),
                ));
            }
            headers
                .get("x-api-key")
                .and_then(|h| h.to_str().ok())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ApiError::Auth("Missing Authorization Bearer or x-api-key header".to_string())
                })?
        };

        // 使用 AuthService 验证 Token（自动检测 JWT 或 API Key）
        // 注意：通过 From 转换而非手动拼接前缀，避免与 KeyComputeError 的
        // Display 前缀（"authentication failed: "）叠加产生重复文案
        let auth_context = auth_service
            .verify_token(token)
            .await
            .map_err(authentication_error)?;

        Self::from_auth_context(auth_context)
    }

    /// 从 AuthContext 创建
    pub fn from_auth_context(ctx: AuthContext) -> Result<Self> {
        Ok(Self {
            user_id: ctx.user_id,
            tenant_id: ctx.selected_tenant_id.ok_or_else(|| {
                ApiError::Auth("tenant selection required for inference".to_string())
            })?,
            platform_role: ctx.platform_role,
            tenant_role: ctx.tenant_role,
            credential_kind: ctx.credential_kind,
            produce_ai_key_id: ctx.produce_ai_key_id,
            permissions: ctx.permissions,
            token_version: ctx.token_version,
            membership_authz_version: ctx
                .membership_authz_version
                .ok_or_else(|| ApiError::Auth("membership version required".into()))?,
            authz_version: ctx
                .authz_version
                .ok_or_else(|| ApiError::Auth("tenant version required".into()))?,
            generation_permit: None,
        })
    }

    pub fn authorization_context(&self) -> AuthContext {
        AuthContext {
            user_id: self.user_id,
            selected_tenant_id: Some(self.tenant_id),
            platform_role: self.platform_role,
            tenant_role: self.tenant_role,
            credential_kind: self.credential_kind,
            produce_ai_key_id: self.produce_ai_key_id,
            permissions: self.permissions.clone(),
            token_version: self.token_version,
            membership_authz_version: Some(self.membership_authz_version),
            authz_version: Some(self.authz_version),
            user_info: None,
            tenant_info: None,
        }
    }
    pub fn require_platform(
        &self,
        action: keycompute_auth::AuthorizationAction,
    ) -> Result<keycompute_types::PlatformScope> {
        self.authorization_context()
            .require_platform(action)
            .map_err(ApiError::from)
    }
    pub fn require_tenant(
        &self,
        action: keycompute_auth::AuthorizationAction,
    ) -> Result<keycompute_types::TenantScope> {
        self.authorization_context()
            .require_tenant(action)
            .map_err(ApiError::from)
    }
    pub fn require_owner(
        &self,
        owner: Uuid,
        action: keycompute_auth::AuthorizationAction,
    ) -> Result<keycompute_types::TenantScope> {
        self.authorization_context()
            .require_owner(owner, action)
            .map_err(ApiError::from)
    }

    /// 使用认证阶段根据 AuthType 构建的权限集合做授权判断。
    /// API Key 即使归属于 admin 用户，也不会因 role 字符串获得后台权限。
    pub fn has_permission(&self, permission: &Permission) -> bool {
        PermissionChecker::check(self.credential_kind, &self.permissions, permission)
    }
}

impl FromRequestParts<AppState> for AuthExtractor {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> impl Future<Output = std::result::Result<Self, Self::Rejection>> + Send {
        let cached_auth = parts.extensions.get::<Self>().cloned();
        let auth_service = Arc::clone(&state.auth);
        let headers = parts.headers.clone();
        let path = parts.uri.path().to_string();

        async move {
            if let Some(auth) = cached_auth {
                return Ok(auth);
            }
            Self::from_header_with_auth_for_path(&headers, &auth_service, Some(&path)).await
        }
    }
}

/// Console-only identity. A validated inference API key is not a console session.
/// Kept separate from AuthExtractor so self-service handlers remain protected
/// even when mounted without the application's outer console middleware.
#[derive(Debug, Clone)]
pub struct ConsoleAuth(AuthExtractor);

impl std::ops::Deref for ConsoleAuth {
    type Target = AuthExtractor;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<AuthExtractor> for ConsoleAuth {
    type Error = ApiError;

    fn try_from(auth: AuthExtractor) -> Result<Self> {
        if auth.credential_kind != CredentialKind::Jwt
            || !auth.has_permission(&Permission::AccessConsole)
        {
            return Err(ApiError::Forbidden("Console session required".to_string()));
        }
        Ok(Self(auth))
    }
}

impl FromRequestParts<AppState> for ConsoleAuth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self> {
        Self::try_from(AuthExtractor::from_request_parts(parts, state).await?)
    }
}

/// Global console identity. It intentionally has no tenant requirement and is
/// used for profile, membership listing, tenant selection, and invitations.
/// Tenant-scoped handlers must continue to extract [`ConsoleAuth`].
#[derive(Debug, Clone)]
pub struct GlobalConsoleAuth(pub AuthContext);

impl std::ops::Deref for GlobalConsoleAuth {
    type Target = AuthContext;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<AuthContext> for GlobalConsoleAuth {
    type Error = ApiError;
    fn try_from(ctx: AuthContext) -> Result<Self> {
        if ctx.credential_kind != CredentialKind::Jwt
            || !ctx.has_permission(&Permission::AccessConsole)
        {
            return Err(ApiError::Forbidden(
                "Global console session required".to_string(),
            ));
        }
        Ok(Self(ctx))
    }
}

impl FromRequestParts<AppState> for GlobalConsoleAuth {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self> {
        if let Some(auth) = parts.extensions.get::<Self>().cloned() {
            return Ok(auth);
        }
        if let Some(ctx) = parts.extensions.get::<AuthContext>().cloned() {
            return Self::try_from(ctx);
        }
        if let Some(auth) = parts.extensions.get::<AuthExtractor>() {
            return Self::try_from(auth.authorization_context());
        }
        let value = parts
            .headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::Auth("Authentication required".into()))?;
        let token = value
            .strip_prefix("Bearer ")
            .ok_or_else(|| ApiError::Auth("Invalid Authorization format".into()))?;
        let ctx = state
            .auth
            .verify_token(token)
            .await
            .map_err(authentication_error)?;
        Self::try_from(ctx)
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
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        parts.extensions.get::<RequestId>().cloned().ok_or_else(|| {
            ApiError::Internal("canonical request ID middleware is not installed".to_string())
        })
    }
}

/// Timestamp captured when the request first enters the application middleware stack.
#[derive(Debug, Clone)]
pub struct RequestReceivedAt(pub DateTime<Utc>);

impl<S> FromRequestParts<S> for RequestReceivedAt
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<RequestReceivedAt>()
            .cloned()
            .ok_or_else(|| {
                ApiError::Internal(
                    "request ingress timestamp middleware is not installed".to_string(),
                )
            })
    }
}

/// Validated client-supplied correlation ID. It never participates in internal joins.
#[derive(Debug, Clone, Default)]
pub struct ClientRequestId(pub Option<String>);

impl<S> FromRequestParts<S> for ClientRequestId
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        Ok(parts
            .extensions
            .get::<ClientRequestId>()
            .cloned()
            .unwrap_or_default())
    }
}

/// 节点会话认证提取器
///
/// 从 Authorization header 中提取 session token 并验证
/// 认证成功后返回 node_id 和 session_id
pub struct NodeSessionAuth {
    /// 节点 ID
    pub node_id: Uuid,
    /// 会话 ID
    pub session_id: Uuid,
}

impl FromRequestParts<AppState> for NodeSessionAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let (node_id, session_id) =
            authenticate_node_session(parts, state, ACTIVE_NODE_SESSION_TOKEN_QUERY).await?;
        Ok(Self {
            node_id,
            session_id,
        })
    }
}

/// Authentication extractor for a node submitting a result for an already
/// leased task.  Unlike [`NodeSessionAuth`], it permits an inactive owning
/// tenant so in-flight work can be finalized, while retaining session expiry
/// and revocation checks.
pub struct NodeSessionCompletionAuth {
    /// 节点 ID
    pub node_id: Uuid,
    /// 会话 ID
    pub session_id: Uuid,
}

impl FromRequestParts<AppState> for NodeSessionCompletionAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let (node_id, session_id) =
            authenticate_node_session(parts, state, NODE_SESSION_COMPLETION_TOKEN_QUERY).await?;
        Ok(Self {
            node_id,
            session_id,
        })
    }
}

async fn authenticate_node_session(
    parts: &Parts,
    state: &AppState,
    query: &str,
) -> Result<(Uuid, Uuid)> {
    let auth_header = parts
        .headers
        .get("Authorization")
        .ok_or_else(|| ApiError::Auth("Missing authorization header".to_string()))?;
    let token = auth_header
        .to_str()
        .map_err(|_| ApiError::Auth("Invalid authorization header".to_string()))?
        .strip_prefix("Bearer ")
        .ok_or_else(|| ApiError::Auth("Invalid bearer token".to_string()))?;
    let token_hash = compute_sha256_hash(token);
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database pool not configured".to_string()))?;

    // Session validity is security-sensitive and must be writer-fresh. A
    // lagging replica could otherwise continue accepting a revoked or expired
    // session (or reject a newly issued session).
    let row = pool
        .write_conn()
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            query,
            [token_hash.as_str().into()],
        ))
        .await
        .map_err(|e| ApiError::Internal(format!("Database query failed: {e}")))?
        .ok_or_else(|| ApiError::Auth("Invalid session token".to_string()))?;
    let node_id: Uuid = row
        .try_get_by_index(0)
        .map_err(|e| ApiError::Internal(format!("Failed to parse node_id: {e}")))?;
    let session_id: Uuid = row
        .try_get_by_index(1)
        .map_err(|e| ApiError::Internal(format!("Failed to parse session_id: {e}")))?;
    Ok((node_id, session_id))
}

/// 计算 SHA-256 hash
fn compute_sha256_hash(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)
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
        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
        assert!(err.to_string().contains("temporarily unavailable"));
        assert!(!err.to_string().contains("not properly configured"));
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

    #[tokio::test]
    async fn auth_extractor_reuses_server_validated_extension() {
        let expected = AuthExtractor::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            CredentialKind::Jwt,
        )
        .with_permissions(vec![Permission::UseApi]);
        let request = axum::http::Request::builder()
            .uri("/v1/responses")
            .extension(expected.clone())
            .body(())
            .unwrap();
        let (mut parts, _) = request.into_parts();

        let actual = AuthExtractor::from_request_parts(&mut parts, &AppState::new())
            .await
            .unwrap();

        assert_eq!(actual.user_id, expected.user_id);
        assert_eq!(actual.tenant_id, expected.tenant_id);
        assert_eq!(actual.produce_ai_key_id, expected.produce_ai_key_id);
        assert_eq!(actual.permissions, expected.permissions);
    }

    #[test]
    fn node_session_token_lookup_rejects_expired_sessions() {
        assert!(ACTIVE_NODE_SESSION_TOKEN_QUERY.contains("revoked_at IS NULL"));
        assert!(ACTIVE_NODE_SESSION_TOKEN_QUERY.contains("expires_at > NOW()"));
        assert!(ACTIVE_NODE_SESSION_TOKEN_QUERY.contains("t.status = 'active'"));
    }

    #[test]
    fn node_completion_lookup_allows_draining_inactive_tenants() {
        assert!(NODE_SESSION_COMPLETION_TOKEN_QUERY.contains("revoked_at IS NULL"));
        assert!(NODE_SESSION_COMPLETION_TOKEN_QUERY.contains("expires_at > NOW()"));
        assert!(!NODE_SESSION_COMPLETION_TOKEN_QUERY.contains("t.status = 'active'"));
    }

    #[tokio::test]
    async fn x_api_key_is_rejected_outside_anthropic_messages_path() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("sk-test-key"));
        let auth_service =
            keycompute_auth::AuthService::new(keycompute_auth::ProduceAiKeyValidator::default());

        let result = AuthExtractor::from_header_with_auth_for_path(
            &headers,
            &auth_service,
            Some("/api/v1/me"),
        )
        .await;

        assert!(matches!(result, Err(ApiError::Auth(message)) if message.contains("only valid")));
    }

    #[test]
    fn test_request_id_new() {
        let id = RequestId::new();
        assert_ne!(id.0, Uuid::nil());
    }

    #[test]
    fn api_keys_never_get_console_permission() {
        let auth = AuthExtractor::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            CredentialKind::Jwt,
        );
        assert!(!auth.has_permission(&Permission::AccessConsole));
        let console = auth.with_permissions(vec![Permission::AccessConsole]);
        assert!(ConsoleAuth::try_from(console).is_ok());
    }

    #[tokio::test]
    async fn console_extractor_protects_handlers_even_without_outer_middleware() {
        use axum::{
            Router,
            body::Body,
            http::{Request, StatusCode},
            routing::get,
        };
        use tower::ServiceExt;
        let state = AppState::default();
        let app = Router::new()
            .route(
                "/standalone-self-service",
                get(crate::handlers::user::get_current_user),
            )
            .with_state(state);
        for platform in [
            PlatformRole::None,
            PlatformRole::Operator,
            PlatformRole::Root,
        ] {
            let mut request = Request::builder()
                .uri("/standalone-self-service")
                .body(Body::empty())
                .unwrap();
            let _ = platform; // Ownership role cannot change credential purpose.
            request.extensions_mut().insert(
                AuthExtractor::new(
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    CredentialKind::ApiKey,
                )
                .with_permissions(vec![Permission::UseApi]),
            );
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::FORBIDDEN
            );
        }
    }
}
