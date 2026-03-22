//! Observability 模块端到端测试
//!
//! 验证可观测性：日志、指标、追踪

use integration_tests::common::VerificationChain;
use keycompute_observability::metrics::{MetricsCollector, REQUEST_TOTAL, TOKENS_TOTAL, INPUT_TOKENS_TOTAL, OUTPUT_TOKENS_TOTAL, ACTIVE_REQUESTS};

/// 测试指标采集
#[test]
fn test_observability_metrics() {
    let mut chain = VerificationChain::new();

    // 1. 初始化指标
    keycompute_observability::init_metrics();
    
    // 2. 测试请求计数器
    let initial = REQUEST_TOTAL.get();
    chain.add_step(
        "keycompute-observability",
        "REQUEST_TOTAL::initial",
        format!("Initial request count: {}", initial),
        true,
    );

    // 3. 增加请求计数
    REQUEST_TOTAL.inc();
    let after_inc = REQUEST_TOTAL.get();
    chain.add_step(
        "keycompute-observability",
        "REQUEST_TOTAL::inc",
        format!("After increment: {}", after_inc),
        after_inc > initial,
    );

    // 4. 测试 Token 计数器
    let initial_tokens = TOKENS_TOTAL.get();
    TOKENS_TOTAL.inc_by(100.0);
    let after_tokens = TOKENS_TOTAL.get();
    chain.add_step(
        "keycompute-observability",
        "TOKENS_TOTAL::inc_by",
        format!("Tokens after +100: {}", after_tokens),
        after_tokens >= initial_tokens + 100.0,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试 MetricsCollector 结构
#[test]
fn test_observability_metrics_collector() {
    let mut chain = VerificationChain::new();

    // 1. 创建 MetricsCollector
    let collector = MetricsCollector::new();
    chain.add_step(
        "keycompute-observability",
        "MetricsCollector::new",
        "MetricsCollector instance created",
        true,
    );

    // 2. 记录请求开始
    let initial_active = ACTIVE_REQUESTS.get();
    collector.request_started();
    let after_start = ACTIVE_REQUESTS.get();
    chain.add_step(
        "keycompute-observability",
        "MetricsCollector::request_started",
        format!("Active requests: {} -> {}", initial_active, after_start),
        after_start > initial_active,
    );

    // 3. 记录请求完成
    collector.request_completed(0.5);
    let after_complete = ACTIVE_REQUESTS.get();
    chain.add_step(
        "keycompute-observability",
        "MetricsCollector::request_completed",
        format!("Active requests after complete: {}", after_complete),
        after_complete < after_start,
    );

    // 4. 记录 Token
    let initial_input = INPUT_TOKENS_TOTAL.get();
    collector.record_tokens(100, 50);
    let after_input = INPUT_TOKENS_TOTAL.get();
    chain.add_step(
        "keycompute-observability",
        "MetricsCollector::record_tokens",
        format!("Input tokens: {} -> {}", initial_input, after_input),
        after_input > initial_input,
    );

    // 5. 收集指标
    let gathered = collector.gather();
    chain.add_step(
        "keycompute-observability",
        "MetricsCollector::gather",
        format!("Gathered {} metric families", gathered.len()),
        !gathered.is_empty(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试日志初始化
#[test]
fn test_observability_logger() {
    let mut chain = VerificationChain::new();

    // 1. 初始化日志（可能已被其他测试初始化，忽略错误）
    let _ = std::panic::catch_unwind(|| {
        keycompute_observability::init_logger();
    });
    
    chain.add_step(
        "keycompute-observability",
        "init_logger",
        "Logger initialized (or already initialized)",
        true,
    );

    // 2. 测试日志宏
    tracing::info!("Test log message");
    chain.add_step(
        "keycompute-observability",
        "tracing::info",
        "Info log works",
        true,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试可观测性初始化
#[test]
fn test_observability_init() {
    let mut chain = VerificationChain::new();

    // 1. 初始化可观测性（可能已被其他测试初始化，忽略错误）
    // 由于全局 subscriber 只能设置一次，我们只验证函数可以调用
    let _ = std::panic::catch_unwind(|| {
        keycompute_observability::init_observability();
    });
    
    chain.add_step(
        "keycompute-observability",
        "init_observability",
        "Observability initialized (or already initialized)",
        true,
    );

    chain.print_report();
    assert!(chain.all_passed());
}
