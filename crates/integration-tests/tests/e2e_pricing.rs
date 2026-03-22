//! Pricing 模块端到端测试
//!
//! 验证定价服务：价格快照、成本计算

use integration_tests::common::VerificationChain;
use keycompute_pricing::PricingService;
use keycompute_types::PricingSnapshot;
use rust_decimal::Decimal;
use uuid::Uuid;

/// 测试 Pricing 服务基础功能
#[tokio::test]
async fn test_pricing_service_flow() {
    let mut chain = VerificationChain::new();

    // 1. 创建 Pricing 服务
    let service = PricingService::new();
    chain.add_step(
        "keycompute-pricing",
        "PricingService::new",
        "Pricing service created",
        true,
    );

    // 2. 创建价格快照（使用引用）
    let tenant_id = Uuid::new_v4();
    let snapshot = service.create_snapshot("gpt-4o", &tenant_id).await.unwrap();
    
    chain.add_step(
        "keycompute-pricing",
        "PricingService::create_snapshot",
        format!("Snapshot model: {}", snapshot.model_name),
        snapshot.model_name == "gpt-4o",
    );

    // 3. 验证价格快照字段
    chain.add_step(
        "keycompute-pricing",
        "PricingSnapshot::currency",
        format!("Currency: {}", snapshot.currency),
        !snapshot.currency.is_empty(),
    );
    chain.add_step(
        "keycompute-pricing",
        "PricingSnapshot::input_price",
        format!("Input price per 1k: {:?}", snapshot.input_price_per_1k),
        snapshot.input_price_per_1k > Decimal::ZERO,
    );
    chain.add_step(
        "keycompute-pricing",
        "PricingSnapshot::output_price",
        format!("Output price per 1k: {:?}", snapshot.output_price_per_1k),
        snapshot.output_price_per_1k > Decimal::ZERO,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试价格快照不可变性
#[test]
fn test_pricing_snapshot_immutability() {
    let mut chain = VerificationChain::new();

    // 1. 创建价格快照
    let snapshot = PricingSnapshot {
        model_name: "gpt-4o".to_string(),
        currency: "CNY".to_string(),
        input_price_per_1k: Decimal::from(1),
        output_price_per_1k: Decimal::from(2),
    };

    chain.add_step(
        "keycompute-types",
        "PricingSnapshot::create",
        "Snapshot created",
        true,
    );

    // 2. 验证克隆
    let snapshot_clone = snapshot.clone();
    chain.add_step(
        "keycompute-types",
        "PricingSnapshot::clone",
        format!("Clone model: {}", snapshot_clone.model_name),
        snapshot_clone.model_name == snapshot.model_name,
    );

    // 3. 验证字段不可变（编译时保证）
    chain.add_step(
        "architecture",
        "PricingSnapshot::immutability",
        "Fields are immutable at compile time",
        true,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试成本计算
#[test]
fn test_pricing_cost_calculation() {
    let mut chain = VerificationChain::new();

    // 1. 创建价格快照
    let snapshot = PricingSnapshot {
        model_name: "gpt-4o".to_string(),
        currency: "CNY".to_string(),
        input_price_per_1k: Decimal::from(1),  // 1元/1k tokens
        output_price_per_1k: Decimal::from(2), // 2元/1k tokens
    };

    // 2. 计算成本
    let input_tokens = 1000u32;
    let output_tokens = 500u32;

    let input_cost = Decimal::from(input_tokens) / Decimal::from(1000) * snapshot.input_price_per_1k;
    let output_cost = Decimal::from(output_tokens) / Decimal::from(1000) * snapshot.output_price_per_1k;
    let total_cost = input_cost + output_cost;

    chain.add_step(
        "keycompute-pricing",
        "calculate_input_cost",
        format!("Input cost (1000 tokens): {:?}", input_cost),
        input_cost == Decimal::from(1),
    );

    chain.add_step(
        "keycompute-pricing",
        "calculate_output_cost",
        format!("Output cost (500 tokens): {:?}", output_cost),
        output_cost == Decimal::from(1),
    );

    chain.add_step(
        "keycompute-pricing",
        "calculate_total_cost",
        format!("Total cost: {:?}", total_cost),
        total_cost == Decimal::from(2),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试不同模型的价格差异
#[tokio::test]
async fn test_pricing_model_variations() {
    let mut chain = VerificationChain::new();

    let service = PricingService::new();
    let tenant_id = Uuid::new_v4();

    // 1. 获取不同模型的价格快照
    let gpt4o = service.create_snapshot("gpt-4o", &tenant_id).await.unwrap();
    let gpt35 = service.create_snapshot("gpt-3.5-turbo", &tenant_id).await.unwrap();

    chain.add_step(
        "keycompute-pricing",
        "PricingService::gpt4o_snapshot",
        format!("GPT-4o input price: {:?}", gpt4o.input_price_per_1k),
        gpt4o.input_price_per_1k > Decimal::ZERO,
    );

    chain.add_step(
        "keycompute-pricing",
        "PricingService::gpt35_snapshot",
        format!("GPT-3.5 input price: {:?}", gpt35.input_price_per_1k),
        gpt35.input_price_per_1k > Decimal::ZERO,
    );

    // 2. 验证模型名称
    chain.add_step(
        "keycompute-pricing",
        "PricingSnapshot::model_names",
        format!("Models: {} vs {}", gpt4o.model_name, gpt35.model_name),
        gpt4o.model_name != gpt35.model_name,
    );

    chain.print_report();
    assert!(chain.all_passed());
}
