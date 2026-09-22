//! 分销模块
//!
//! 处理分销收益、推荐关系、分销规则等

use crate::client::ApiClient;
use crate::error::Result;
use serde::{Deserialize, Deserializer, Serialize};

pub use super::common::{MessageResponse, encode_query_value};

/// 分销 API 客户端
#[derive(Debug, Clone)]
pub struct DistributionApi {
    client: ApiClient,
}

impl DistributionApi {
    /// 创建新的分销 API 客户端
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    pub async fn get_my_overview(&self, token: &str) -> Result<DistributionOverview> {
        self.client
            .get_json("/api/v1/me/distribution/overview", Some(token))
            .await
    }

    // ==================== 用户端接口 ====================

    /// 获取我的分销收益
    pub async fn get_my_distribution_earnings(&self, token: &str) -> Result<DistributionEarnings> {
        self.client
            .get_json("/api/v1/me/distribution/earnings", Some(token))
            .await
    }

    /// 获取我的推荐列表
    pub async fn get_my_referrals(&self, token: &str) -> Result<Vec<ReferralInfo>> {
        self.client
            .get_json("/api/v1/me/distribution/referrals", Some(token))
            .await
    }

    /// Read one explicit server page. The legacy Vec method is capped at the
    /// server's first default page and never implicitly downloads every user.
    pub async fn get_my_referrals_page(
        &self,
        page: u32,
        page_size: u32,
        token: &str,
    ) -> Result<ReferralPage> {
        self.client
            .get_json(
                &format!("/api/v1/me/distribution/referrals?page={page}&page_size={page_size}"),
                Some(token),
            )
            .await
    }

    /// 获取我的推荐码
    pub async fn get_my_referral_code(&self, token: &str) -> Result<ReferralCodeResponse> {
        self.client
            .get_json("/api/v1/me/referral/code", Some(token))
            .await
    }

    /// 生成邀请链接
    pub async fn generate_invite_link(&self, token: &str) -> Result<InviteLinkResponse> {
        self.client
            .post_json(
                "/api/v1/me/referral/invite-link",
                &serde_json::json!({}),
                Some(token),
            )
            .await
    }

    // ==================== Admin 端接口 ====================

    /// 获取分销记录列表（Admin）
    pub async fn list_distribution_records(
        &self,
        params: Option<&DistributionQueryParams>,
        token: &str,
    ) -> Result<Vec<DistributionRecord>> {
        let path = if let Some(p) = params {
            format!("/api/v1/distribution/records?{}", p.to_query_string())
        } else {
            "/api/v1/distribution/records".to_string()
        };
        self.client.get_json(&path, Some(token)).await
    }

    /// 获取一页分销记录（现代分页接口）。
    pub async fn list_distribution_records_page(
        &self,
        params: &DistributionQueryParams,
        token: &str,
    ) -> Result<DistributionRecordPage> {
        let path = format!("/api/v1/distribution/records?{}", params.to_query_string());
        self.client.get_json(&path, Some(token)).await
    }

    /// 获取分销统计（Admin）
    pub async fn get_distribution_stats(&self, token: &str) -> Result<DistributionStats> {
        self.client
            .get_json("/api/v1/distribution/stats", Some(token))
            .await
    }

    /// 获取分销规则列表（Admin）
    pub async fn list_distribution_rules(&self, token: &str) -> Result<Vec<DistributionRule>> {
        self.client
            .get_json("/api/v1/distribution/rules", Some(token))
            .await
    }

    /// 创建分销规则（Admin）
    pub async fn create_distribution_rule(
        &self,
        req: &CreateDistributionRuleRequest,
        token: &str,
    ) -> Result<DistributionRule> {
        self.client
            .post_json("/api/v1/distribution/rules", req, Some(token))
            .await
    }

    /// 更新分销规则（Admin）
    pub async fn update_distribution_rule(
        &self,
        id: &str,
        req: &UpdateDistributionRuleRequest,
        token: &str,
    ) -> Result<DistributionRule> {
        self.client
            .put_json(
                &format!("/api/v1/distribution/rules/{}", id),
                req,
                Some(token),
            )
            .await
    }

    /// 删除分销规则（Admin）
    pub async fn delete_distribution_rule(&self, id: &str, token: &str) -> Result<MessageResponse> {
        self.client
            .delete_json(&format!("/api/v1/distribution/rules/{}", id), Some(token))
            .await
    }
}

/// 分销收益
#[derive(Debug, Clone, Deserialize)]
pub struct DistributionEarnings {
    #[serde(rename = "total_earnings")]
    pub total_earnings: String,
    #[serde(rename = "settled_amount")]
    pub available_earnings: String,
    #[serde(rename = "pending_amount")]
    pub pending_earnings: String,
    pub currency: String,
    #[serde(rename = "level1_referrals")]
    pub referral_count: i64,
}

/// 推荐人信息
#[derive(Debug, Clone, Deserialize)]
pub struct ReferralInfo {
    #[serde(alias = "user_id")]
    pub id: String,
    #[serde(default)]
    pub email: String,
    #[serde(default, alias = "user_name")]
    pub name: Option<String>,
    #[serde(rename = "created_at", alias = "registered_at")]
    pub joined_at: String,
    #[serde(
        rename = "total_consumption",
        default = "default_zero_string",
        deserialize_with = "deserialize_string_from_number_or_string"
    )]
    pub total_spent: String,
    #[serde(
        rename = "earnings",
        alias = "total_earnings",
        default = "default_zero_string",
        deserialize_with = "deserialize_string_from_number_or_string"
    )]
    pub earnings_from_referral: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReferralPage {
    pub referrals: Vec<ReferralInfo>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
    pub as_of: String,
}

/// 推荐码响应
#[derive(Debug, Clone, Deserialize)]
pub struct ReferralCodeResponse {
    pub referral_code: String,
    #[serde(rename = "invite_link")]
    pub referral_link: String,
}

/// 邀请链接响应
#[derive(Debug, Clone, Deserialize)]
pub struct InviteLinkResponse {
    pub invite_link: String,
    pub expires_at: Option<String>,
}

/// 分销查询参数
#[derive(Debug, Clone, Serialize, Default)]
pub struct DistributionQueryParams {
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
    pub page: Option<i32>,
    pub page_size: Option<i32>,
}

impl DistributionQueryParams {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_start_date(mut self, date: impl Into<String>) -> Self {
        self.start_date = Some(date.into());
        self
    }

    pub fn with_end_date(mut self, date: impl Into<String>) -> Self {
        self.end_date = Some(date.into());
        self
    }

    pub fn with_limit(mut self, limit: i32) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn with_offset(mut self, offset: i32) -> Self {
        self.offset = Some(offset);
        self
    }

    pub fn with_page(mut self, page: i32) -> Self {
        self.page = Some(page);
        self
    }

    pub fn with_page_size(mut self, page_size: i32) -> Self {
        self.page_size = Some(page_size);
        self
    }

    pub fn to_query_string(&self) -> String {
        let mut params = Vec::new();
        if let Some(ref start) = self.start_date {
            params.push(format!("start_date={}", encode_query_value(start)));
        }
        if let Some(ref end) = self.end_date {
            params.push(format!("end_date={}", encode_query_value(end)));
        }
        if let Some(limit) = self.limit {
            params.push(format!("limit={}", limit));
        }
        if let Some(offset) = self.offset {
            params.push(format!("offset={}", offset));
        }
        if let Some(page) = self.page {
            params.push(format!("page={}", page));
        }
        if let Some(page_size) = self.page_size {
            params.push(format!("page_size={}", page_size));
        }
        params.join("&")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DistributionRecordPage {
    pub records: Vec<DistributionRecord>,
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub page: i64,
    #[serde(default)]
    pub page_size: i64,
    #[serde(default)]
    pub total_pages: i64,
}

/// 分销记录
#[derive(Debug, Clone, Deserialize)]
pub struct DistributionRecord {
    pub id: String,
    #[serde(alias = "beneficiary_id")]
    pub referrer_id: Option<String>,
    #[serde(alias = "usage_log_id")]
    pub referred_id: String,
    #[serde(default, deserialize_with = "deserialize_string_from_number_or_string")]
    pub amount: String,
    #[serde(
        default,
        alias = "share_amount",
        deserialize_with = "deserialize_string_from_number_or_string"
    )]
    pub commission: String,
    pub status: String,
    pub created_at: String,
}

/// 分销统计
#[derive(Debug, Clone, Deserialize)]
pub struct DistributionStats {
    pub total_commission: f64,
    pub total_referrals: i64,
    pub active_referrals: i64,
    pub period: String,
}

/// 分销规则
#[derive(Debug, Clone, Deserialize)]
pub struct DistributionRule {
    pub id: String,
    pub name: String,
    pub commission_rate: f64,
    pub min_purchase_amount: Option<f64>,
    pub max_commission_amount: Option<f64>,
    pub is_active: bool,
    pub created_at: String,
}

/// 创建分销规则请求
#[derive(Debug, Clone, Serialize)]
pub struct CreateDistributionRuleRequest {
    pub name: String,
    pub commission_rate: f64,
    pub min_purchase_amount: Option<f64>,
    pub max_commission_amount: Option<f64>,
}

fn default_zero_string() -> String {
    "0".to_string()
}

fn deserialize_string_from_number_or_string<'de, D>(
    deserializer: D,
) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(default_zero_string()),
        serde_json::Value::Number(number) => Ok(number.to_string()),
        serde_json::Value::String(string) => Ok(string),
        other => Err(serde::de::Error::custom(format!(
            "expected number or string, got {other}"
        ))),
    }
}

impl CreateDistributionRuleRequest {
    pub fn new(name: impl Into<String>, commission_rate: f64) -> Self {
        Self {
            name: name.into(),
            commission_rate,
            min_purchase_amount: None,
            max_commission_amount: None,
        }
    }

    pub fn with_min_purchase_amount(mut self, amount: f64) -> Self {
        self.min_purchase_amount = Some(amount);
        self
    }

    pub fn with_max_commission_amount(mut self, amount: f64) -> Self {
        self.max_commission_amount = Some(amount);
        self
    }
}

/// 更新分销规则请求
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateDistributionRuleRequest {
    pub name: Option<String>,
    pub commission_rate: Option<f64>,
    pub min_purchase_amount: Option<f64>,
    pub max_commission_amount: Option<f64>,
    pub is_active: Option<bool>,
}

impl UpdateDistributionRuleRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_commission_rate(mut self, rate: f64) -> Self {
        self.commission_rate = Some(rate);
        self
    }

    pub fn with_is_active(mut self, is_active: bool) -> Self {
        self.is_active = Some(is_active);
        self
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DistributionOverview {
    pub earnings: DistributionEarnings,
    pub referral: ReferralCodeResponse,
    pub as_of: String,
}
