//! 内存缓存实现

use crate::{CacheError, CacheResult};
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// 内存缓存条目
#[derive(Debug, Clone)]
struct CacheEntry<T> {
    value: T,
    expires_at: Option<Instant>,
}

/// 内存缓存实现
///
/// 使用 RwLock 实现并发读写，支持 TTL 过期
#[derive(Debug)]
pub struct MemoryCache<T> {
    /// 缓存存储
    store: Arc<RwLock<HashMap<String, CacheEntry<T>>>>,
    /// 默认 TTL
    default_ttl: Duration,
}

impl<T> MemoryCache<T> {
    /// 创建新的内存缓存
    pub fn new() -> Self {
        Self::with_ttl(Duration::from_secs(300))
    }

    /// 创建带默认 TTL 的内存缓存
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            store: Arc::new(RwLock::new(HashMap::new())),
            default_ttl: ttl,
        }
    }

    /// 设置缓存值
    pub async fn set(&self, key: impl Into<String> + Send, value: T, ttl: Option<Duration>) {
        let key = key.into();
        let ttl = ttl.unwrap_or(self.default_ttl);
        let expires_at = Some(Instant::now() + ttl);

        let mut store = self.store.write().await;
        store.insert(key, CacheEntry { value, expires_at });
    }

    /// 获取缓存值
    pub async fn get(&self, key: impl Into<String>) -> Option<T>
    where
        T: Clone,
    {
        let key = key.into();
        let mut store = self.store.write().await;

        if let Some(entry) = store.get(&key) {
            // 检查是否过期
            if let Some(expires_at) = entry.expires_at {
                if Instant::now() > expires_at {
                    store.remove(&key);
                    return None;
                }
            }
            // 返回克隆值
            return Some(entry.value.clone());
        }

        None
    }

    /// 获取缓存值并删除
    pub async fn get_or_take(&self, key: impl Into<String>) -> Option<T> {
        let key = key.into();
        let mut store = self.store.write().await;

        if let Some(entry) = store.remove(&key) {
            // 检查是否过期
            if let Some(expires_at) = entry.expires_at {
                if Instant::now() > expires_at {
                    return None;
                }
            }
            return Some(entry.value);
        }

        None
    }

    /// 删除缓存值
    pub async fn remove(&self, key: impl Into<String>) -> bool
    where
        T: Send,
    {
        let key = key.into();
        let mut store = self.store.write().await;
        store.remove(&key).is_some()
    }

    /// 检查键是否存在且未过期
    pub async fn contains(&self, key: impl Into<String>) -> bool
    where
        T: Clone,
    {
        let key = key.into();
        let mut store = self.store.write().await;

        if let Some(entry) = store.get(&key) {
            if let Some(expires_at) = entry.expires_at {
                if Instant::now() > expires_at {
                    store.remove(&key);
                    return false;
                }
            }
            return true;
        }

        false
    }

    /// 清理过期条目
    pub async fn cleanup(&self) {
        let now = Instant::now();
        let mut store = self.store.write().await;

        store.retain(|_, entry| {
            if let Some(expires_at) = entry.expires_at {
                now < expires_at
            } else {
                true
            }
        });
    }

    /// 清空所有缓存
    pub async fn clear(&self) {
        let mut store = self.store.write().await;
        store.clear();
    }

    /// 获取缓存条目数量
    pub async fn len(&self) -> usize {
        let store = self.store.read().await;
        store.len()
    }

    /// 检查是否为空
    pub async fn is_empty(&self) -> bool {
        let store = self.store.read().await;
        store.is_empty()
    }
}

impl<T> Default for MemoryCache<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// 字符串专用的内存缓存（无需 Clone）
impl MemoryCache<String> {
    /// 获取并消费值
    pub async fn take(&self, key: impl Into<String>) -> Option<String> {
        self.get_or_take(key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_memory_cache_basic() {
        let cache: MemoryCache<String> = MemoryCache::new();

        cache.set("key1", "value1".to_string(), None).await;
        let value = cache.get("key1").await;
        assert_eq!(value, Some("value1".to_string()));

        cache.remove("key1").await;
        let value = cache.get("key1").await;
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn test_memory_cache_ttl() {
        let cache: MemoryCache<String> = MemoryCache::with_ttl(Duration::from_millis(10));

        cache.set("key1", "value1".to_string(), Some(Duration::from_millis(5))).await;
        assert!(cache.contains("key1").await);

        tokio::time::sleep(Duration::from_millis(20)).await;

        let value = cache.get("key1").await;
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn test_memory_cache_take() {
        let cache: MemoryCache<String> = MemoryCache::new();

        cache.set("key1", "value1".to_string(), None).await;
        let value = cache.take("key1").await;
        assert_eq!(value, Some("value1".to_string()));

        // 再次获取应为 None
        let value = cache.get("key1").await;
        assert_eq!(value, None);
    }
}