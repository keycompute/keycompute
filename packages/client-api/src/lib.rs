//! Client API
//!
//! Dioxus 前端与 keycompute-server 之间的 HTTP 客户端封装层
//!
//! ## 使用示例
//!
//! ```rust,no_run
//! use client_api::{ApiClient, ClientConfig, AuthApi};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // 创建客户端配置
//!     let config = ClientConfig::new("http://localhost:8080");
//!
//!     // 创建 API 客户端
//!     let client = ApiClient::new(config)?;
//!
//!     // 使用认证 API
//!     let auth_api = AuthApi::new(&client);
//!     let response = auth_api.login(&client_api::api::auth::LoginRequest::new(
//!         "user@example.com",
//!         "password123",
//!     )).await?;
//!
//!     println!("登录成功: {:?}", response);
//!     Ok(())
//! }
//! ```

pub mod api;
pub mod client;
pub mod config;
pub mod error;

// 重新导出主要类型
pub use api::admin::AdminApi;
pub use api::api_key::ApiKeyApi;
pub use api::auth::AuthApi;
pub use api::billing::BillingApi;
pub use api::debug::DebugApi;
pub use api::distribution::DistributionApi;
pub use api::health::HealthApi;
pub use api::openai::OpenAiApi;
pub use api::payment::PaymentApi;
pub use api::settings::SettingsApi;
pub use api::tenant::TenantApi;
pub use api::usage::UsageApi;
pub use api::user::UserApi;
pub use client::{ApiClient, OpenAiClient};
pub use config::ClientConfig;
pub use error::{ClientError, Result};
