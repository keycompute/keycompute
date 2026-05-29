//! 计费计算器
//!
//! 计算用户应付金额：user_amount = (input_tokens/1000)*input_price + (output_tokens/1000)*output_price

use keycompute_types::PricingSnapshot;
use rust_decimal::Decimal;

/// 计算用户应付金额
///
/// # 公式
/// ```text
/// user_amount = (input_tokens/1000)*input_price + (output_tokens/1000)*output_price
/// ```
///
/// # 参数
/// - `input_tokens`: 输入 token 数量
/// - `output_tokens`: 输出 token 数量
/// - `pricing`: 价格快照（包含 input/output 单价）
///
/// # 返回
/// 计算后的应付金额（Decimal）
pub fn calculate_amount(
    input_tokens: u32,
    output_tokens: u32,
    pricing: &PricingSnapshot,
) -> Decimal {
    let input_cost = Decimal::from(input_tokens) / Decimal::from(1000) * pricing.input_price_per_1k;
    let output_cost =
        Decimal::from(output_tokens) / Decimal::from(1000) * pricing.output_price_per_1k;
    // 统一精度 10 位小数，与 usage_logs(user_amount DECIMAL(20,10)) 对齐
    (input_cost + output_cost).round_dp(10)
}

/// 计算上游成本（与计算用户金额使用相同公式）
///
/// 用于计算实际支付给上游 Provider 的成本
pub fn calculate_upstream_cost(
    input_tokens: u32,
    output_tokens: u32,
    upstream_input_price: Decimal,
    upstream_output_price: Decimal,
) -> Decimal {
    let input_cost = Decimal::from(input_tokens) / Decimal::from(1000) * upstream_input_price;
    let output_cost = Decimal::from(output_tokens) / Decimal::from(1000) * upstream_output_price;
    input_cost + output_cost
}

/// 计算毛利
///
/// profit = user_amount - upstream_cost
pub fn calculate_profit(user_amount: Decimal, upstream_cost: Decimal) -> Decimal {
    user_amount - upstream_cost
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn create_test_pricing() -> PricingSnapshot {
        PricingSnapshot {
            model_name: "gpt-4o".to_string(),
            currency: "CNY".to_string(),
            input_price_per_1k: Decimal::from(1), // 1元/1k tokens
            output_price_per_1k: Decimal::from(2), // 2元/1k tokens
        }
    }

    #[test]
    fn test_calculate_amount() {
        let pricing = create_test_pricing();

        // 1000 input + 500 output
        // = (1000/1000)*1 + (500/1000)*2
        // = 1 + 1 = 2
        let amount = calculate_amount(1000, 500, &pricing);
        assert_eq!(amount, Decimal::from(2));
    }

    #[test]
    fn test_calculate_amount_zero_tokens() {
        let pricing = create_test_pricing();

        let amount = calculate_amount(0, 0, &pricing);
        assert_eq!(amount, Decimal::from(0));
    }

    #[test]
    fn test_calculate_upstream_cost() {
        // 上游价格通常比用户价格低
        let upstream_input = Decimal::from_f64_retain(0.5).unwrap();
        let upstream_output = Decimal::from_f64_retain(1.0).unwrap();

        // 1000 input + 500 output
        // = (1000/1000)*0.5 + (500/1000)*1.0
        // = 0.5 + 0.5 = 1.0
        let cost = calculate_upstream_cost(1000, 500, upstream_input, upstream_output);
        assert_eq!(cost, Decimal::from(1));
    }

    #[test]
    fn test_calculate_profit() {
        let user_amount = Decimal::from(10);
        let upstream_cost = Decimal::from(6);

        let profit = calculate_profit(user_amount, upstream_cost);
        assert_eq!(profit, Decimal::from(4));
    }
}
