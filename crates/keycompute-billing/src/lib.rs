//! Billing Module
//!
//! 计费模块，仅在 stream 结束后执行。
//! 架构约束：不参与路由，不预扣余额，不反向影响执行结果。

pub mod balance;
pub mod calculator;
pub mod usage_log;
pub mod usage_source;

pub use balance::{BalanceService, min_balance_threshold};
pub use calculator::{calculate_amount, calculate_amount_with_prices, calculate_breakdown};
pub use usage_log::{BillingService, NewUsageLog};
pub use usage_source::UsageSource;

use keycompute_types::RequestContext;
use rust_decimal::Decimal;

/// 计费触发器
///
/// 流结束后执行（唯一调用点在 Gateway）
pub struct BillingTrigger;

impl BillingTrigger {
    /// 创建新的计费触发器
    pub fn new() -> Self {
        Self
    }

    /// 触发计费结算
    ///
    /// 输入: usage + pricing_snapshot + request metadata
    /// 输出: 同步写入不可变 usage_logs 主账本
    ///
    /// # 错误
    /// - 如果计费结算失败，返回错误而不 panic
    pub async fn trigger(
        &self,
        ctx: &RequestContext,
        provider_name: &str,
        account_id: uuid::Uuid,
        status: &str,
        billing: &BillingService,
    ) -> keycompute_types::Result<crate::usage_log::NewUsageLog> {
        billing
            .finalize(ctx, provider_name, account_id, status)
            .await
    }
}

impl Default for BillingTrigger {
    fn default() -> Self {
        Self::new()
    }
}

/// 账单状态
#[derive(Debug, Clone, PartialEq)]
pub enum BillingStatus {
    /// 成功完成
    Success,
    /// 部分完成（流中断但已产生内容）
    Partial,
    /// 上游错误
    UpstreamError,
}

impl BillingStatus {
    /// 转换为字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            BillingStatus::Success => "success",
            BillingStatus::Partial => "partial",
            BillingStatus::UpstreamError => "upstream_error",
        }
    }
}

/// 计算用户应付金额
///
/// user_amount = (input_tokens/1000)*input_price + (output_tokens/1000)*output_price
pub fn compute_user_amount(
    input_tokens: u32,
    output_tokens: u32,
    input_price_per_1k: Decimal,
    output_price_per_1k: Decimal,
) -> Decimal {
    calculate_amount_with_prices(
        input_tokens,
        output_tokens,
        input_price_per_1k,
        output_price_per_1k,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    #[test]
    fn test_billing_status() {
        assert_eq!(BillingStatus::Success.as_str(), "success");
        assert_eq!(BillingStatus::Partial.as_str(), "partial");
        assert_eq!(BillingStatus::UpstreamError.as_str(), "upstream_error");
    }

    #[test]
    fn test_compute_user_amount() {
        let input_price = Decimal::from(1); // 1元/1k tokens
        let output_price = Decimal::from(2); // 2元/1k tokens

        let amount = compute_user_amount(1000, 500, input_price, output_price);
        assert_eq!(amount, Decimal::from(2)); // 1*1 + 2*0.5 = 2
    }
}
