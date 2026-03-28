//! 账单模块
//!
//! 处理账单记录查询和统计

use crate::client::ApiClient;
use crate::error::Result;
use serde::{Deserialize, Serialize};

/// 账单 API 客户端
#[derive(Debug, Clone)]
pub struct BillingApi {
    client: ApiClient,
}

impl BillingApi {
    /// 创建新的账单 API 客户端
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    /// 获取账单记录列表
    pub async fn list_billing_records(
        &self,
        params: Option<&BillingQueryParams>,
        token: &str,
    ) -> Result<Vec<BillingRecord>> {
        let path = if let Some(p) = params {
            format!("/api/v1/billing/records?{}", p.to_query_string())
        } else {
            "/api/v1/billing/records".to_string()
        };
        self.client.get_json(&path, Some(token)).await
    }

    /// 获取账单统计
    pub async fn get_billing_stats(&self, token: &str) -> Result<BillingStats> {
        self.client
            .get_json("/api/v1/billing/stats", Some(token))
            .await
    }
}

/// 账单查询参数
#[derive(Debug, Clone, Serialize, Default)]
pub struct BillingQueryParams {
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
}

impl BillingQueryParams {
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

    pub fn to_query_string(&self) -> String {
        let mut params = Vec::new();
        if let Some(ref start) = self.start_date {
            params.push(format!("start_date={}", start));
        }
        if let Some(ref end) = self.end_date {
            params.push(format!("end_date={}", end));
        }
        if let Some(limit) = self.limit {
            params.push(format!("limit={}", limit));
        }
        if let Some(offset) = self.offset {
            params.push(format!("offset={}", offset));
        }
        params.join("&")
    }
}

/// 账单记录
#[derive(Debug, Clone, Deserialize)]
pub struct BillingRecord {
    pub id: String,
    pub user_id: String,
    pub amount: f64,
    pub currency: String,
    pub description: Option<String>,
    pub status: String,
    pub created_at: String,
    pub paid_at: Option<String>,
}

/// 账单统计
#[derive(Debug, Clone, Deserialize)]
pub struct BillingStats {
    pub total_amount: f64,
    pub total_paid: f64,
    pub total_unpaid: f64,
    pub currency: String,
    pub period: String,
}
