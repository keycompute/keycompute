//! 缓存错误定义

use thiserror::Error;

/// 缓存操作错误
#[derive(Error, Debug)]
pub enum CacheError {
    /// 连接错误
    #[error("缓存连接失败: {0}")]
    ConnectionFailed(String),

    /// 操作失败
    #[error("缓存操作失败: {0}")]
    OperationFailed(String),

    /// 键不存在
    #[error("键不存在: {0}")]
    KeyNotFound(String),

    /// 序列化失败
    #[error("数据序列化失败: {0}")]
    SerializationFailed(String),

    /// 反序列化失败
    #[error("数据反序列化失败: {0}")]
    DeserializationFailed(String),

    /// 池错误
    #[error("连接池错误: {0}")]
    PoolError(String),

    /// 超时
    #[error("操作超时: {0}")]
    Timeout(String),
}

impl From<deadpool_redis::PoolError> for CacheError {
    fn from(e: deadpool_redis::PoolError) -> Self {
        CacheError::PoolError(e.to_string())
    }
}

impl From<deadpool_redis::redis::RedisError> for CacheError {
    fn from(e: deadpool_redis::redis::RedisError) -> Self {
        CacheError::OperationFailed(e.to_string())
    }
}

impl From<serde_json::Error> for CacheError {
    fn from(e: serde_json::Error) -> Self {
        CacheError::SerializationFailed(e.to_string())
    }
}

/// 缓存操作结果
pub type CacheResult<T> = Result<T, CacheError>;