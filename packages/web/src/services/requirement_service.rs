//! 需求收集服务
//!
//! 提交首页"提交算力需求"表单到后端公开接口 `/api/v1/requirements`。
#![allow(dead_code)]

use client_api::error::Result;
use serde::{Deserialize, Serialize};

use super::api_client::get_client;

/// 需求提交请求体（字段需与后端 RequirementRequest 对齐）
#[derive(Debug, Clone, Serialize)]
pub struct RequirementSubmission {
    pub requirement_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_scale: Option<String>,
    pub deployment: String,
    pub contact_method: String,
    pub contact_value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// 需求提交响应体
#[derive(Debug, Clone, Deserialize)]
pub struct RequirementResponse {
    pub message: String,
}

/// 提交算力需求（公开接口，无需登录）
pub async fn submit_requirement(req: &RequirementSubmission) -> Result<RequirementResponse> {
    get_client()
        .post_json("/api/v1/requirements", req, None)
        .await
}
