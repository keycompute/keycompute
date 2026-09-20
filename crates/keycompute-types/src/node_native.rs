//! Versioned native node wire contract. Keep synchronized with keycompute-types.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const NODE_NATIVE_VERSION: &str = "node.native.v1";
pub const MAX_NATIVE_BODY_BYTES: usize = 1024 * 1024;
pub const MAX_NATIVE_HEADERS: usize = 16;
pub const MAX_NATIVE_HEADER_VALUE_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeNativeOperation {
    Chat,
    Messages,
    Responses,
}

impl NodeNativeOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Messages => "messages",
            Self::Responses => "responses",
        }
    }

    pub fn local_path(self) -> &'static str {
        match self {
            Self::Chat => "/v1/chat/completions",
            Self::Messages => "/v1/messages",
            Self::Responses => "/v1/responses",
        }
    }

    pub fn protocol(self) -> &'static str {
        match self {
            Self::Chat | Self::Responses => "openai",
            Self::Messages => "anthropic",
        }
    }

    pub fn api_capability(self) -> &'static str {
        match self {
            Self::Chat => "chat_completions",
            Self::Messages => "messages",
            Self::Responses => "responses",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeNativeRequest {
    pub operation: NodeNativeOperation,
    pub body: Value,
    pub headers: Vec<(String, String)>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeNativeHttpResult {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

macro_rules! redacted_debug {
    ($ty:ty) => {
        impl std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($ty), " { <redacted> }"))
            }
        }
    };
}
redacted_debug!(NodeNativeRequest);
redacted_debug!(NodeNativeHttpResult);

/// Counts serialized bytes without allocating a second body buffer.
pub fn validate_native_body(body: &Value) -> Result<(), &'static str> {
    native_body_size(body).map(|_| ())
}

pub fn native_body_size(body: &Value) -> Result<usize, &'static str> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > MAX_NATIVE_BODY_BYTES {
                return Err(std::io::Error::other("native body limit"));
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, body).map_err(|_| "native_body_too_large")?;
    Ok(counter.0)
}

pub fn native_response_header_allowed(name: &str, value: &str) -> bool {
    matches!(
        name,
        "content-type"
            | "retry-after"
            | "x-request-id"
            | "request-id"
            | "x-ratelimit-limit-requests"
            | "x-ratelimit-remaining-requests"
            | "x-ratelimit-reset-requests"
            | "x-ratelimit-limit-tokens"
            | "x-ratelimit-remaining-tokens"
            | "x-ratelimit-reset-tokens"
    ) && value.len() <= MAX_NATIVE_HEADER_VALUE_BYTES
        && value.bytes().all(|b| (32..127).contains(&b))
        && (name != "content-type" || value == "application/json")
}

fn native_request_headers_valid(
    operation: NodeNativeOperation,
    headers: &[(String, String)],
) -> Result<(), &'static str> {
    if headers.len() > MAX_NATIVE_HEADERS {
        return Err("native_request_headers_unsupported");
    }
    match operation {
        NodeNativeOperation::Messages => {
            if headers.len() > 1
                || headers
                    .iter()
                    .any(|(name, value)| name != "anthropic-version" || value != "2023-06-01")
            {
                return Err("native_request_header_unsupported");
            }
        }
        NodeNativeOperation::Chat | NodeNativeOperation::Responses => {
            if !headers.is_empty() {
                return Err("native_request_headers_unsupported");
            }
        }
    }
    Ok(())
}

/// Inspect protocol content blocks, not arbitrary JSON in tool arguments,
/// schemas or vendor extensions. Such data may legitimately contain any key.
fn validate_content(value: &Value, operation: NodeNativeOperation) -> Result<(), &'static str> {
    let Some(parts) = value.as_array() else {
        return Ok(());
    };
    for part in parts {
        if operation == NodeNativeOperation::Messages && part.get("cache_control").is_some() {
            return Err("native_prompt_cache_unsupported");
        }
        match part.get("type").and_then(Value::as_str) {
            Some("document" | "pdf" | "file" | "input_file") => {
                return Err("native_document_unsupported");
            }
            Some("image_url" | "input_image") => {
                let image = part.get("image_url").unwrap_or(&Value::Null);
                let url = image
                    .as_str()
                    .or_else(|| image.get("url").and_then(Value::as_str));
                if !url.is_some_and(|v| v.starts_with("data:image/")) {
                    return Err("native_remote_image_unsupported");
                }
            }
            Some("image") => {
                let source = part.get("source").ok_or("native_image_source_missing")?;
                if source.get("type").and_then(Value::as_str) != Some("base64")
                    || source.get("data").and_then(Value::as_str).is_none()
                    || !source
                        .get("media_type")
                        .and_then(Value::as_str)
                        .is_some_and(|v| v.starts_with("image/"))
                {
                    return Err("native_remote_image_unsupported");
                }
            }
            Some("tool_result") => {
                if let Some(content) = part.get("content") {
                    validate_content(content, operation)?;
                }
            }
            Some("message") | None => {
                if let Some(content) = part.get("content") {
                    validate_content(content, operation)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_runtime_features(
    body: &Value,
    operation: NodeNativeOperation,
) -> Result<(), &'static str> {
    if operation == NodeNativeOperation::Messages {
        if body.get("cache_control").is_some()
            || body
                .get("tools")
                .and_then(Value::as_array)
                .is_some_and(|tools| tools.iter().any(|t| t.get("cache_control").is_some()))
        {
            return Err("native_prompt_cache_unsupported");
        }
        if body
            .pointer("/metadata/user_id")
            .is_some_and(|v| !v.is_null())
        {
            return Err("native_metadata_user_id_unsupported");
        }
        if body
            .pointer("/thinking/budget_tokens")
            .is_some_and(|v| !v.is_null())
        {
            return Err("native_thinking_budget_unsupported");
        }
        if let Some(choice) = body.get("tool_choice").filter(|v| !v.is_null())
            && choice.get("type").and_then(Value::as_str) != Some("auto")
            && choice.as_str() != Some("auto")
        {
            return Err("native_forced_tool_choice_unsupported");
        }
        if let Some(system) = body.get("system") {
            validate_content(system, operation)?;
        }
    } else if let Some(choice) = body.get("tool_choice").filter(|v| !v.is_null())
        && !matches!(choice.as_str(), Some("auto" | "none"))
    {
        return Err("native_forced_tool_choice_unsupported");
    }
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for message in messages {
            if let Some(content) = message.get("content") {
                validate_content(content, operation)?;
            }
        }
    }
    if operation == NodeNativeOperation::Responses {
        if let Some(input) = body.get("input") {
            validate_content(input, operation)?;
        }
        if body
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| {
                tools
                    .iter()
                    .any(|tool| tool.get("type").and_then(Value::as_str) != Some("function"))
            })
        {
            return Err("native_hosted_tools_unsupported");
        }
    }
    Ok(())
}

fn validate_stream(body: &Value) -> Result<(), &'static str> {
    match body.get("stream") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(()),
        _ => Err("native_streaming_unsupported_until_phase4"),
    }
}

fn validate_request_structure(
    operation: NodeNativeOperation,
    body: &Value,
) -> Result<(), &'static str> {
    if !body.is_object() {
        return Err("native_invalid_request_json");
    }
    validate_stream(body)?;
    match operation {
        NodeNativeOperation::Chat | NodeNativeOperation::Messages => {
            if body
                .get("messages")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
            {
                return Err("native_messages_required");
            }
            if operation == NodeNativeOperation::Messages {
                let max_tokens = body
                    .get("max_tokens")
                    .and_then(Value::as_u64)
                    .filter(|value| *value > 0)
                    .ok_or("native_max_tokens_required")?;
                if max_tokens > i32::MAX as u64 {
                    return Err("native_max_tokens_invalid");
                }
            }
        }
        NodeNativeOperation::Responses => {
            if body.get("input").is_none_or(Value::is_null) {
                return Err("native_input_required");
            }
            if body
                .get("previous_response_id")
                .is_some_and(|v| !v.is_null())
                || body.get("conversation").is_some_and(|v| !v.is_null())
                || body.get("background").and_then(Value::as_bool) == Some(true)
                || body.get("store").and_then(Value::as_bool) == Some(true)
            {
                return Err("native_stateful_responses_unsupported");
            }
            if body
                .get("max_output_tokens")
                .filter(|v| !v.is_null())
                .is_some_and(|value| {
                    value
                        .as_u64()
                        .is_none_or(|tokens| tokens == 0 || tokens > i32::MAX as u64)
                })
            {
                return Err("native_max_output_tokens_invalid");
            }
        }
    }
    validate_runtime_features(body, operation)?;
    Ok(())
}

impl NodeNativeRequest {
    pub fn validate(&self, model: &str) -> Result<(), &'static str> {
        self.validate_for(self.operation, model)
    }

    pub fn validate_for(
        &self,
        operation: NodeNativeOperation,
        model: &str,
    ) -> Result<(), &'static str> {
        validate_native_body(&self.body)?;
        if self.operation != operation {
            return Err("native_operation_mismatch");
        }
        native_request_headers_valid(operation, &self.headers)?;
        if self.body.get("model").and_then(Value::as_str) != Some(model)
            || model.is_empty()
            || model.chars().count() > 100
        {
            return Err("native_model_mismatch");
        }
        validate_request_structure(operation, &self.body)
    }
}

fn bounded_usage(usage: &Value, name: &str) -> Result<u32, &'static str> {
    usage
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value <= i32::MAX as u32)
        .ok_or("native_invalid_usage")
}

impl NodeNativeHttpResult {
    /// Chat compatibility wrapper; other operations must use `validate_for`.
    pub fn validate(&self, model: &str) -> Result<Option<(u32, u32)>, &'static str> {
        self.validate_for(NodeNativeOperation::Chat, model)
    }

    /// Validate without projecting or rewriting the provider's JSON body.
    pub fn validate_for(
        &self,
        operation: NodeNativeOperation,
        model: &str,
    ) -> Result<Option<(u32, u32)>, &'static str> {
        validate_native_body(&self.body)?;
        if self.headers.len() > MAX_NATIVE_HEADERS
            || self
                .headers
                .iter()
                .any(|(name, value)| !native_response_header_allowed(name, value))
        {
            return Err("native_invalid_headers");
        }
        if !self.body.is_object() {
            return Err("native_invalid_json_result");
        }
        if (400..=599).contains(&self.status) {
            if self.body.get("error").is_none_or(Value::is_null) {
                return Err("native_error_required");
            }
            return Ok(None);
        }
        if self.status != 200 || self.body.get("error").is_some_and(|value| !value.is_null()) {
            return Err("native_invalid_success");
        }
        if self.body.get("model").and_then(Value::as_str) != Some(model) {
            return Err("native_invalid_success");
        }
        let usage = self.body.get("usage").ok_or("native_usage_required")?;
        let (input, output, total) = match operation {
            NodeNativeOperation::Chat => {
                if self
                    .body
                    .get("choices")
                    .and_then(Value::as_array)
                    .is_none_or(Vec::is_empty)
                {
                    return Err("native_invalid_success");
                }
                (
                    bounded_usage(usage, "prompt_tokens")?,
                    bounded_usage(usage, "completion_tokens")?,
                    bounded_usage(usage, "total_tokens")?,
                )
            }
            NodeNativeOperation::Messages => {
                if self
                    .body
                    .get("content")
                    .and_then(Value::as_array)
                    .is_none_or(Vec::is_empty)
                {
                    return Err("native_invalid_success");
                }
                (
                    bounded_usage(usage, "input_tokens")?,
                    bounded_usage(usage, "output_tokens")?,
                    0,
                )
            }
            NodeNativeOperation::Responses => {
                if self
                    .body
                    .get("output")
                    .and_then(Value::as_array)
                    .is_none_or(Vec::is_empty)
                {
                    return Err("native_invalid_success");
                }
                (
                    bounded_usage(usage, "input_tokens")?,
                    bounded_usage(usage, "output_tokens")?,
                    bounded_usage(usage, "total_tokens")?,
                )
            }
        };
        if operation != NodeNativeOperation::Messages && input.checked_add(output) != Some(total) {
            return Err("native_invalid_usage");
        }
        Ok(Some((input, output)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(operation: NodeNativeOperation, body: Value) -> NodeNativeRequest {
        NodeNativeRequest {
            operation,
            body,
            headers: if operation == NodeNativeOperation::Messages {
                vec![("anthropic-version".into(), "2023-06-01".into())]
            } else {
                vec![]
            },
        }
    }

    #[test]
    fn operation_names_paths_and_protocols_are_stable() {
        assert_eq!(NodeNativeOperation::Chat.as_str(), "chat");
        assert_eq!(NodeNativeOperation::Messages.local_path(), "/v1/messages");
        assert_eq!(NodeNativeOperation::Responses.protocol(), "openai");
        assert_eq!(NodeNativeOperation::Messages.api_capability(), "messages");
    }

    #[test]
    fn golden_fixtures_round_trip_without_projection() {
        for fixture in [
            include_str!("../fixtures/node-native-chat-v1.json"),
            include_str!("../fixtures/node-native-messages-v1.json"),
            include_str!("../fixtures/node-native-responses-v1.json"),
        ] {
            let golden: Value = serde_json::from_str(fixture).unwrap();
            let request: NodeNativeRequest = serde_json::from_value(golden.clone()).unwrap();
            let model = request.body["model"].as_str().unwrap();
            request.validate_for(request.operation, model).unwrap();
            assert_eq!(serde_json::to_value(request).unwrap(), golden);
        }
    }

    #[test]
    fn messages_headers_and_unsupported_features_are_explicit() {
        let mut request = request(
            NodeNativeOperation::Messages,
            json!({"model":"m","messages":[{"role":"user","content":"x"}],"max_tokens":1}),
        );
        assert!(
            request
                .validate_for(NodeNativeOperation::Messages, "m")
                .is_ok()
        );
        request.headers[0].1 = "2024-01-01".into();
        assert!(
            request
                .validate_for(NodeNativeOperation::Messages, "m")
                .is_err()
        );
        request.headers[0].1 = "2023-06-01".into();
        request.body["cache_control"] = json!({"type":"ephemeral"});
        assert!(
            request
                .validate_for(NodeNativeOperation::Messages, "m")
                .is_err()
        );
    }

    #[test]
    fn responses_reject_state_and_accept_unknown_fields() {
        let mut request = request(
            NodeNativeOperation::Responses,
            json!({"model":"m","input":"x","unknown":{"kept":true}}),
        );
        assert!(
            request
                .validate_for(NodeNativeOperation::Responses, "m")
                .is_ok()
        );
        request.body["store"] = json!(true);
        assert!(
            request
                .validate_for(NodeNativeOperation::Responses, "m")
                .is_err()
        );
    }

    #[test]
    fn result_usage_is_operation_specific_and_bounded() {
        let messages = NodeNativeHttpResult {
            status: 200,
            headers: vec![],
            body: json!({"model":"m","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":2,"output_tokens":3},"extra":true}),
        };
        assert_eq!(
            messages.validate_for(NodeNativeOperation::Messages, "m"),
            Ok(Some((2, 3)))
        );
        let mut responses = NodeNativeHttpResult {
            status: 200,
            headers: vec![],
            body: json!({"model":"m","output":[{"type":"message"}],"usage":{"input_tokens":2,"output_tokens":3,"total_tokens":5}}),
        };
        assert_eq!(
            responses.validate_for(NodeNativeOperation::Responses, "m"),
            Ok(Some((2, 3)))
        );
        responses.body["usage"]["total_tokens"] = json!(-1);
        assert!(
            responses
                .validate_for(NodeNativeOperation::Responses, "m")
                .is_err()
        );
    }
}

#[cfg(test)]
mod chat_regression_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn golden_request_round_trip() {
        let golden: Value =
            serde_json::from_str(include_str!("../fixtures/node-native-chat-v1.json")).unwrap();
        let request: NodeNativeRequest = serde_json::from_value(golden.clone()).unwrap();
        request.validate("node:literal").unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), golden);
    }
    #[test]
    fn preserves_rich_request_and_redacts_debug() {
        let body = json!({"model":"node:literal","messages":[{"role":"assistant","content":null,"tool_calls":[{"unknown":{"nested":null}}]}],"stop":["end"],"temperature":0.2,"top_p":0.9,"n":2,"max_tokens":42,"tools":[],"unknown":{"nested":[null,1]}});
        let request = NodeNativeRequest {
            operation: NodeNativeOperation::Chat,
            body: body.clone(),
            headers: vec![],
        };
        request.validate("node:literal").unwrap();
        let decoded: NodeNativeRequest =
            serde_json::from_value(serde_json::to_value(&request).unwrap()).unwrap();
        assert_eq!(decoded.body, body);
        assert!(!format!("{request:?}").contains("literal"));
    }
    #[test]
    fn rejects_limits_headers_stream_and_invalid_usage() {
        assert!(validate_native_body(&json!("a".repeat(MAX_NATIVE_BODY_BYTES))).is_err());
        for name in [
            "authorization",
            "cookie",
            "set-cookie",
            "x-ratelimit-secret",
            "location",
        ] {
            assert!(!native_response_header_allowed(name, "secret"));
        }
        let request = NodeNativeRequest {
            operation: NodeNativeOperation::Chat,
            body: json!({"model":"m","messages":[{}],"stream":true}),
            headers: vec![],
        };
        assert!(request.validate("m").is_err());
        let mut result = NodeNativeHttpResult {
            status: 200,
            headers: vec![],
            body: json!({"model":"m","choices":[{"message":{"content":null,"tool_calls":[],"reasoning":"r"},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}),
        };
        assert_eq!(result.validate("m"), Ok(Some((2, 3))));
        result.body["usage"]["total_tokens"] = json!(6);
        assert!(result.validate("m").is_err());
        result.status = 429;
        result.body = json!({"error":{"message":"original","extra":null}});
        assert_eq!(result.validate("m"), Ok(None));
    }
}
