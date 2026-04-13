//! KeyCompute 缓存抽象层
//!
//! 提供统一的缓存接口，支持 Redis 和内存缓存
//!
//! # 使用示例
//! ```rust,ignore
//! use keycompute_cache::{Cache, CacheConfig, CacheBackend};
//!
//! // 初始化缓存
//! let cache = Cache::new(CacheConfig::default()).await?;
//!
//! // 缓存数据
//! cache.set("user:1", &user, Some(Duration::from_secs(300))).await?;
//!
//! // 获取数据
//! let user: Option<User> = cache.get("user:1").await?;
//! ```

mod cache;
mod config;
mod error;
mod memory;

pub use cache::*;
pub use config::*;
pub use error::*;
pub use memory::*;