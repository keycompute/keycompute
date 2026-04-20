#![allow(dead_code)]

use client_api::error::Result;
use client_api::{
    AuthApi,
    api::auth::{
        AuthResponse, CompleteRegistrationRequest, CompleteRegistrationResponse,
        ForgotPasswordRequest, LoginRequest, MessageResponse, RefreshTokenRequest,
        RequestRegistrationCodeRequest, RequestRegistrationCodeResponse, ResetPasswordRequest,
    },
};

use super::api_client::get_client;

pub async fn login(email: &str, password: &str) -> Result<AuthResponse> {
    let client = get_client();
    let api = AuthApi::new(&client);
    let resp = api.login(&LoginRequest::new(email, password)).await?;
    // 登录成功后设置全局 token
    client.set_token(&resp.access_token);
    Ok(resp)
}

pub async fn request_registration_code(
    email: &str,
    referral_code: Option<&str>,
) -> Result<RequestRegistrationCodeResponse> {
    let client = get_client();
    let api = AuthApi::new(&client);
    let mut req = RequestRegistrationCodeRequest::new(email);
    if let Some(code) = referral_code {
        req = req.with_referral_code(code);
    }
    api.request_registration_code(&req).await
}

pub async fn complete_registration(
    email: &str,
    code: &str,
    password: &str,
    name: Option<&str>,
) -> Result<CompleteRegistrationResponse> {
    let client = get_client();
    let api = AuthApi::new(&client);
    let mut req = CompleteRegistrationRequest::new(email, code, password);
    if let Some(n) = name {
        req = req.with_name(n);
    }
    api.complete_registration(&req).await
}

pub async fn refresh_token(refresh_token: &str) -> Result<AuthResponse> {
    let client = get_client();
    AuthApi::new(&client)
        .refresh_token(&RefreshTokenRequest::new(refresh_token))
        .await
}

pub async fn forgot_password(email: &str) -> Result<MessageResponse> {
    let client = get_client();
    let api = AuthApi::new(&client);
    api.forgot_password(&ForgotPasswordRequest::new(email))
        .await
}

pub async fn reset_password(token: &str, new_password: &str) -> Result<MessageResponse> {
    let client = get_client();
    let api = AuthApi::new(&client);
    api.reset_password(&ResetPasswordRequest::new(token, new_password))
        .await
}

pub async fn verify_reset_token(token: &str) -> Result<MessageResponse> {
    let client = get_client();
    let api = AuthApi::new(&client);
    api.verify_reset_token(token).await
}
