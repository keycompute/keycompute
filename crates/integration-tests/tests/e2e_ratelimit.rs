//! RateLimit 模块端到端测试
//!
//! 验证限流模块在各场景下的行为

use integration_tests::common::VerificationChain;
use keycompute_ratelimit::{MemoryRateLimiter, RateLimitConfig, RateLimitKey, RateLimitService, RateLimiter};
use std::sync::Arc;
use uuid::Uuid;

/// 测试限流基础流程
#[tokio::test]
async fn test_ratelimit_basic_flow() {
    let mut chain = VerificationChain::new();

    // 1. 创建限流配置
    let config = RateLimitConfig {
        rpm_limit: 10,
        tpm_limit: 1000,
        concurrency_limit: 5,
    };
    chain.add_step(
        "keycompute-ratelimit",
        "RateLimitConfig::new",
        format!("RPM: {}, TPM: {}", config.rpm_limit, config.tpm_limit),
        config.rpm_limit == 10,
    );

    // 2. 创建内存限流器
    let limiter = Arc::new(MemoryRateLimiter::new(config.clone()));
    chain.add_step(
        "keycompute-ratelimit",
        "MemoryRateLimiter::new",
        "Memory rate limiter created",
        true,
    );

    // 3. 创建限流键
    let key = RateLimitKey::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    chain.add_step(
        "keycompute-ratelimit",
        "RateLimitKey::new",
        format!("Tenant: {:?}, User: {:?}", key.tenant_id, key.user_id),
        !key.tenant_id.is_nil(),
    );

    // 4. 检查限流（应该通过）
    let allowed = limiter.check(&key).await.unwrap();
    chain.add_step(
        "keycompute-ratelimit",
        "RateLimiter::check",
        format!("First check allowed: {}", allowed),
        allowed,
    );

    // 5. 记录请求
    limiter.record(&key).await.unwrap();
    chain.add_step(
        "keycompute-ratelimit",
        "RateLimiter::record",
        "Request recorded",
        true,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试限流服务
#[tokio::test]
async fn test_ratelimit_service() {
    let mut chain = VerificationChain::new();

    // 1. 创建限流服务
    let service = RateLimitService::default_memory();
    chain.add_step(
        "keycompute-ratelimit",
        "RateLimitService::default_memory",
        "Rate limit service created",
        true,
    );

    // 2. 创建限流键
    let key = RateLimitKey::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );

    // 3. 检查并记录
    let result = service.check_and_record(&key).await;
    chain.add_step(
        "keycompute-ratelimit",
        "RateLimitService::check_and_record",
        format!("Check and record result: {:?}", result.is_ok()),
        result.is_ok(),
    );

    // 4. 仅检查
    let allowed = service.check_only(&key).await.unwrap();
    chain.add_step(
        "keycompute-ratelimit",
        "RateLimitService::check_only",
        format!("Check only result: {}", allowed),
        allowed,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试多维度限流键
#[test]
fn test_ratelimit_key_dimensions() {
    let mut chain = VerificationChain::new();

    // 1. 测试不同维度的限流键
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let api_key_id = Uuid::new_v4();

    let key1 = RateLimitKey::new(tenant_id, user_id, api_key_id);
    let key2 = RateLimitKey::new(tenant_id, user_id, api_key_id);
    let key3 = RateLimitKey::new(tenant_id, Uuid::new_v4(), api_key_id);

    chain.add_step(
        "keycompute-ratelimit",
        "RateLimitKey::equality",
        "Same keys are equal",
        key1 == key2,
    );

    chain.add_step(
        "keycompute-ratelimit",
        "RateLimitKey::inequality",
        "Different user keys are not equal",
        key1 != key3,
    );

    // 2. 测试哈希
    use std::collections::HashMap;
    let mut map = HashMap::new();
    map.insert(key1.clone(), 1);
    map.insert(key2.clone(), 2);
    
    chain.add_step(
        "keycompute-ratelimit",
        "RateLimitKey::hash",
        format!("HashMap size: {} (should be 1 due to same key)", map.len()),
        map.len() == 1,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试限流配置边界
#[test]
fn test_ratelimit_config_boundaries() {
    let mut chain = VerificationChain::new();

    // 1. 默认配置
    let default_config = RateLimitConfig::default();
    chain.add_step(
        "keycompute-ratelimit",
        "RateLimitConfig::default_rpm",
        format!("Default RPM: {}", default_config.rpm_limit),
        default_config.rpm_limit == 60,
    );
    chain.add_step(
        "keycompute-ratelimit",
        "RateLimitConfig::default_tpm",
        format!("Default TPM: {}", default_config.tpm_limit),
        default_config.tpm_limit == 10000,
    );
    chain.add_step(
        "keycompute-ratelimit",
        "RateLimitConfig::default_concurrency",
        format!("Default concurrency: {}", default_config.concurrency_limit),
        default_config.concurrency_limit == 10,
    );

    // 2. 自义配置
    let custom_config = RateLimitConfig {
        rpm_limit: 100,
        tpm_limit: 50000,
        concurrency_limit: 20,
    };
    chain.add_step(
        "keycompute-ratelimit",
        "RateLimitConfig::custom",
        format!("Custom RPM: {}", custom_config.rpm_limit),
        custom_config.rpm_limit == 100,
    );

    chain.print_report();
    assert!(chain.all_passed());
}
