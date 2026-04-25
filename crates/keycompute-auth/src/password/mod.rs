//! 密码认证模块
//!
//! 提供邮箱/密码认证的核心功能：
//! - 密码哈希与验证 (Argon2id)
//! - 密码强度验证
//! - 邮箱格式验证
//! - 用户注册服务
//! - 用户登录服务
//! - 密码重置服务
//! - 邮件服务集成

mod hash;
mod login;
mod register;
mod reset;
mod validator;

// 重新导出公共接口
pub use hash::PasswordHasher;
pub use login::{LoginRequest, LoginResponse, LoginService};
pub use register::{
    CompleteRegistrationRequest, CompleteRegistrationResponse, RegistrationService,
    RequestRegistrationCodeRequest, RequestRegistrationCodeResponse,
};
pub use reset::{PasswordResetService, RequestPasswordResetRequest, ResetPasswordRequest};
pub use validator::{EmailValidator, PasswordValidator};

// 重新导出邮件服务，方便调用方使用
pub use keycompute_emailserver::{EmailConfig, EmailService};
