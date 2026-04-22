//! 认证处理器
//!
//! 处理用户注册、登录和密码重置等认证相关的 HTTP 请求

use crate::{
    error::{ApiError, Result},
    handlers::configured_public_base_url,
    middleware::extract_client_ip_from_headers,
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use keycompute_auth::{
    CompleteRegistrationRequest, LoginRequest, PasswordResetService, RegistrationService,
    RequestPasswordResetRequest, RequestRegistrationCodeRequest, ResetPasswordRequest,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const FORGOT_PASSWORD_IDENTITY_ERROR: &str = "邮箱地址或用户名错误，无法发送重置密码链接";

// ============================================================================
// 请求/响应类型
// ============================================================================

/// 注册请求
#[derive(Debug, Deserialize)]
pub struct RegisterRequestJson {
    pub email: String,
    pub referral_code: Option<String>,
}

/// 完成注册请求
#[derive(Debug, Deserialize)]
pub struct CompleteRegistrationRequestJson {
    pub email: String,
    pub code: String,
    pub password: String,
    pub name: Option<String>,
}

/// 登录请求
#[derive(Debug, Deserialize)]
pub struct LoginRequestJson {
    pub email: String,
    pub password: String,
}

/// 忘记密码请求
#[derive(Debug, Deserialize)]
pub struct ForgotPasswordRequestJson {
    pub name: String,
    pub email: String,
}

/// 重置密码请求
#[derive(Debug, Deserialize)]
pub struct ResetPasswordRequestJson {
    pub token: String,
    pub new_password: String,
}

/// 通用消息响应
#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub message: String,
}

/// 验证令牌响应
#[derive(Debug, Serialize)]
pub struct VerifyTokenResponse {
    pub valid: bool,
    pub user_id: Option<String>,
}

// ============================================================================
// 处理器函数
// ============================================================================

/// 用户注册
///
/// POST /auth/register
pub async fn register_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RegisterRequestJson>,
) -> Result<impl IntoResponse> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;

    let register_req = RequestRegistrationCodeRequest {
        email: req.email,
        referral_code: req.referral_code,
    };

    let service = RegistrationService::new(Arc::clone(pool))
        .with_email_service((*state.email_service).clone());
    let response = service
        .request_registration_code(&register_req, extract_client_ip_from_headers(&headers))
        .await
        .map_err(ApiError::from)?;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "email": response.email,
            "message": response.message,
            "expires_in_seconds": response.expires_in_seconds
        })),
    ))
}

/// 完成注册
///
/// POST /auth/register/complete
pub async fn complete_registration_handler(
    State(state): State<AppState>,
    Json(req): Json<CompleteRegistrationRequestJson>,
) -> Result<impl IntoResponse> {
    use keycompute_db::models::system_setting::setting_keys;

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;
    let default_quota_setting =
        keycompute_db::SystemSetting::find_by_key(pool, setting_keys::DEFAULT_USER_QUOTA)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to query default user quota: {}", e)))?
            .ok_or_else(|| {
                ApiError::Config("Missing system setting: default_user_quota".to_string())
            })?;
    let default_quota = default_quota_setting.parse_decimal().map_err(|e| {
        ApiError::Config(format!("Invalid system setting default_user_quota: {}", e))
    })?;
    if !default_quota.is_finite() {
        return Err(ApiError::Config(
            "Invalid system setting default_user_quota: value must be finite".to_string(),
        ));
    }

    let complete_req = CompleteRegistrationRequest {
        email: req.email,
        code: req.code,
        password: req.password,
        name: req.name,
    };

    let service = RegistrationService::new(Arc::clone(pool))
        .with_email_service((*state.email_service).clone());
    let response = service
        .complete_registration(&complete_req, default_quota)
        .await
        .map_err(ApiError::from)?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "user_id": response.user_id.to_string(),
            "tenant_id": response.tenant_id.to_string(),
            "email": response.email,
            "message": response.message
        })),
    ))
}

/// 用户登录
///
/// POST /auth/login
pub async fn login_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LoginRequestJson>,
) -> Result<impl IntoResponse> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;

    let jwt_validator = state
        .auth
        .get_jwt_validator()
        .ok_or_else(|| ApiError::Internal("JWT not configured".into()))?
        .clone();

    let login_req = LoginRequest {
        email: req.email,
        password: req.password,
        client_ip: extract_client_ip_from_headers(&headers),
    };

    let service = keycompute_auth::LoginService::new(Arc::clone(pool), jwt_validator);
    let response = service.login(&login_req).await.map_err(|e| match e {
        keycompute_types::KeyComputeError::AuthError(msg) => ApiError::Auth(msg),
        _ => ApiError::Internal(format!("Login failed: {}", e)),
    })?;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "user_id": response.user_id.to_string(),
            "tenant_id": response.tenant_id.to_string(),
            "email": response.email,
            "role": response.role,
            "access_token": response.jwt_token,
            "token_type": "Bearer",
            "expires_in": response.expires_in
        })),
    ))
}

/// 忘记密码
///
/// POST /auth/forgot-password
pub async fn forgot_password_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ForgotPasswordRequestJson>,
) -> Result<impl IntoResponse> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;

    let service = PasswordResetService::new(Arc::clone(pool))
        .with_email_service((*state.email_service).clone());
    let public_base_url =
        configured_public_base_url(state.app_base_url.as_deref()).ok_or_else(|| {
            ApiError::Config("APP_BASE_URL is required to send password reset emails".to_string())
        })?;

    let reset_result = service
        .request_reset(&RequestPasswordResetRequest {
            name: req.name,
            email: req.email,
            client_ip: extract_client_ip_from_headers(&headers),
            public_base_url,
        })
        .await
        .map_err(ApiError::from)?;

    if reset_result.is_none() {
        return Err(ApiError::BadRequest(
            FORGOT_PASSWORD_IDENTITY_ERROR.to_string(),
        ));
    }

    Ok((
        StatusCode::OK,
        Json(MessageResponse {
            message: "If the account information matches, a reset link has been sent.".to_string(),
        }),
    ))
}

/// 重置密码
///
/// POST /auth/reset-password
pub async fn reset_password_handler(
    State(state): State<AppState>,
    Json(req): Json<ResetPasswordRequestJson>,
) -> Result<impl IntoResponse> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;

    let service = PasswordResetService::new(Arc::clone(pool));

    let reset_req = ResetPasswordRequest {
        token: req.token,
        new_password: req.new_password,
    };

    let user_id = service
        .reset_password(&reset_req)
        .await
        .map_err(|e| match e {
            keycompute_types::KeyComputeError::AuthError(msg) => ApiError::Auth(msg),
            keycompute_types::KeyComputeError::ValidationError(msg) => ApiError::BadRequest(msg),
            _ => ApiError::Internal(format!("Password reset failed: {}", e)),
        })?;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "message": "Password reset successfully",
            "user_id": user_id.to_string()
        })),
    ))
}

/// 验证重置令牌
///
/// GET /auth/verify-reset-token/:token
pub async fn verify_reset_token_handler(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<impl IntoResponse> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;

    let service = PasswordResetService::new(Arc::clone(pool));
    let valid = service
        .verify_token(&token)
        .await
        .map_err(|e| ApiError::Internal(format!("Token verification failed: {}", e)))?;

    Ok((
        StatusCode::OK,
        Json(VerifyTokenResponse {
            valid,
            user_id: None,
        }),
    ))
}

/// 刷新 Token
///
/// POST /auth/refresh-token
pub async fn refresh_token_handler(
    State(state): State<AppState>,
    Json(req): Json<RefreshTokenRequestJson>,
) -> Result<impl IntoResponse> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;

    let jwt_validator = state
        .auth
        .get_jwt_validator()
        .ok_or_else(|| ApiError::Internal("JWT not configured".into()))?
        .clone();

    let service = keycompute_auth::LoginService::new(Arc::clone(pool), jwt_validator);
    let response = service
        .refresh_token(&req.token)
        .await
        .map_err(|e| ApiError::Auth(format!("Token refresh failed: {}", e)))?;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "user_id": response.user_id.to_string(),
            "tenant_id": response.tenant_id.to_string(),
            "email": response.email,
            "role": response.role,
            "access_token": response.jwt_token,
            "token_type": "Bearer",
            "expires_in": response.expires_in
        })),
    ))
}

/// 刷新 Token 请求
#[derive(Debug, Deserialize)]
pub struct RefreshTokenRequestJson {
    pub token: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_request_json() {
        let json = RegisterRequestJson {
            email: "test@example.com".to_string(),
            referral_code: Some("6aac8ab5-aeec-48b8-a4cc-0a446d952862".to_string()),
        };

        assert_eq!(json.email, "test@example.com");
        assert_eq!(
            json.referral_code.as_deref(),
            Some("6aac8ab5-aeec-48b8-a4cc-0a446d952862")
        );
    }

    #[test]
    fn test_complete_registration_request_json() {
        let json = CompleteRegistrationRequestJson {
            email: "test@example.com".to_string(),
            code: "123456".to_string(),
            password: "SecurePass123!".to_string(),
            name: Some("Test User".to_string()),
        };

        assert_eq!(json.email, "test@example.com");
    }

    #[test]
    fn test_login_request_json() {
        let json = LoginRequestJson {
            email: "test@example.com".to_string(),
            password: "SecurePass123!".to_string(),
        };

        assert_eq!(json.email, "test@example.com");
    }

    #[test]
    fn test_forgot_password_request_json() {
        let json = ForgotPasswordRequestJson {
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
        };

        assert_eq!(json.name, "Test User");
        assert_eq!(json.email, "test@example.com");
    }

    #[test]
    fn test_forgot_password_identity_error_message() {
        assert_eq!(
            FORGOT_PASSWORD_IDENTITY_ERROR,
            "邮箱地址或用户名错误，无法发送重置密码链接"
        );
    }

    #[test]
    fn test_message_response() {
        let resp = MessageResponse {
            message: "Success".to_string(),
        };

        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("Success"));
    }
}
