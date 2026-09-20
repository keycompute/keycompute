//! Versioned native node wire contract. Keep synchronized with node-token.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const NODE_NATIVE_VERSION: &str = "node.native.v1";
pub const MAX_NATIVE_BODY_BYTES: usize = 1024 * 1024;
pub const MAX_NATIVE_HEADERS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeNativeOperation {
    Chat,
}

impl NodeNativeOperation {
    pub fn local_path(self) -> &'static str {
        match self {
            Self::Chat => "/v1/chat/completions",
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
    serde_json::to_writer(Counter(0), body).map_err(|_| "native_body_too_large")
}

pub fn native_response_header_allowed(name: &str, value: &str) -> bool {
    // Exact names only: an arbitrary x-ratelimit-* header can contain secrets.
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
    ) && value.len() <= 256
        && value.bytes().all(|b| (32..127).contains(&b))
        && (name != "content-type" || value == "application/json")
}

impl NodeNativeRequest {
    pub fn validate(&self, model: &str) -> Result<(), &'static str> {
        validate_native_body(&self.body)?;
        if !self.headers.is_empty() {
            return Err("native_request_headers_unsupported");
        }
        if self.body.get("model").and_then(Value::as_str) != Some(model)
            || model.is_empty()
            || model.chars().count() > 100
        {
            return Err("native_model_mismatch");
        }
        match self.body.get("stream") {
            None | Some(Value::Null) | Some(Value::Bool(false)) => {}
            _ => return Err("native_streaming_unsupported_until_phase4"),
        }
        if self
            .body
            .get("messages")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
        {
            return Err("native_messages_required");
        }
        // Ollama must not fetch caller-selected remote image URLs. Inline
        // images pass unchanged; bounded remote downloads need a later policy.
        for message in self.body["messages"].as_array().expect("validated array") {
            if let Some(parts) = message.get("content").and_then(Value::as_array) {
                for part in parts {
                    if part.get("type").and_then(Value::as_str) == Some("image_url") {
                        let url = part
                            .pointer("/image_url/url")
                            .and_then(Value::as_str)
                            .or_else(|| part.get("image_url").and_then(Value::as_str));
                        if url.is_none_or(|url| !url.starts_with("data:image/")) {
                            return Err("native_remote_images_unsupported");
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl NodeNativeHttpResult {
    /// Validate separately from delivery. Never rewrite provider usage or trust fees.
    pub fn validate(&self, model: &str) -> Result<Option<(u32, u32)>, &'static str> {
        validate_native_body(&self.body)?;
        if self.headers.len() > MAX_NATIVE_HEADERS
            || self
                .headers
                .iter()
                .any(|(n, v)| !native_response_header_allowed(n, v))
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
        if self.status != 200
            || self.body.get("error").is_some_and(|value| !value.is_null())
            || self.body.get("model").and_then(Value::as_str) != Some(model)
            || self
                .body
                .get("choices")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
        {
            return Err("native_invalid_success");
        }
        let usage = self.body.get("usage").ok_or("native_usage_required")?;
        let count = |name| {
            usage
                .get(name)
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok().filter(|n| *n <= i32::MAX as u32))
                .ok_or("native_invalid_usage")
        };
        let input = count("prompt_tokens")?;
        let output = count("completion_tokens")?;
        if input.checked_add(output) != Some(count("total_tokens")?) {
            return Err("native_invalid_usage");
        }
        Ok(Some((input, output)))
    }
}

#[cfg(test)]
mod tests {
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
