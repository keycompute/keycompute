//! 用量统计模块
//!
//! 处理用户用量记录查询和统计

use crate::client::ApiClient;
use crate::error::Result;
use serde::{Deserialize, Serialize};

/// 用量 API 客户端
#[derive(Debug, Clone)]
pub struct UsageApi {
    client: ApiClient,
}

impl UsageApi {
    /// 创建新的用量 API 客户端
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    /// 获取用量记录列表
    pub async fn get_my_usage(
        &self,
        params: Option<&UsageQueryParams>,
        token: &str,
    ) -> Result<Vec<UsageRecord>> {
        let path = if let Some(p) = params {
            format!("/api/v1/usage?{}", p.to_query_string())
        } else {
            "/api/v1/usage".to_string()
        };
        self.client.get_json(&path, Some(token)).await
    }

    /// 获取用量统计
    pub async fn get_usage_stats(&self, token: &str) -> Result<UsageStats> {
        self.client
            .get_json("/api/v1/usage/stats", Some(token))
            .await
    }
}

/// 用量查询参数
#[derive(Debug, Clone, Serialize, Default)]
pub struct UsageQueryParams {
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
}

impl UsageQueryParams {
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

/// 用量记录
#[derive(Debug, Clone, Deserialize)]
pub struct UsageRecord {
    pub id: String,
    pub user_id: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cost: Option<f64>,
    pub created_at: String,
}

/// 用量统计
#[derive(Debug, Clone, Deserialize)]
pub struct UsageStats {
    pub total_requests: i64,
    pub total_tokens: i64,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub total_cost: f64,
    pub period: String,
}
