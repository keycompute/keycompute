//! Redis 限流模块端到端测试
//!
//! 验证 Redis 限流模块在各场景下的行为，包括：
//! - Redis 连接测试
//! - 分布式限流测试
//! - 多实例同步测试

use integration_tests::common::VerificationChain;
use keycompute_ratelimit::{RateLimitConfig, RateLimitKey, RateLimitService};
use std::sync::Arc;
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
async fn test_redis_connection() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();

    // 尝试创建 Redis 限流服务
    let result = RateLimitService::new_redis(&redis_url);

    chain.add_step(
        "keycompute-ratelimit",
        "RedisRateLimiter::new",
        format!("Redis URL: {}", redis_url),
        result.is_ok(),
    );

    if let Ok(service) = result {
        // 验证后端类型为 Redis
        chain.add_step(
            "keycompute-ratelimit",
            "RateLimitService::backend",
            "Backend is Redis",
            matches!(
                service.backend(),
                keycompute_ratelimit::RateLimitBackend::Redis
            ),
        );
    }

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试分布式限流 - 基本功能
#[tokio::test]
async fn test_distributed_ratelimit_basic() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();

    // 创建 Redis 限流服务
    let service = match RateLimitService::new_redis(&redis_url) {
        Ok(s) => s,
        Err(_) => {
            println!("Warning: Redis not available, skipping test");
            return;
        }
    };

    // 创建限流键
    let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let config = RateLimitConfig::new(10, 1000);

    // 第一次检查应该通过
    let allowed = service.check_only_with_config(&key, &config).await.unwrap();
    chain.add_step(
        "keycompute-ratelimit",
        "check_only_with_config",
        "First check allowed",
        allowed,
    );

    // 记录请求
    let record_result = service.check_and_record_with_config(&key, &config).await;
    chain.add_step(
        "keycompute-ratelimit",
        "check_and_record_with_config",
        "Record request success",
        record_result.is_ok(),
    );

    // 获取当前计数
    let count = service.get_rpm_count(&key).await.unwrap();
    chain.add_step(
        "keycompute-ratelimit",
        "get_rpm_count",
        format!("Current count: {}", count),
        count > 0,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试分布式限流 - 限流触发
#[tokio::test]
async fn test_distributed_ratelimit_exceeded() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();
    let test_id = generate_test_id();

    // 创建带测试隔离前缀的 Redis 限流服务
    let service =
        match RateLimitService::new_redis_with_prefix(&redis_url, format!("test-{}", test_id)) {
            Ok(s) => s,
            Err(_) => {
                println!("Warning: Redis not available, skipping test");
                return;
            }
        };

    // 创建限流键，使用非常低的限制
    let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let config = RateLimitConfig::new(2, 1000);

    // 前两次请求应该成功
    let result1 = service.check_and_record_with_config(&key, &config).await;
    let result2 = service.check_and_record_with_config(&key, &config).await;

    chain.add_step(
        "keycompute-ratelimit",
        "check_and_record_with_config",
        "First request allowed",
        result1.is_ok(),
    );

    chain.add_step(
        "keycompute-ratelimit",
        "check_and_record_with_config",
        "Second request allowed",
        result2.is_ok(),
    );

    // 第三次请求应该被拒绝（达到限制）
    let result3 = service.check_and_record_with_config(&key, &config).await;
    chain.add_step(
        "keycompute-ratelimit",
        "check_and_record_with_config",
        "Third request rejected",
        result3.is_err(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试多实例同步 - 多个服务实例共享限流状态
#[tokio::test]
async fn test_multi_instance_sync() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();
    let test_id = generate_test_id();
    let prefix = format!("test-{}", test_id);

    // 创建两个独立的 Redis 限流服务实例（使用相同前缀共享状态）
    let service1 = match RateLimitService::new_redis_with_prefix(&redis_url, &prefix) {
        Ok(s) => s,
        Err(_) => {
            println!("Warning: Redis not available, skipping test");
            return;
        }
    };

    let service2 = match RateLimitService::new_redis_with_prefix(&redis_url, &prefix) {
        Ok(s) => s,
        Err(_) => {
            println!("Warning: Redis not available, skipping test");
            return;
        }
    };

    // 使用相同的限流键
    let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let config = RateLimitConfig::new(3, 1000);

    // 通过实例1记录请求
    let result1 = service1.check_and_record_with_config(&key, &config).await;
    chain.add_step(
        "keycompute-ratelimit",
        "instance1::check_and_record",
        "Instance 1 first request",
        result1.is_ok(),
    );

    // 通过实例2检查计数（应该能看到实例1的记录）
    let count_after_first = service2.get_rpm_count(&key).await.unwrap();
    chain.add_step(
        "keycompute-ratelimit",
        "instance2::get_rpm_count",
        format!("Count after instance1 request: {}", count_after_first),
        count_after_first == 1,
    );

    // 通过实例2记录请求
    let result2 = service2.check_and_record_with_config(&key, &config).await;
    chain.add_step(
        "keycompute-ratelimit",
        "instance2::check_and_record",
        "Instance 2 request",
        result2.is_ok(),
    );

    // 通过实例1检查计数（应该能看到实例2的记录）
    let count_after_second = service1.get_rpm_count(&key).await.unwrap();
    chain.add_step(
        "keycompute-ratelimit",
        "instance1::get_rpm_count",
        format!("Count after instance2 request: {}", count_after_second),
        count_after_second == 2,
    );

    // 通过实例1记录第三次请求
    let result3 = service1.check_and_record_with_config(&key, &config).await;
    chain.add_step(
        "keycompute-ratelimit",
        "instance1::check_and_record",
        "Instance 1 third request",
        result3.is_ok(),
    );

    // 通过实例2检查，第四次请求应该被拒绝
    let result4 = service2.check_and_record_with_config(&key, &config).await;
    chain.add_step(
        "keycompute-ratelimit",
        "instance2::check_and_record",
        "Instance 2 fourth request rejected",
        result4.is_err(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试多租户隔离 - 不同租户之间互不影响
#[tokio::test]
async fn test_multi_tenant_isolation() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();
    let test_id = generate_test_id();

    // 创建带测试隔离前缀的 Redis 限流服务
    let service =
        match RateLimitService::new_redis_with_prefix(&redis_url, format!("test-{}", test_id)) {
            Ok(s) => s,
            Err(_) => {
                println!("Warning: Redis not available, skipping test");
                return;
            }
        };

    // 创建两个不同租户的用户
    let tenant1 = Uuid::new_v4();
    let tenant2 = Uuid::new_v4();
    let user1 = Uuid::new_v4();
    let user2 = Uuid::new_v4();
    let api_key1 = Uuid::new_v4();
    let api_key2 = Uuid::new_v4();

    let key1 = RateLimitKey::new(tenant1, user1, api_key1);
    let key2 = RateLimitKey::new(tenant2, user2, api_key2);

    let config = RateLimitConfig::new(2, 1000);

    // 租户1达到限流
    let _ = service.check_and_record_with_config(&key1, &config).await;
    let _ = service.check_and_record_with_config(&key1, &config).await;

    // 租户1第三次请求应该被拒绝
    let result1_rejected = service.check_and_record_with_config(&key1, &config).await;
    chain.add_step(
        "keycompute-ratelimit",
        "tenant1::check_and_record",
        "Tenant 1 limit exceeded",
        result1_rejected.is_err(),
    );

    // 租户2的请求应该仍然通过（不受租户1影响）
    let result2_allowed = service.check_and_record_with_config(&key2, &config).await;
    chain.add_step(
        "keycompute-ratelimit",
        "tenant2::check_and_record",
        "Tenant 2 request allowed",
        result2_allowed.is_ok(),
    );

    // 验证计数隔离
    let count1 = service.get_rpm_count(&key1).await.unwrap();
    let count2 = service.get_rpm_count(&key2).await.unwrap();

    chain.add_step(
        "keycompute-ratelimit",
        "get_rpm_count",
        format!("Tenant 1 count: {}, Tenant 2 count: {}", count1, count2),
        count1 == 2 && count2 == 1,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试带前缀的 Redis 限流器
#[tokio::test]
async fn test_redis_ratelimit_with_prefix() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();
    let test_id = generate_test_id();

    // 创建带不同前缀的两个限流服务（包含测试ID确保隔离）
    let service1 = match RateLimitService::new_redis_with_prefix(
        &redis_url,
        format!("test-{}-prefix-1", test_id),
    ) {
        Ok(s) => s,
        Err(_) => {
            println!("Warning: Redis not available, skipping test");
            return;
        }
    };

    let service2 = match RateLimitService::new_redis_with_prefix(
        &redis_url,
        format!("test-{}-prefix-2", test_id),
    ) {
        Ok(s) => s,
        Err(_) => {
            println!("Warning: Redis not available, skipping test");
            return;
        }
    };

    // 使用相同的限流键
    let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let config = RateLimitConfig::new(1, 1000);

    // 通过 service1 记录请求
    let result1 = service1.check_and_record_with_config(&key, &config).await;
    chain.add_step(
        "keycompute-ratelimit",
        "service1::check_and_record",
        "Service 1 first request",
        result1.is_ok(),
    );

    // service1 第二次请求应该被拒绝
    let result1_rejected = service1.check_and_record_with_config(&key, &config).await;
    chain.add_step(
        "keycompute-ratelimit",
        "service1::check_and_record",
        "Service 1 second request rejected",
        result1_rejected.is_err(),
    );

    // service2 使用不同前缀，第一次请求应该仍然通过
    let result2 = service2.check_and_record_with_config(&key, &config).await;
    chain.add_step(
        "keycompute-ratelimit",
        "service2::check_and_record",
        "Service 2 first request (different prefix)",
        result2.is_ok(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试并发场景下的分布式限流
#[tokio::test]
async fn test_concurrent_distributed_ratelimit() {
    let mut chain = VerificationChain::new();
    let redis_url = get_redis_url();
    let test_id = generate_test_id();

    // 创建带测试隔离前缀的 Redis 限流服务
    let service =
        match RateLimitService::new_redis_with_prefix(&redis_url, format!("test-{}", test_id)) {
            Ok(s) => Arc::new(s),
            Err(_) => {
                println!("Warning: Redis not available, skipping test");
                return;
            }
        };

    // 使用相同的限流键
    let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let config = RateLimitConfig::new(5, 1000);

    // 并发发送 10 个请求
    let mut handles = vec![];
    for _ in 0..10 {
        let service_clone = Arc::clone(&service);
        let key_clone = key.clone();
        let config_clone = config.clone();
        let handle = tokio::spawn(async move {
            service_clone
                .check_and_record_with_config(&key_clone, &config_clone)
                .await
        });
        handles.push(handle);
    }

    // 收集结果
    let mut success_count = 0;
    let mut reject_count = 0;
    for handle in handles {
        match handle.await.unwrap() {
            Ok(_) => success_count += 1,
            Err(_) => reject_count += 1,
        }
    }

    chain.add_step(
        "keycompute-ratelimit",
        "concurrent::check_and_record",
        format!("Success: {}, Rejected: {}", success_count, reject_count),
        success_count == 5 && reject_count == 5,
    );

    // 验证最终计数
    let final_count = service.get_rpm_count(&key).await.unwrap();
    chain.add_step(
        "keycompute-ratelimit",
        "get_rpm_count",
        format!("Final count: {}", final_count),
        final_count == 5,
    );

    chain.print_report();
    assert!(chain.all_passed());
}
