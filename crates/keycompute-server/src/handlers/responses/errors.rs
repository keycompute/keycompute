//! Public Responses stream-error sanitization.

use super::*;

pub(super) fn responses_error_event(message: &str) -> Event {
    Event::default().event("error").data(
        json!({
            "type": "error",
            "code": "server_error",
            "message": message,
            "param": null,
            "sequence_number": 0,
        })
        .to_string(),
    )
}

/// Preserve the stable fields SDKs use for classification while applying the
/// same free-form-message policy as non-streaming Responses HTTP errors.
pub(super) fn sanitize_upstream_responses_error(event: Option<&str>, body: &mut Value) -> bool {
    let body_type = body.get("type").and_then(Value::as_str);
    if event == Some("error") || body_type == Some("error") {
        let code = sanitize_openai_responses_error_code(
            body.get("code"),
            Value::String("server_error".to_string()),
        );
        let param = sanitize_openai_responses_error_param(body.get("param"));
        let sequence_number = body
            .get("sequence_number")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        *body = json!({
            "type": "error",
            "code": code,
            "message": "Upstream request failed",
            "param": param,
            "sequence_number": sequence_number,
        });
        return true;
    }

    if event == Some("response.failed") || body_type == Some("response.failed") {
        if let Some(error) = body.pointer_mut("/response/error") {
            sanitize_upstream_response_error_object(error);
            return true;
        }
        return false;
    }

    if body.get("object").and_then(Value::as_str) == Some("response")
        && body.get("status").and_then(Value::as_str) == Some("failed")
        && let Some(error) = body.get_mut("error")
    {
        sanitize_upstream_response_error_object(error);
        return true;
    }
    false
}

pub(super) fn sanitize_upstream_response_error_object(error: &mut Value) {
    let code = sanitize_openai_responses_error_code(
        error.get("code"),
        Value::String("server_error".to_string()),
    );
    let error_type = error
        .get("type")
        .and_then(Value::as_str)
        .map(|_| sanitize_openai_responses_error_type(error.get("type"), "server_error"));
    let param = error
        .get("param")
        .map(|_| sanitize_openai_responses_error_param(error.get("param")));
    let mut sanitized = serde_json::Map::new();
    sanitized.insert("code".to_string(), code);
    sanitized.insert(
        "message".to_string(),
        Value::String("Upstream request failed".to_string()),
    );
    if let Some(error_type) = error_type {
        sanitized.insert("type".to_string(), error_type);
    }
    if let Some(param) = param {
        sanitized.insert("param".to_string(), param);
    }
    *error = Value::Object(sanitized);
}
