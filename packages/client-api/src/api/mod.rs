//! API 模块
//!
//! 按功能模块组织的 API 客户端，与后端路由结构对齐

pub mod admin;
pub mod api_key;
pub mod auth;
pub mod billing;
pub mod debug;
pub mod distribution;
pub mod health;
pub mod openai;
pub mod payment;
pub mod settings;
pub mod tenant;
pub mod usage;
pub mod user;

use crate::client::ApiClient;

/// API 模块 trait
///
/// 所有 API 模块都实现此 trait，提供统一的客户端访问方式
pub trait ApiModule {
    /// 创建新的 API 模块实例
    fn new(client: &ApiClient) -> Self
    where
        Self: Sized;
}
