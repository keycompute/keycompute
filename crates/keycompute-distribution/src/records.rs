//! 分销记录
//!
//! 写入 distribution_records

use crate::{DistributionContext, DistributionLevel, calculator::DistributionShare};
use chrono::{DateTime, Utc};
use keycompute_db::CreateDistributionRecordRequest;
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

/// 分销记录
///
/// 对应 distribution_records 表的字段
#[derive(Debug, Clone)]
pub struct DistributionRecord {
    /// 记录 ID
    pub id: Uuid,
    /// 关联的 usage_log ID
    pub usage_log_id: Uuid,
    /// 租户 ID
    pub tenant_id: Uuid,
    /// 受益人 ID
    pub beneficiary_id: Uuid,
    /// 分成金额
    pub share_amount: Decimal,
    /// 分成比例
    pub share_ratio: Decimal,
    /// 分销层级
    pub level: String,
    /// 状态
    pub status: String,
    /// 创建时间
    pub created_at: DateTime<Utc>,
}

impl DistributionRecord {
    /// 从分成记录创建分销记录
    pub fn from_share(ctx: &DistributionContext, share: &DistributionShare) -> Self {
        Self {
            id: Uuid::new_v4(),
            usage_log_id: ctx.usage_log_id,
            tenant_id: ctx.tenant_id,
            beneficiary_id: share.beneficiary_id,
            share_amount: share.share_amount,
            share_ratio: share.share_ratio,
            level: share.level.as_str().to_string(),
            status: "pending".to_string(),
            created_at: Utc::now(),
        }
    }

    /// 标记为已结算
    pub fn mark_settled(&mut self) {
        self.status = "settled".to_string();
    }

    /// 标记为已取消
    pub fn mark_cancelled(&mut self) {
        self.status = "cancelled".to_string();
    }
}

/// 分销服务
#[derive(Clone)]
pub struct DistributionService {
    /// 数据库连接池（可选）
    pool: Option<Arc<PgPool>>,
}

impl std::fmt::Debug for DistributionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DistributionService")
            .field("pool", &self.pool.as_ref().map(|_| "PgPool"))
            .finish()
    }
}

impl DistributionService {
    /// 创建新的分销服务（无数据库连接）
    pub fn new() -> Self {
        Self { pool: None }
    }

    /// 创建带数据库连接的分销服务
    pub fn with_pool(pool: Arc<PgPool>) -> Self {
        Self { pool: Some(pool) }
    }

    /// 处理分销
    ///
    /// 根据分成记录生成分销记录
    pub fn process_distribution(
        &self,
        ctx: &DistributionContext,
        shares: &[DistributionShare],
    ) -> Vec<DistributionRecord> {
        shares
            .iter()
            .map(|share| DistributionRecord::from_share(ctx, share))
            .collect()
    }

    /// 计算总分成金额
    pub fn calculate_total_distribution(&self, records: &[DistributionRecord]) -> Decimal {
        records.iter().map(|r| r.share_amount).sum()
    }

    /// 验证分成总额
    ///
    /// 确保分成总额不超过用户金额的一定比例
    pub fn validate_distribution(
        &self,
        ctx: &DistributionContext,
        records: &[DistributionRecord],
        max_ratio: Decimal,
    ) -> bool {
        let total_share: Decimal = records.iter().map(|r| r.share_amount).sum();
        let max_amount = ctx.user_amount * max_ratio;
        total_share <= max_amount
    }

    /// 处理分销并保存到数据库
    ///
    /// 根据分成记录生成分销记录并写入数据库
    pub async fn process_and_save(
        &self,
        ctx: &DistributionContext,
        shares: &[DistributionShare],
    ) -> keycompute_types::Result<Vec<keycompute_db::DistributionRecord>> {
        // 生成分销记录
        let records = self.process_distribution(ctx, shares);

        // 如果没有数据库连接，返回空
        let Some(pool) = &self.pool else {
            tracing::debug!("No database pool, skipping distribution save");
            return Ok(vec![]);
        };

        // 转换为数据库请求并保存
        let mut saved_records = Vec::with_capacity(records.len());
        for record in records {
            let req = CreateDistributionRecordRequest {
                usage_log_id: record.usage_log_id,
                tenant_id: record.tenant_id,
                beneficiary_id: record.beneficiary_id,
                share_amount: decimal_to_bigdecimal(&record.share_amount),
                share_ratio: decimal_to_bigdecimal(&record.share_ratio),
            };

            match keycompute_db::DistributionRecord::create(pool, &req).await {
                Ok(saved) => {
                    tracing::info!(
                        usage_log_id = %record.usage_log_id,
                        beneficiary_id = %record.beneficiary_id,
                        share_amount = %record.share_amount,
                        "Distribution record saved"
                    );
                    saved_records.push(saved);
                }
                Err(e) => {
                    tracing::error!(
                        usage_log_id = %record.usage_log_id,
                        error = %e,
                        "Failed to save distribution record"
                    );
                    return Err(keycompute_types::KeyComputeError::DatabaseError(format!(
                        "Failed to save distribution record: {}",
                        e
                    )));
                }
            }
        }

        Ok(saved_records)
    }
}

/// 将 Decimal 转换为 BigDecimal
fn decimal_to_bigdecimal(value: &Decimal) -> bigdecimal::BigDecimal {
    let s = value.to_string();
    s.parse().unwrap_or(bigdecimal::BigDecimal::from(0))
}

/// 分销记录构建器
#[derive(Debug)]
pub struct DistributionRecordBuilder {
    usage_log_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    beneficiary_id: Option<Uuid>,
    share_amount: Option<Decimal>,
    share_ratio: Option<Decimal>,
    level: Option<DistributionLevel>,
}

impl DistributionRecordBuilder {
    /// 创建新的构建器
    pub fn new() -> Self {
        Self {
            usage_log_id: None,
            tenant_id: None,
            beneficiary_id: None,
            share_amount: None,
            share_ratio: None,
            level: None,
        }
    }

    /// 设置 usage_log ID
    pub fn usage_log_id(mut self, id: Uuid) -> Self {
        self.usage_log_id = Some(id);
        self
    }

    /// 设置租户 ID
    pub fn tenant_id(mut self, id: Uuid) -> Self {
        self.tenant_id = Some(id);
        self
    }

    /// 设置受益人 ID
    pub fn beneficiary_id(mut self, id: Uuid) -> Self {
        self.beneficiary_id = Some(id);
        self
    }

    /// 设置分成金额
    pub fn share_amount(mut self, amount: Decimal) -> Self {
        self.share_amount = Some(amount);
        self
    }

    /// 设置分成比例
    pub fn share_ratio(mut self, ratio: Decimal) -> Self {
        self.share_ratio = Some(ratio);
        self
    }

    /// 设置层级
    pub fn level(mut self, level: DistributionLevel) -> Self {
        self.level = Some(level);
        self
    }

    /// 构建分销记录
    pub fn build(self) -> Option<DistributionRecord> {
        Some(DistributionRecord {
            id: Uuid::new_v4(),
            usage_log_id: self.usage_log_id?,
            tenant_id: self.tenant_id?,
            beneficiary_id: self.beneficiary_id?,
            share_amount: self.share_amount?,
            share_ratio: self.share_ratio?,
            level: self.level?.as_str().to_string(),
            status: "pending".to_string(),
            created_at: Utc::now(),
        })
    }
}

impl Default for DistributionRecordBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calculator::calculate_shares;
    use rust_decimal::Decimal;

    #[test]
    fn test_distribution_record_from_share() {
        let usage_log_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();
        let ctx = DistributionContext::new(usage_log_id, tenant_id, Decimal::from(100), "CNY");

        let beneficiary_id = Uuid::new_v4();
        let share = DistributionShare::new(
            beneficiary_id,
            Decimal::from(10),
            Decimal::from_f64_retain(0.1).unwrap(),
            DistributionLevel::Level1,
        );

        let record = DistributionRecord::from_share(&ctx, &share);

        assert_eq!(record.usage_log_id, usage_log_id);
        assert_eq!(record.tenant_id, tenant_id);
        assert_eq!(record.beneficiary_id, beneficiary_id);
        assert_eq!(record.share_amount, Decimal::from(10));
        assert_eq!(record.level, "level1");
        assert_eq!(record.status, "pending");
    }

    #[test]
    fn test_distribution_record_status() {
        let usage_log_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();
        let ctx = DistributionContext::new(usage_log_id, tenant_id, Decimal::from(100), "CNY");

        let share = DistributionShare::new(
            Uuid::new_v4(),
            Decimal::from(10),
            Decimal::from_f64_retain(0.1).unwrap(),
            DistributionLevel::Level1,
        );

        let mut record = DistributionRecord::from_share(&ctx, &share);
        assert_eq!(record.status, "pending");

        record.mark_settled();
        assert_eq!(record.status, "settled");

        record.mark_cancelled();
        assert_eq!(record.status, "cancelled");
    }

    #[test]
    fn test_distribution_service_process() {
        let usage_log_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();
        let ctx = DistributionContext::new(usage_log_id, tenant_id, Decimal::from(100), "CNY");

        let shares = calculate_shares(
            Decimal::from(100),
            Decimal::from_f64_retain(0.1).unwrap(),
            Decimal::from_f64_retain(0.05).unwrap(),
            Uuid::new_v4(),
            Some(Uuid::new_v4()),
        );

        let service = DistributionService::new();
        let records = service.process_distribution(&ctx, &shares);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].level, "level1");
        assert_eq!(records[1].level, "level2");
    }

    #[test]
    fn test_distribution_service_calculate_total() {
        let records = vec![
            DistributionRecordBuilder::new()
                .usage_log_id(Uuid::new_v4())
                .tenant_id(Uuid::new_v4())
                .beneficiary_id(Uuid::new_v4())
                .share_amount(Decimal::from(10))
                .share_ratio(Decimal::from_f64_retain(0.1).unwrap())
                .level(DistributionLevel::Level1)
                .build()
                .unwrap(),
            DistributionRecordBuilder::new()
                .usage_log_id(Uuid::new_v4())
                .tenant_id(Uuid::new_v4())
                .beneficiary_id(Uuid::new_v4())
                .share_amount(Decimal::from(5))
                .share_ratio(Decimal::from_f64_retain(0.05).unwrap())
                .level(DistributionLevel::Level2)
                .build()
                .unwrap(),
        ];

        let service = DistributionService::new();
        let total = service.calculate_total_distribution(&records);
        assert_eq!(total, Decimal::from(15));
    }

    #[test]
    fn test_distribution_service_validate() {
        let usage_log_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();
        let ctx = DistributionContext::new(usage_log_id, tenant_id, Decimal::from(100), "CNY");

        let records = vec![
            DistributionRecordBuilder::new()
                .usage_log_id(usage_log_id)
                .tenant_id(tenant_id)
                .beneficiary_id(Uuid::new_v4())
                .share_amount(Decimal::from(20)) // 20% of 100
                .share_ratio(Decimal::from_f64_retain(0.2).unwrap())
                .level(DistributionLevel::Level1)
                .build()
                .unwrap(),
        ];

        let service = DistributionService::new();

        // 20 <= 30% of 100 = 30, should be valid
        assert!(service.validate_distribution(
            &ctx,
            &records,
            Decimal::from_f64_retain(0.30).unwrap()
        ));

        // 20 > 15% of 100 = 15, should be invalid
        assert!(!service.validate_distribution(
            &ctx,
            &records,
            Decimal::from_f64_retain(0.15).unwrap()
        ));
    }
}
