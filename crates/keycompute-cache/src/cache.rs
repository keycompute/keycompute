//! 统一缓存管理器

use crate::{CacheBackend, CacheConfig, CacheError, CacheResult};
use deadpool_redis::{Config as RedisConfig, Pool};
use serde::{de::DeserializeOwned, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// 缓存管理器
///
/// 提供统一的缓存接口，支持 Redis 和内存缓存两种后端
#[derive(Clone)]
pub enum Cache {
    /// Redis 缓存
    Redis(RedisCache),
    /// 内存缓存
    Memory(Arc<MemoryCacheInner>),
}

/// 内存缓存内部实现（使用 Arc 共享）
type MemoryCacheInner = RwLock<super::MemoryCache<String>>;

/// Redis 缓存实现
#[derive(Clone)]
pub struct RedisCache {
    pool: Pool,
    key_prefix: String,
    default_ttl: Duration,
}

impl RedisCache {
    /// 从配置创建 Redis 缓存
    pub fn new(config: &CacheConfig) -> CacheResult<Self> {
        use deadpool_redis::Runtime;

        let mut redis_config = RedisConfig::from_url(&config.redis_url);
        redis_config.pool = Some(deadpool_redis::PoolConfig {
            max_size: config.pool_size,
            ..Default::default()
        });

        let pool = redis_config
            .create_pool(Some(Runtime::Tokio1))
            .map_err(|e| CacheError::PoolError(e.to_string()))?;

        Ok(Self {
            pool,
            key_prefix: config.key_prefix.clone(),
            default_ttl: config.default_ttl(),
        })
    }

    /// 构建完整的 Redis Key
    fn build_key(&self, key: &str) -> String {
        format!("{}:{}", self.key_prefix, key)
    }

    /// 获取连接
    async fn get_conn(&self) -> CacheResult<deadpool_redis::Connection> {
        self.pool
            .get()
            .await
            .map_err(|e| CacheError::PoolError(e.to_string()))
    }

    /// 健康检查
    pub async fn health_check(&self) -> CacheResult<()> {
        let mut conn = self.get_conn().await?;
        let _: () = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .map_err(|e| CacheError::OperationFailed(e.to_string()))?;
        Ok(())
    }

    /// 获取连接池状态
    pub fn pool_status(&self) -> deadpool_redis::Status {
        self.pool.status()
    }
}

impl Cache {
    /// 从配置创建缓存管理器
    pub async fn new(config: CacheConfig) -> CacheResult<Self> {
        match config.backend {
            CacheBackend::Redis => {
                let cache = RedisCache::new(&config)?;
                // 测试连接
                if let Err(e) = cache.health_check().await {
                    tracing::warn!("Redis connection failed: {}, falling back to memory", e);
                    Ok(Cache::Memory(Arc::new(RwLock::new(
                        super::MemoryCache::with_ttl(config.default_ttl()),
                    ))))
                } else {
                    tracing::info!("Redis cache initialized: {}", config.redis_url);
                    Ok(Cache::Redis(cache))
                }
            }
            CacheBackend::Memory => {
                tracing::info!("Memory cache initialized");
                Ok(Cache::Memory(Arc::new(RwLock::new(
                    super::MemoryCache::with_ttl(config.default_ttl()),
                ))))
            }
        }
    }

    /// 从环境变量创建缓存管理器
    pub async fn from_env() -> CacheResult<Self> {
        Self::new(CacheConfig::from_env()).await
    }

    /// 设置缓存值
    pub async fn set<T: Serialize>(
        &self,
        key: impl Into<String> + Send,
        value: &T,
        ttl: Option<Duration>,
    ) -> CacheResult<()> {
        let key = key.into();
        let value =
            serde_json::to_string(value).map_err(|e| CacheError::SerializationFailed(e.to_string()))?;

        match self {
            Cache::Redis(cache) => {
                let key = cache.build_key(&key);
                let ttl = ttl.unwrap_or(cache.default_ttl);
                let mut conn = cache.get_conn().await?;
                let _: () = redis::cmd("SET")
                    .arg(&key)
                    .arg(&value)
                    .arg("EX")
                    .arg(ttl.as_secs())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            }
            Cache::Memory(cache) => {
                let cache = cache.read().await;
                cache.set(key, value, ttl).await;
                Ok(())
            }
        }
    }

    /// 获取缓存值
    pub async fn get<T: DeserializeOwned>(
        &self,
        key: impl Into<String>,
    ) -> CacheResult<Option<T>> {
        let key = key.into();

        match self {
            Cache::Redis(cache) => {
                let key = cache.build_key(&key);
                let mut conn = cache.get_conn().await?;
                let value: Option<String> = redis::cmd("GET")
                    .arg(&key)
                    .query_async(&mut conn)
                    .await?;

                match value {
                    Some(v) => {
                        let deserialized =
                            serde_json::from_str(&v).map_err(|e| CacheError::DeserializationFailed(e.to_string()))?;
                        Ok(Some(deserialized))
                    }
                    None => Ok(None),
                }
            }
            Cache::Memory(cache) => {
                let cache = cache.read().await;
                let value = cache.get(&key).await;
                match value {
                    Some(v) => {
                        let deserialized = serde_json::from_str(&v)
                            .map_err(|e| CacheError::DeserializationFailed(e.to_string()))?;
                        Ok(Some(deserialized))
                    }
                    None => Ok(None),
                }
            }
        }
    }

    /// 获取缓存值，如果不存在则执行回调并缓存结果
    pub async fn get_or_set<T: Serialize + DeserializeOwned, F, Fut>(
        &self,
        key: impl Into<String> + Send + Clone,
        ttl: Option<Duration>,
        fetcher: F,
    ) -> CacheResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = CacheResult<T>>,
    {
        let key = key.into();

        // 尝试从缓存获取
        if let Some(value) = self.get::<T>(&key).await? {
            return Ok(value);
        }

        // 缓存不存在，执行回调
        let value = fetcher().await?;

        // 存入缓存
        self.set(&key, &value, ttl).await?;

        Ok(value)
    }

    /// 删除缓存值
    pub async fn remove(&self, key: impl Into<String> + Send) -> CacheResult<bool> {
        let key = key.into();

        match self {
            Cache::Redis(cache) => {
                let key = cache.build_key(&key);
                let mut conn = cache.get_conn().await?;
                let count: i64 = redis::cmd("DEL")
                    .arg(&key)
                    .query_async(&mut conn)
                    .await?;
                Ok(count > 0)
            }
            Cache::Memory(cache) => {
                let cache = cache.read().await;
                Ok(cache.remove(key).await)
            }
        }
    }

    /// 检查键是否存在
    pub async fn exists(&self, key: impl Into<String>) -> CacheResult<bool> {
        let key = key.into();

        match self {
            Cache::Redis(cache) => {
                let key = cache.build_key(&key);
                let mut conn = cache.get_conn().await?;
                let exists: bool = redis::cmd("EXISTS")
                    .arg(&key)
                    .query_async(&mut conn)
                    .await?;
                Ok(exists)
            }
            Cache::Memory(cache) => {
                let cache = cache.read().await;
                Ok(cache.contains(key).await)
            }
        }
    }

    /// 设置过期时间
    pub async fn expire(
        &self,
        key: impl Into<String> + Send,
        ttl: Duration,
    ) -> CacheResult<bool> {
        let key = key.into();

        match self {
            Cache::Redis(cache) => {
                let key = cache.build_key(&key);
                let mut conn = cache.get_conn().await?;
                let result: i64 = redis::cmd("EXPIRE")
                    .arg(&key)
                    .arg(ttl.as_secs())
                    .query_async(&mut conn)
                    .await?;
                Ok(result > 0)
            }
            Cache::Memory(cache) => {
                let cache = cache.read().await;
                Ok(cache.contains(key).await)
            }
        }
    }

    /// 递增数值
    pub async fn incr(&self, key: impl Into<String> + Send) -> CacheResult<i64> {
        let key = key.into();

        match self {
            Cache::Redis(cache) => {
                let key = cache.build_key(&key);
                let mut conn = cache.get_conn().await?;
                let value: i64 = redis::cmd("INCR")
                    .arg(&key)
                    .query_async(&mut conn)
                    .await?;
                Ok(value)
            }
            Cache::Memory(cache) => {
                let cache = cache.read().await;
                // 内存模式使用内部计数器
                let counter_key = format!("{}:counter", key);
                let current: i64 = cache
                    .get(&counter_key)
                    .await
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let new_value = current + 1;
                cache.set(counter_key, new_value.to_string(), None).await;
                Ok(new_value)
            }
        }
    }

    /// 递减数值
    pub async fn decr(&self, key: impl Into<String> + Send) -> CacheResult<i64> {
        let key = key.into();

        match self {
            Cache::Redis(cache) => {
                let key = cache.build_key(&key);
                let mut conn = cache.get_conn().await?;
                let value: i64 = redis::cmd("DECR")
                    .arg(&key)
                    .query_async(&mut conn)
                    .await?;
                Ok(value)
            }
            Cache::Memory(cache) => {
                let cache = cache.read().await;
                let counter_key = format!("{}:counter", key);
                let current: i64 = cache
                    .get(&counter_key)
                    .await
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let new_value = current - 1;
                cache.set(counter_key, new_value.to_string(), None).await;
                Ok(new_value)
            }
        }
    }

    /// 清空所有缓存
    pub async fn clear(&self) -> CacheResult<()> {
        match self {
            Cache::Redis(_cache) => {
                tracing::warn!("Redis clear not implemented");
                Ok(())
            }
            Cache::Memory(cache) => {
                let cache = cache.read().await;
                cache.clear().await;
                Ok(())
            }
        }
    }

    /// 批量获取
    pub async fn mget<T: DeserializeOwned>(
        &self,
        keys: Vec<String>,
    ) -> CacheResult<Vec<Option<T>>> {
        match self {
            Cache::Redis(cache) => {
                let full_keys: Vec<String> = keys.iter().map(|k| cache.build_key(k)).collect();
                let mut conn = cache.get_conn().await?;
                let values: Vec<Option<String>> = redis::cmd("MGET")
                    .arg(&full_keys)
                    .query_async(&mut conn)
                    .await?;

                let results: Vec<Option<T>> = values
                    .into_iter()
                    .map(|v| {
                        v.and_then(|s| serde_json::from_str(&s).ok())
                    })
                    .collect();
                Ok(results)
            }
            Cache::Memory(cache) => {
                let cache = cache.read().await;
                let mut final_results = Vec::new();
                for k in &keys {
                    let v = cache.get(k).await;
                    final_results.push(v.and_then(|val| serde_json::from_str(&val).ok()));
                }
                Ok(final_results)
            }
        }
    }

    /// 批量设置
    pub async fn mset<T: Serialize>(
        &self,
        kvs: Vec<(String, T)>,
        ttl: Option<Duration>,
    ) -> CacheResult<()> {
        match self {
            Cache::Redis(cache) => {
                let mut conn = cache.get_conn().await?;
                let ttl = ttl.unwrap_or(cache.default_ttl);

                for (key, value) in kvs {
                    let value = serde_json::to_string(&value)
                        .map_err(|e| CacheError::SerializationFailed(e.to_string()))?;
                    let full_key = cache.build_key(&key);
                    let _: () = redis::cmd("SET")
                        .arg(&full_key)
                        .arg(&value)
                        .arg("EX")
                        .arg(ttl.as_secs())
                        .query_async(&mut conn)
                        .await?;
                }
                Ok(())
            }
            Cache::Memory(cache) => {
                let cache = cache.read().await;
                for (key, value) in kvs {
                    let value = serde_json::to_string(&value)
                        .map_err(|e| CacheError::SerializationFailed(e.to_string()))?;
                    cache.set(key, value, ttl).await;
                }
                Ok(())
            }
        }
    }

    /// 获取后端类型
    pub fn backend(&self) -> CacheBackend {
        match self {
            Cache::Redis(_) => CacheBackend::Redis,
            Cache::Memory(_) => CacheBackend::Memory,
        }
    }
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Cache::Redis(_) => f.debug_struct("Cache::Redis").finish(),
            Cache::Memory(_) => f.debug_struct("Cache::Memory").finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::time::Duration;

    #[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
    struct TestUser {
        id: u64,
        name: String,
    }

    // ============ 基础功能测试 ============

    #[tokio::test]
    async fn test_memory_cache_set_get() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        // 测试 set/get
        let user = TestUser {
            id: 1,
            name: "Alice".to_string(),
        };
        cache.set("user:1", &user, None).await.unwrap();

        let cached: Option<TestUser> = cache.get("user:1").await.unwrap();
        assert_eq!(cached, Some(user));
    }

    #[tokio::test]
    async fn test_memory_cache_get_not_found() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        let cached: Option<TestUser> = cache.get("not_exist").await.unwrap();
        assert!(cached.is_none());
    }

    // ============ 删除操作测试 ============

    #[tokio::test]
    async fn test_memory_cache_remove() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        // 设置后删除
        cache.set("key1", &"value1", None).await.unwrap();
        let removed = cache.remove("key1").await.unwrap();
        assert!(removed);

        // 验证已删除
        let value: Option<String> = cache.get("key1").await.unwrap();
        assert!(value.is_none());

        // 删除不存在的键
        let removed = cache.remove("not_exist").await.unwrap();
        assert!(!removed);
    }

    // ============ 存在性检查测试 ============

    #[tokio::test]
    async fn test_memory_cache_exists() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        // 设置后存在
        cache.set("key1", &"value1", None).await.unwrap();
        let exists = cache.exists("key1").await.unwrap();
        assert!(exists);

        // 不存在
        let exists = cache.exists("not_exist").await.unwrap();
        assert!(!exists);
    }

    // ============ TTL 过期测试 ============

    #[tokio::test]
    async fn test_memory_cache_ttl() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        // 设置短 TTL
        cache
            .set("ttl_key", &"ttl_value", Some(Duration::from_millis(50)))
            .await
            .unwrap();

        // 立即检查存在
        let exists = cache.exists("ttl_key").await.unwrap();
        assert!(exists);

        // 等待过期
        tokio::time::sleep(Duration::from_millis(100)).await;

        // 检查已过期
        let value: Option<String> = cache.get("ttl_key").await.unwrap();
        assert!(value.is_none());
    }

    // ============ get_or_set 测试 ============

    #[tokio::test]
    async fn test_memory_cache_get_or_set_miss() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        // 缓存未命中，执行回调
        let user = cache
            .get_or_set("user:2", None, || async {
                Ok(TestUser {
                    id: 2,
                    name: "Bob".to_string(),
                })
            })
            .await
            .unwrap();

        assert_eq!(user.name, "Bob");
    }

    #[tokio::test]
    async fn test_memory_cache_get_or_set_hit() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        // 先设置值
        cache
            .set("user:3", &TestUser { id: 3, name: "Carol".to_string() }, None)
            .await
            .unwrap();

        // 缓存命中，不再执行回调
        let user: TestUser = cache
            .get_or_set("user:3", None, || async {
                panic!("Should not be called");
            })
            .await
            .unwrap();

        assert_eq!(user.name, "Carol");
    }

    // ============ 计数器测试 ============

    #[tokio::test]
    async fn test_memory_cache_incr() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        // 初始值递增
        let count = cache.incr("counter").await.unwrap();
        assert_eq!(count, 1);

        // 再次递增
        let count = cache.incr("counter").await.unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_memory_cache_decr() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        // 初始值递减
        let count = cache.decr("counter").await.unwrap();
        assert_eq!(count, -1);

        // 再次递减
        let count = cache.decr("counter").await.unwrap();
        assert_eq!(count, -2);
    }

    // ============ 批量操作测试 ============

    #[tokio::test]
    async fn test_memory_cache_mget() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        // 批量设置
        cache
            .mset(
                vec![
                    ("key1".to_string(), "value1"),
                    ("key2".to_string(), "value2"),
                    ("key3".to_string(), "value3"),
                ],
                None,
            )
            .await
            .unwrap();

        // 批量获取
        let results: Vec<Option<String>> = cache
            .mget(vec![
                "key1".to_string(),
                "key2".to_string(),
                "not_exist".to_string(),
            ])
            .await
            .unwrap();

        assert_eq!(results[0], Some("value1".to_string()));
        assert_eq!(results[1], Some("value2".to_string()));
        assert_eq!(results[2], None);
    }

    #[tokio::test]
    async fn test_memory_cache_mset() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        // 批量设置
        cache
            .mset(
                vec![
                    ("batch1".to_string(), "data1"),
                    ("batch2".to_string(), "data2"),
                ],
                None,
            )
            .await
            .unwrap();

        // 验证
        let v1: Option<String> = cache.get("batch1").await.unwrap();
        let v2: Option<String> = cache.get("batch2").await.unwrap();

        assert_eq!(v1, Some("data1".to_string()));
        assert_eq!(v2, Some("data2".to_string()));
    }

    // ============ 清空缓存测试 ============

    #[tokio::test]
    async fn test_memory_cache_clear() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        // 设置多个值
        cache.set("k1", &"v1", None).await.unwrap();
        cache.set("k2", &"v2", None).await.unwrap();

        // 清空
        cache.clear().await.unwrap();

        // 验证已清空
        let v1: Option<String> = cache.get("k1").await.unwrap();
        let v2: Option<String> = cache.get("k2").await.unwrap();

        assert!(v1.is_none());
        assert!(v2.is_none());
    }

    // ============ 复杂数据结构测试 ============

    #[tokio::test]
    async fn test_memory_cache_complex_struct() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        // 测试嵌套结构
        #[derive(Serialize, Deserialize, Debug, Clone)]
        struct NestedStruct {
            user: TestUser,
            tags: Vec<String>,
            metadata: std::collections::HashMap<String, String>,
        }

        let nested = NestedStruct {
            user: TestUser {
                id: 100,
                name: "Nested".to_string(),
            },
            tags: vec!["tag1".to_string(), "tag2".to_string()],
            metadata: {
                let mut m = std::collections::HashMap::new();
                m.insert("key".to_string(), "value".to_string());
                m
            },
        };

        cache.set("nested", &nested, None).await.unwrap();

        let cached: Option<NestedStruct> = cache.get("nested").await.unwrap();
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().user.name, "Nested");
    }

    // ============ 后端类型测试 ============

    #[tokio::test]
    async fn test_cache_backend_type() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        assert_eq!(cache.backend(), CacheBackend::Memory);
    }

    // ============ 并发测试 ============

    #[tokio::test]
    async fn test_memory_cache_concurrent() {
        let config = CacheConfig {
            backend: CacheBackend::Memory,
            ..Default::default()
        };
        let cache = Cache::new(config).await.unwrap();

        // 并发写入
        let mut handles = Vec::new();
        for i in 0..10 {
            let cache = cache.clone();
            let handle = tokio::spawn(async move {
                cache
                    .set(format!("concurrent:{}", i), &i, None)
                    .await
                    .unwrap();
            });
            handles.push(handle);
        }

        // 等待所有任务完成
        for handle in handles {
            handle.await.unwrap();
        }

        // 验证所有值都写入成功
        for i in 0..10 {
            let value: Option<i32> = cache.get(format!("concurrent:{}", i)).await.unwrap();
            assert_eq!(value, Some(i));
        }
    }
}