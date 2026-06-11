//! 需求收集处理器
//!
//! 处理首页"提交算力需求"表单提交：基础校验后通过邮件发送至配置的接收人邮箱。
//! 不落库；邮件未配置或发送失败时返回错误。

use crate::{
    error::{ApiError, Result},
    state::AppState,
};
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};

/// 补充说明最大长度
const MAX_NOTE_LEN: usize = 500;

/// 需求提交请求体
#[derive(Debug, Deserialize)]
pub struct RequirementRequest {
    /// 需求类型（如 API 调用 / 私有部署 / 节点租赁 ...）
    pub requirement_type: String,
    /// 模型需求（可选）
    #[serde(default)]
    pub model: Option<String>,
    /// 预计使用规模（可选）
    #[serde(default)]
    pub usage_scale: Option<String>,
    /// 节点部署方案
    pub deployment: String,
    /// 联系方式类型（wechat / email / telegram / phone）
    pub contact_method: String,
    /// 联系方式内容（必填）
    pub contact_value: String,
    /// 补充说明（可选）
    #[serde(default)]
    pub note: Option<String>,
}

/// 需求提交响应体
#[derive(Debug, Serialize)]
pub struct RequirementResponse {
    pub message: String,
}

/// 提交算力需求
///
/// POST /api/v1/requirements
pub async fn submit_requirement_handler(
    State(state): State<AppState>,
    Json(req): Json<RequirementRequest>,
) -> Result<impl IntoResponse> {
    // 基础校验（前端为主，后端兜底）
    if req.requirement_type.trim().is_empty() {
        return Err(ApiError::BadRequest("需求类型不能为空".to_string()));
    }
    if req.contact_value.trim().is_empty() {
        return Err(ApiError::BadRequest("联系方式不能为空".to_string()));
    }
    if let Some(note) = &req.note
        && note.chars().count() > MAX_NOTE_LEN
    {
        return Err(ApiError::BadRequest("补充说明长度超过限制".to_string()));
    }

    // 接收人未配置 → 功能不可用
    let recipient = state
        .email_service
        .requirement_recipient()
        .await
        .filter(|r| !r.trim().is_empty())
        .ok_or_else(|| {
            ApiError::ServiceUnavailable("需求提交服务暂未开放，请稍后再试".to_string())
        })?;

    let subject = format!("[算力需求] {}", req.requirement_type.trim());
    let (text_body, html_body) = build_email_bodies(&req);

    state
        .email_service
        .send_html_email(&recipient, &subject, &text_body, &html_body)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "需求邮件发送失败");
            ApiError::ServiceUnavailable("提交失败，请稍后重试".to_string())
        })?;

    Ok((
        StatusCode::OK,
        Json(RequirementResponse {
            message: "已收到您的需求".to_string(),
        }),
    ))
}

/// 转义 HTML 特殊字符，避免用户输入破坏邮件结构
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 构建纯文本与 HTML 两种邮件正文
fn build_email_bodies(req: &RequirementRequest) -> (String, String) {
    let model = req.model.as_deref().map(str::trim).unwrap_or("-");
    let model = if model.is_empty() { "-" } else { model };
    let usage = req.usage_scale.as_deref().map(str::trim).unwrap_or("-");
    let usage = if usage.is_empty() { "-" } else { usage };
    let note = req.note.as_deref().map(str::trim).unwrap_or("-");
    let note = if note.is_empty() { "-" } else { note };

    let text_body = format!(
        "新的算力需求提交\n\n需求类型：{}\n模型需求：{}\n预计使用规模：{}\n节点部署方案：{}\n联系方式（{}）：{}\n补充说明：{}\n",
        req.requirement_type.trim(),
        model,
        usage,
        req.deployment.trim(),
        req.contact_method.trim(),
        req.contact_value.trim(),
        note,
    );

    let html_body = format!(
        r#"<h2>新的算力需求提交</h2>
<table cellpadding="8" style="border-collapse:collapse;border:1px solid #ddd">
<tr><td><b>需求类型</b></td><td>{}</td></tr>
<tr><td><b>模型需求</b></td><td>{}</td></tr>
<tr><td><b>预计使用规模</b></td><td>{}</td></tr>
<tr><td><b>节点部署方案</b></td><td>{}</td></tr>
<tr><td><b>联系方式（{}）</b></td><td>{}</td></tr>
<tr><td><b>补充说明</b></td><td>{}</td></tr>
</table>"#,
        esc(req.requirement_type.trim()),
        esc(model),
        esc(usage),
        esc(req.deployment.trim()),
        esc(req.contact_method.trim()),
        esc(req.contact_value.trim()),
        esc(note),
    );

    (text_body, html_body)
}
