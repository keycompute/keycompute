//! 分销计算器
//!
//! 根据规则计算分成金额

use crate::DistributionLevel;
use rust_decimal::Decimal;
use uuid::Uuid;

/// 分销分成
#[derive(Debug, Clone)]
pub struct DistributionShare {
    /// 受益人 ID
    pub beneficiary_id: Uuid,
    /// 分成金额
    pub share_amount: Decimal,
    /// 分成比例
    pub share_ratio: Decimal,
    /// 分销层级
    pub level: DistributionLevel,
}

impl DistributionShare {
    /// 创建新的分成记录
    pub fn new(
        beneficiary_id: Uuid,
        share_amount: Decimal,
        share_ratio: Decimal,
        level: DistributionLevel,
    ) -> Self {
        Self {
            beneficiary_id,
            share_amount,
            share_ratio,
            level,
        }
    }
}

/// 计算分成
///
/// 根据二级分销规则计算各方分成
///
/// # 参数
/// - `user_amount`: 用户应付金额
/// - `level1_ratio`: 第一级分成比例（如 0.1 表示 10%）
/// - `level2_ratio`: 第二级分成比例（如 0.05 表示 5%）
/// - `level1_beneficiary`: 第一级受益人 ID
/// - `level2_beneficiary`: 第二级受益人 ID（可选）
///
/// # 返回
/// 分成记录列表
pub fn calculate_shares(
    user_amount: Decimal,
    level1_ratio: Decimal,
    level2_ratio: Decimal,
    level1_beneficiary: Uuid,
    level2_beneficiary: Option<Uuid>,
) -> Vec<DistributionShare> {
    let mut shares = Vec::new();

    // 第一级分成（统一精度 10 位小数，与 distribution_records.share_amount DECIMAL(20,10) 对齐）
    let level1_amount = (user_amount * level1_ratio).round_dp(10);
    shares.push(DistributionShare::new(
        level1_beneficiary,
        level1_amount,
        level1_ratio,
        DistributionLevel::Level1,
    ));

    // 第二级分成（如果有）
    if let Some(beneficiary_id) = level2_beneficiary {
        let level2_amount = (user_amount * level2_ratio).round_dp(10);
        shares.push(DistributionShare::new(
            beneficiary_id,
            level2_amount,
            level2_ratio,
            DistributionLevel::Level2,
        ));
    }

    shares
}

/// 计算总分成金额
///
/// 用于验证分成总额不超过用户金额
pub fn calculate_total_share(shares: &[DistributionShare]) -> Decimal {
    shares.iter().map(|s| s.share_amount).sum()
}

/// 验证分成比例
///
/// 确保分成总额不超过用户金额的一定比例（如 30%）
pub fn validate_share_ratio(total_share_ratio: Decimal, max_ratio: Decimal) -> bool {
    total_share_ratio <= max_ratio
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    // 辅助函数：创建精确的小数（避免浮点精度问题）
    fn dec(ratio: i64, div: i64) -> Decimal {
        Decimal::from(ratio) / Decimal::from(div)
    }

    #[test]
    fn test_calculate_shares_level1_only() {
        let user_amount = Decimal::from(100);
        let level1_ratio = dec(1, 10); // 0.1 = 1/10
        let level2_ratio = dec(5, 100); // 0.05 = 5/100
        let level1_beneficiary = Uuid::new_v4();

        let shares = calculate_shares(
            user_amount,
            level1_ratio,
            level2_ratio,
            level1_beneficiary,
            None,
        );

        assert_eq!(shares.len(), 1);
        assert_eq!(shares[0].share_amount, Decimal::from(10)); // 100 * 0.1
        assert_eq!(shares[0].share_ratio, level1_ratio);
        assert_eq!(shares[0].level, DistributionLevel::Level1);
    }

    #[test]
    fn test_calculate_shares_two_levels() {
        let user_amount = Decimal::from(100);
        let level1_ratio = dec(1, 10); // 0.1 = 1/10
        let level2_ratio = dec(5, 100); // 0.05 = 5/100
        let level1_beneficiary = Uuid::new_v4();
        let level2_beneficiary = Uuid::new_v4();

        let shares = calculate_shares(
            user_amount,
            level1_ratio,
            level2_ratio,
            level1_beneficiary,
            Some(level2_beneficiary),
        );

        assert_eq!(shares.len(), 2);

        // Level 1
        assert_eq!(shares[0].share_amount, Decimal::from(10)); // 100 * 0.1
        assert_eq!(shares[0].beneficiary_id, level1_beneficiary);
        assert_eq!(shares[0].level, DistributionLevel::Level1);

        // Level 2
        assert_eq!(shares[1].share_amount, Decimal::from(5)); // 100 * 0.05
        assert_eq!(shares[1].beneficiary_id, level2_beneficiary);
        assert_eq!(shares[1].level, DistributionLevel::Level2);
    }

    #[test]
    fn test_calculate_total_share() {
        let user_amount = Decimal::from(100);
        let level1_ratio = dec(1, 10);
        let level2_ratio = dec(5, 100);
        let level1_beneficiary = Uuid::new_v4();
        let level2_beneficiary = Uuid::new_v4();

        let shares = calculate_shares(
            user_amount,
            level1_ratio,
            level2_ratio,
            level1_beneficiary,
            Some(level2_beneficiary),
        );

        let total = calculate_total_share(&shares);
        assert_eq!(total, Decimal::from(15)); // 10 + 5
    }

    #[test]
    fn test_validate_share_ratio() {
        let valid = dec(25, 100); // 0.25 = 25%
        let max = dec(30, 100); // 0.30 = 30%
        assert!(validate_share_ratio(valid, max));

        let invalid = Decimal::from_f64_retain(0.35).unwrap(); // 35%
        assert!(!validate_share_ratio(invalid, max));
    }
}
