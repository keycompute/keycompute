//! 内存缓存实现
//!
//! 基于 moka 库实现高性能内存缓存，支持：
//! - TTL 过期
//! - 容量限制
//! - 并发安全
//! - 异步支持

use moka::future::Cache;
use std::sync::Arc;
use std::time::Duration;

/// 内存缓存实现
///
/// 基于 moka 的异步缓存，支持 TTL 和容量限制
#[derive(Debug, Clone)]
pub struct MemoryCache {
    /// 底层缓存
    cache: Arc<Cache<String, String>>,
    /// 默认 TTL
    default_ttl: Duration,
}

impl MemoryCache {
    /// 创建新的内存缓存
    pub fn new() -> Self {
        Self::with_ttl(Duration::from_secs(300))
    }

    /// 创建带默认 TTL 的内存缓存
    pub fn with_ttl(ttl: Duration) -> Self {
        let cache = Cache::builder()
            .time_to_live(ttl)
            .build();
        Self {
            cache: Arc::new(cache),
            default_ttl: ttl,
        }
    }

    /// 创建带容量限制的内存缓存
    pub fn with_capacity_and_ttl(capacity: u64, ttl: Duration) -> Self {
        let cache = Cache::builder()
            .max_capacity(capacity)
            .time_to_live(ttl)
            .build();
        Self {
            cache: Arc::new(cache),
            default_ttl: ttl,
        }
    }

    /// 设置缓存值
    pub async fn set(&self, key: impl Into<String> + Send, value: String, _ttl: Option<Duration>) {
        let key = key.into();
        // TTL 在构建时设置，运行时通过重新插入来刷新 TTL
        self.cache.insert(key, value).await;
    }

    /// 获取缓存值
    pub async fn get(&self, key: impl Into<String>) -> Option<String> {
        let key = key.into();
        self.cache.get(&key).await
    }

    /// 删除缓存值
    pub async fn remove(&self, key: impl Into<String> + Send) -> bool {
        let key = key.into();
        // 先检查是否存在
        let existed = self.cache.get(&key).await.is_some();
        if existed {
            self.cache.invalidate(&key).await;
        }
        existed
    }

    /// 检查键是否存在且未过期
    pub async fn contains(&self, key: impl Into<String>) -> bool {
        let key = key.into();
        self.cache.get(&key).await.is_some()
    }

    /// 清理所有缓存
    pub async fn clear(&self) {
        self.cache.invalidate_all();
    }

    /// 获取缓存条目数量
    pub async fn len(&self) -> u64 {
        self.cache.entry_count()
    }

    /// 检查是否为空
    pub async fn is_empty(&self) -> bool {
        self.cache.entry_count() == 0
    }

    /// 获取并消费值
    pub async fn take(&self, key: impl Into<String>) -> Option<String> {
        let key = key.into();
        self.cache.get(&key).await.map(|v| {
            let _ = self.cache.invalidate(&key);
            v
        })
    }

    /// 获取默认 TTL
    pub fn default_ttl(&self) -> Duration {
        self.default_ttl
    }
}

impl Default for MemoryCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_memory_cache_basic() {
        let cache = MemoryCache::new();

        cache.set("key1", "value1".to_string(), None).await;
        let value = cache.get("key1").await;
        assert_eq!(value, Some("value1".to_string()));

        cache.remove("key1").await;
        let value = cache.get("key1").await;
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn test_memory_cache_ttl() {
        let cache = MemoryCache::with_ttl(Duration::from_millis(10));

        cache.set("key1", "value1".to_string(), None).await;
        assert!(cache.contains("key1").await);

        tokio::time::sleep(Duration::from_millis(20)).await;

        let value = cache.get("key1").await;
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn test_memory_cache_clear() {
        let cache = MemoryCache::new();

        cache.set("key1", "value1".to_string(), None).await;
        cache.set("key2", "value2".to_string(), None).await;

        cache.clear().await;

        assert!(!cache.contains("key1").await);
        assert!(!cache.contains("key2").await);
    }
}