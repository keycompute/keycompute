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
const MAX_REQUIREMENT_TYPE_LEN: usize = 128;
const MAX_OPTIONAL_FIELD_LEN: usize = 256;
const MAX_DEPLOYMENT_LEN: usize = 128;
const MAX_CONTACT_VALUE_LEN: usize = 512;
const ALLOWED_CONTACT_METHODS: &[&str] = &["wechat", "email", "telegram", "phone"];

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
    validate_requirement_request(&req)?;

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

fn validate_requirement_request(req: &RequirementRequest) -> Result<()> {
    let requirement_type = req.requirement_type.trim();
    if requirement_type.is_empty() {
        return Err(ApiError::BadRequest("需求类型不能为空".to_string()));
    }
    if requirement_type.chars().any(char::is_control) {
        return Err(ApiError::BadRequest("需求类型不能包含控制字符".to_string()));
    }
    validate_max_len(requirement_type, MAX_REQUIREMENT_TYPE_LEN, "需求类型")?;

    validate_optional_max_len(req.model.as_deref(), MAX_OPTIONAL_FIELD_LEN, "模型需求")?;
    validate_optional_max_len(
        req.usage_scale.as_deref(),
        MAX_OPTIONAL_FIELD_LEN,
        "预计使用规模",
    )?;

    let deployment = req.deployment.trim();
    if deployment.is_empty() {
        return Err(ApiError::BadRequest("节点部署方案不能为空".to_string()));
    }
    validate_max_len(deployment, MAX_DEPLOYMENT_LEN, "节点部署方案")?;

    let contact_method = req.contact_method.trim();
    if !ALLOWED_CONTACT_METHODS.contains(&contact_method) {
        return Err(ApiError::BadRequest("联系方式类型无效".to_string()));
    }

    let contact_value = req.contact_value.trim();
    if contact_value.is_empty() {
        return Err(ApiError::BadRequest("联系方式不能为空".to_string()));
    }
    validate_max_len(contact_value, MAX_CONTACT_VALUE_LEN, "联系方式")?;
    validate_contact_value(contact_method, contact_value)?;

    validate_optional_max_len(req.note.as_deref(), MAX_NOTE_LEN, "补充说明")?;

    Ok(())
}

fn validate_optional_max_len(value: Option<&str>, max_len: usize, field_name: &str) -> Result<()> {
    if let Some(value) = value {
        let value = value.trim();
        if !value.is_empty() {
            validate_max_len(value, max_len, field_name)?;
        }
    }

    Ok(())
}

fn validate_max_len(value: &str, max_len: usize, field_name: &str) -> Result<()> {
    if value.chars().count() > max_len {
        return Err(ApiError::BadRequest(format!("{field_name}长度超过限制")));
    }

    Ok(())
}

fn validate_contact_value(contact_method: &str, contact_value: &str) -> Result<()> {
    let valid = match contact_method {
        "email" => contact_value.contains('@') && contact_value.contains('.'),
        "phone" => contact_value.chars().filter(|c| c.is_ascii_digit()).count() == 11,
        "wechat" | "telegram" => true,
        _ => false,
    };

    if valid {
        Ok(())
    } else {
        Err(ApiError::BadRequest("联系方式格式不正确".to_string()))
    }
}

/// 转义 HTML 特殊字符，避免用户输入破坏邮件结构
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_request() -> RequirementRequest {
        RequirementRequest {
            requirement_type: "API 调用".to_string(),
            model: Some("deepseek-chat".to_string()),
            usage_scale: Some("10w tokens/day".to_string()),
            deployment: "容器镜像部署".to_string(),
            contact_method: "email".to_string(),
            contact_value: "buyer@example.com".to_string(),
            note: Some("需要稳定吞吐".to_string()),
        }
    }

    fn assert_bad_request(result: Result<()>, expected: &str) {
        match result {
            Err(ApiError::BadRequest(msg)) => assert!(msg.contains(expected), "{msg}"),
            other => panic!("expected BadRequest containing {expected}, got {other:?}"),
        }
    }

    #[test]
    fn validates_required_fields() {
        let mut req = valid_request();
        req.requirement_type = "   ".to_string();
        assert_bad_request(validate_requirement_request(&req), "需求类型");

        let mut req = valid_request();
        req.contact_value = "   ".to_string();
        assert_bad_request(validate_requirement_request(&req), "联系方式不能为空");
    }

    #[test]
    fn rejects_control_characters_in_subject_source() {
        let mut req = valid_request();
        req.requirement_type = "API 调用\nBcc: attacker@example.com".to_string();

        assert_bad_request(validate_requirement_request(&req), "控制字符");
    }

    #[test]
    fn validates_contact_method_and_format() {
        let mut req = valid_request();
        req.contact_method = "discord".to_string();
        assert_bad_request(validate_requirement_request(&req), "联系方式类型");

        let mut req = valid_request();
        req.contact_value = "not-an-email".to_string();
        assert_bad_request(validate_requirement_request(&req), "联系方式格式");

        let mut req = valid_request();
        req.contact_method = "phone".to_string();
        req.contact_value = "12345".to_string();
        assert_bad_request(validate_requirement_request(&req), "联系方式格式");
    }

    #[test]
    fn validates_note_length() {
        let mut req = valid_request();
        req.note = Some("x".repeat(MAX_NOTE_LEN + 1));

        assert_bad_request(validate_requirement_request(&req), "补充说明");
    }

    #[test]
    fn validates_field_lengths() {
        let mut req = valid_request();
        req.requirement_type = "x".repeat(MAX_REQUIREMENT_TYPE_LEN + 1);
        assert_bad_request(validate_requirement_request(&req), "需求类型");

        let mut req = valid_request();
        req.model = Some("x".repeat(MAX_OPTIONAL_FIELD_LEN + 1));
        assert_bad_request(validate_requirement_request(&req), "模型需求");

        let mut req = valid_request();
        req.usage_scale = Some("x".repeat(MAX_OPTIONAL_FIELD_LEN + 1));
        assert_bad_request(validate_requirement_request(&req), "预计使用规模");

        let mut req = valid_request();
        req.deployment = "x".repeat(MAX_DEPLOYMENT_LEN + 1);
        assert_bad_request(validate_requirement_request(&req), "节点部署方案");

        let mut req = valid_request();
        req.contact_value = format!("{}@example.com", "x".repeat(MAX_CONTACT_VALUE_LEN));
        assert_bad_request(validate_requirement_request(&req), "联系方式");
    }

    #[test]
    fn builds_email_bodies_with_html_escaping_and_defaults() {
        let req = RequirementRequest {
            requirement_type: "API <调用> \"quoted\"".to_string(),
            model: None,
            usage_scale: None,
            deployment: "容器 & 镜像".to_string(),
            contact_method: "wechat".to_string(),
            contact_value: "wx<unsafe>&id'".to_string(),
            note: Some("预算 < 100 & 需要 \"稳定\"".to_string()),
        };

        let (text_body, html_body) = build_email_bodies(&req);

        assert!(text_body.contains("模型需求：-"));
        assert!(text_body.contains("预计使用规模：-"));
        assert!(html_body.contains("API &lt;调用&gt; &quot;quoted&quot;"));
        assert!(html_body.contains("容器 &amp; 镜像"));
        assert!(html_body.contains("wx&lt;unsafe&gt;&amp;id&#39;"));
        assert!(html_body.contains("预算 &lt; 100 &amp; 需要 &quot;稳定&quot;"));
    }

    #[tokio::test]
    async fn returns_service_unavailable_without_requirement_recipient() {
        let result =
            submit_requirement_handler(State(AppState::new()), Json(valid_request())).await;

        match result {
            Err(ApiError::ServiceUnavailable(msg)) => assert!(msg.contains("暂未开放"), "{msg}"),
            Err(other) => panic!("expected ServiceUnavailable, got {other:?}"),
            Ok(_) => panic!("expected ServiceUnavailable, got success"),
        }
    }
}
