//! Redis Runtime 模块端到端测试
//!
//! 验证 keycompute-runtime 的 Redis 存储功能，包括：
//! - Redis 连接测试
//! - 基本存储操作测试 (get/set/del)
//! - 计数器操作测试 (incr/decr)
//! - TTL 管理测试
//! - 批量操作测试 (mget/mset)
//! - 多实例数据共享测试

use integration_tests::common::VerificationChain;
use keycompute_runtime::{RedisRuntimeStore, RuntimeStore};
use std::time::Duration;
use uuid::Uuid;

/// 获取测试用 Redis URL
fn get_redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
}

/// 生成唯一的测试标识符
fn generate_test_id() -> String {
    Uuid::new_v4().simple().to_string()
}

/// 测试 Redis 连接
#[tokio::test]
async fn test_redis_runtime_connection() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();

    // 尝试创建 Redis 运行时存储
    let result = RedisRuntimeStore::new(&redis_url);

    chain.add_step(
        "keycompute-runtime",
        "RedisRuntimeStore::new",
        format!("Redis URL: {}", redis_url),
        result.is_ok(),
    );

    if let Ok(store) = result {
        // 验证可以执行基本操作
        store.set("connection_test", "ok", None).await;
        let value = store.get("connection_test").await;

        chain.add_step(
            "keycompute-runtime",
            "basic_operation",
            "Can perform basic operations",
            value == Some("ok".to_string()),
        );

        // 清理
        let _ = store.flush_prefix().await;
    }

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试基本存储操作 (get/set/del)
#[tokio::test]
async fn test_redis_runtime_basic_operations() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();
    let test_id = generate_test_id();
    let prefix = format!("test:{}:basic", test_id);

    let store = match RedisRuntimeStore::with_prefix(&redis_url, &prefix) {
        Ok(s) => s,
        Err(_) => {
            println!("Warning: Redis not available, skipping test");
            return;
        }
    };

    // 清理测试数据
    let _ = store.flush_prefix().await;

    // 测试 set/get
    store.set("test_key", "test_value", None).await;
    let value = store.get("test_key").await;

    chain.add_step(
        "keycompute-runtime",
        "set/get",
        format!("Value: {:?}", value),
        value == Some("test_value".to_string()),
    );

    // 测试更新值
    store.set("test_key", "updated_value", None).await;
    let updated = store.get("test_key").await;

    chain.add_step(
        "keycompute-runtime",
        "update",
        format!("Updated value: {:?}", updated),
        updated == Some("updated_value".to_string()),
    );

    // 测试 del
    store.del("test_key").await;
    let deleted = store.get("test_key").await;

    chain.add_step(
        "keycompute-runtime",
        "del",
        "Key deleted",
        deleted.is_none(),
    );

    // 测试获取不存在的 key
    let not_found = store.get("non_existent_key").await;

    chain.add_step(
        "keycompute-runtime",
        "get_non_existent",
        "Non-existent key returns None",
        not_found.is_none(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试计数器操作 (incr/decr)
#[tokio::test]
async fn test_redis_runtime_counter_operations() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();
    let test_id = generate_test_id();
    let prefix = format!("test:{}:counter", test_id);

    let store = match RedisRuntimeStore::with_prefix(&redis_url, &prefix) {
        Ok(s) => s,
        Err(_) => {
            println!("Warning: Redis not available, skipping test");
            return;
        }
    };

    // 清理测试数据
    let _ = store.flush_prefix().await;

    // 测试 incr
    let count1 = store.incr("counter").await;
    chain.add_step(
        "keycompute-runtime",
        "incr_first",
        format!("First incr: {}", count1),
        count1 == 1,
    );

    let count2 = store.incr("counter").await;
    chain.add_step(
        "keycompute-runtime",
        "incr_second",
        format!("Second incr: {}", count2),
        count2 == 2,
    );

    let count3 = store.incr("counter").await;
    chain.add_step(
        "keycompute-runtime",
        "incr_third",
        format!("Third incr: {}", count3),
        count3 == 3,
    );

    // 测试 decr
    let count4 = store.decr("counter").await;
    chain.add_step(
        "keycompute-runtime",
        "decr_first",
        format!("First decr: {}", count4),
        count4 == 2,
    );

    let count5 = store.decr("counter").await;
    chain.add_step(
        "keycompute-runtime",
        "decr_second",
        format!("Second decr: {}", count5),
        count5 == 1,
    );

    // 测试 decr 到负数
    let count6 = store.decr("counter").await;
    let count7 = store.decr("counter").await;

    chain.add_step(
        "keycompute-runtime",
        "decr_negative",
        format!("Negative values: {}, {}", count6, count7),
        count6 == 0 && count7 == -1,
    );

    // 测试多个计数器
    let counter_a = store.incr("counter_a").await;
    let counter_b = store.incr("counter_b").await;

    chain.add_step(
        "keycompute-runtime",
        "multiple_counters",
        format!("Counter A: {}, Counter B: {}", counter_a, counter_b),
        counter_a == 1 && counter_b == 1,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试 TTL 管理
#[tokio::test]
async fn test_redis_runtime_ttl() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();
    let test_id = generate_test_id();
    let prefix = format!("test:{}:ttl", test_id);

    let store = match RedisRuntimeStore::with_prefix(&redis_url, &prefix) {
        Ok(s) => s,
        Err(_) => {
            println!("Warning: Redis not available, skipping test");
            return;
        }
    };

    // 清理测试数据
    let _ = store.flush_prefix().await;

    // 测试设置带 TTL 的值
    store
        .set("ttl_key", "ttl_value", Some(Duration::from_secs(10)))
        .await;

    // 验证值存在
    let exists = store.exists("ttl_key").await;
    chain.add_step(
        "keycompute-runtime",
        "exists",
        "Key with TTL exists",
        exists,
    );

    // 验证 TTL 设置正确
    let ttl = store.ttl("ttl_key").await;
    chain.add_step(
        "keycompute-runtime",
        "ttl",
        format!("TTL: {} (should be > 0 and <= 10)", ttl),
        ttl > 0 && ttl <= 10,
    );

    // 验证可以获取值
    let value = store.get("ttl_key").await;
    chain.add_step(
        "keycompute-runtime",
        "get_with_ttl",
        format!("Value: {:?}", value),
        value == Some("ttl_value".to_string()),
    );

    // 测试 expire 操作
    store.set("expire_key", "expire_value", None).await;
    store.expire("expire_key", Duration::from_secs(5)).await;

    let expire_ttl = store.ttl("expire_key").await;
    chain.add_step(
        "keycompute-runtime",
        "expire",
        format!("Expire TTL: {} (should be > 0 and <= 5)", expire_ttl),
        expire_ttl > 0 && expire_ttl <= 5,
    );

    // 测试不存在的 key 的 TTL
    let not_exist_ttl = store.ttl("non_existent_key").await;
    chain.add_step(
        "keycompute-runtime",
        "ttl_non_existent",
        format!("TTL for non-existent: {} (should be -2)", not_exist_ttl),
        not_exist_ttl == -2,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试批量操作 (mget/mset)
#[tokio::test]
async fn test_redis_runtime_batch_operations() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();
    let test_id = generate_test_id();
    let prefix = format!("test:{}:batch", test_id);

    let store = match RedisRuntimeStore::with_prefix(&redis_url, &prefix) {
        Ok(s) => s,
        Err(_) => {
            println!("Warning: Redis not available, skipping test");
            return;
        }
    };

    // 清理测试数据
    let _ = store.flush_prefix().await;

    // 测试 mset
    let kvs = [
        ("batch_key1", "value1"),
        ("batch_key2", "value2"),
        ("batch_key3", "value3"),
    ];
    store.mset(&kvs, None).await;

    // 测试 mget
    let keys = ["batch_key1", "batch_key2", "batch_key3"];
    let values = store.mget(&keys).await;

    chain.add_step(
        "keycompute-runtime",
        "mset/mget",
        format!(
            "Values: {:?}",
            values
                .iter()
                .map(|v| v.as_deref().unwrap_or("None"))
                .collect::<Vec<_>>()
        ),
        values.len() == 3
            && values[0] == Some("value1".to_string())
            && values[1] == Some("value2".to_string())
            && values[2] == Some("value3".to_string()),
    );

    // 测试 mget 包含不存在的 key
    let mixed_keys = ["batch_key1", "non_existent", "batch_key3"];
    let mixed_values = store.mget(&mixed_keys).await;

    chain.add_step(
        "keycompute-runtime",
        "mget_mixed",
        "mget with non-existent keys",
        mixed_values.len() == 3
            && mixed_values[0].is_some()
            && mixed_values[1].is_none()
            && mixed_values[2].is_some(),
    );

    // 测试批量删除后 mget
    store.del("batch_key1").await;
    store.del("batch_key2").await;
    store.del("batch_key3").await;

    let after_del = store.mget(&keys).await;
    chain.add_step(
        "keycompute-runtime",
        "mget_after_del",
        "All values are None after delete",
        after_del.iter().all(|v| v.is_none()),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试多实例数据共享
#[tokio::test]
async fn test_redis_runtime_multi_instance() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();
    let test_id = generate_test_id();
    let prefix = format!("test:{}:multi", test_id);

    // 创建两个独立的 Redis 存储实例（使用相同前缀共享状态）
    let store1 = match RedisRuntimeStore::with_prefix(&redis_url, &prefix) {
        Ok(s) => s,
        Err(_) => {
            println!("Warning: Redis not available, skipping test");
            return;
        }
    };

    let store2 = match RedisRuntimeStore::with_prefix(&redis_url, &prefix) {
        Ok(s) => s,
        Err(_) => {
            println!("Warning: Redis not available, skipping test");
            return;
        }
    };

    // 清理测试数据
    let _ = store1.flush_prefix().await;

    // 通过 store1 设置值
    store1.set("shared_key", "shared_value", None).await;

    // 通过 store2 读取值（应该能看到 store1 设置的值）
    let value = store2.get("shared_key").await;

    chain.add_step(
        "keycompute-runtime",
        "instance1_set/instance2_get",
        format!("Shared value: {:?}", value),
        value == Some("shared_value".to_string()),
    );

    // 通过 store2 更新值
    store2.set("shared_key", "updated_by_instance2", None).await;

    // 通过 store1 读取更新后的值
    let updated = store1.get("shared_key").await;

    chain.add_step(
        "keycompute-runtime",
        "instance2_update/instance1_get",
        format!("Updated value: {:?}", updated),
        updated == Some("updated_by_instance2".to_string()),
    );

    // 测试计数器共享
    let count1 = store1.incr("shared_counter").await;
    let count2 = store2.incr("shared_counter").await;
    let count3 = store1.incr("shared_counter").await;

    chain.add_step(
        "keycompute-runtime",
        "shared_counter",
        format!("Counts: {}, {}, {}", count1, count2, count3),
        count1 == 1 && count2 == 2 && count3 == 3,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试带前缀的 Redis 存储
#[tokio::test]
async fn test_redis_runtime_with_prefix() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();
    let test_id = generate_test_id();

    // 创建带不同前缀的两个存储实例（包含测试ID确保隔离）
    let store1 =
        match RedisRuntimeStore::with_prefix(&redis_url, format!("test:{}:prefix:1", test_id)) {
            Ok(s) => s,
            Err(_) => {
                println!("Warning: Redis not available, skipping test");
                return;
            }
        };

    let store2 =
        match RedisRuntimeStore::with_prefix(&redis_url, format!("test:{}:prefix:2", test_id)) {
            Ok(s) => s,
            Err(_) => {
                println!("Warning: Redis not available, skipping test");
                return;
            }
        };

    // 清理测试数据
    let _ = store1.flush_prefix().await;
    let _ = store2.flush_prefix().await;

    // 使用相同的 key 但在不同前缀下
    store1.set("same_key", "value_from_prefix1", None).await;
    store2.set("same_key", "value_from_prefix2", None).await;

    // 各自读取应该得到不同的值
    let value1 = store1.get("same_key").await;
    let value2 = store2.get("same_key").await;

    chain.add_step(
        "keycompute-runtime",
        "prefix_isolation",
        format!("Prefix1: {:?}, Prefix2: {:?}", value1, value2),
        value1 == Some("value_from_prefix1".to_string())
            && value2 == Some("value_from_prefix2".to_string()),
    );

    // 验证前缀配置
    chain.add_step(
        "keycompute-runtime",
        "key_prefix",
        format!(
            "Store1 prefix: {}, Store2 prefix: {}",
            store1.key_prefix(),
            store2.key_prefix()
        ),
        store1.key_prefix().starts_with("test:") && store2.key_prefix().starts_with("test:"),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试并发场景下的数据一致性
#[tokio::test]
async fn test_redis_runtime_concurrent_access() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();
    let test_id = generate_test_id();
    let prefix = format!("test:{}:concurrent", test_id);

    let store = match RedisRuntimeStore::with_prefix(&redis_url, &prefix) {
        Ok(s) => std::sync::Arc::new(s),
        Err(_) => {
            println!("Warning: Redis not available, skipping test");
            return;
        }
    };

    // 清理测试数据
    let _ = store.flush_prefix().await;

    // 并发执行 incr 操作
    let mut handles = vec![];
    for _ in 0..10 {
        let store_clone = std::sync::Arc::clone(&store);
        let handle = tokio::spawn(async move {
            for _ in 0..10 {
                let _ = store_clone.incr("concurrent_counter").await;
            }
        });
        handles.push(handle);
    }

    // 等待所有任务完成
    for handle in handles {
        let _ = handle.await;
    }

    // 验证最终计数
    let final_count = store.incr("concurrent_counter").await;

    chain.add_step(
        "keycompute-runtime",
        "concurrent_incr",
        format!("Final count: {} (expected 101)", final_count),
        final_count == 101,
    );

    // 测试并发 set/get
    let store_for_set = std::sync::Arc::clone(&store);
    let set_handle = tokio::spawn(async move {
        for i in 0..5 {
            store_for_set
                .set(
                    &format!("concurrent_key_{}", i),
                    &format!("value_{}", i),
                    None,
                )
                .await;
        }
    });

    let store_for_get = std::sync::Arc::clone(&store);
    let get_handle = tokio::spawn(async move {
        let mut found = 0;
        for _ in 0..10 {
            for i in 0..5 {
                if store_for_get
                    .get(&format!("concurrent_key_{}", i))
                    .await
                    .is_some()
                {
                    found += 1;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
        found
    });

    let _ = set_handle.await;
    let found_count = get_handle.await.unwrap();

    chain.add_step(
        "keycompute-runtime",
        "concurrent_set_get",
        format!("Found count: {} (should be > 0)", found_count),
        found_count > 0,
    );

    chain.print_report();
    assert!(chain.all_passed());
}
