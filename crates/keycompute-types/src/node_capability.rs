//! Versioned per-model native capabilities. Shared wire shape with keycompute-types.
use crate::node_native::{
    MAX_NATIVE_BODY_BYTES, NodeNativeHttpResult, NodeNativeOperation, NodeNativeRequest,
    native_body_size,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const NATIVE_CAPABILITY_VERSION: u16 = 1;
pub const MAX_NATIVE_PROFILES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeFeature {
    Sse,
    Tools,
    Vision,
    StructuredOutput,
    Thinking,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeModelProfile {
    pub version: u16,
    pub model: String,
    pub operation: NodeNativeOperation,
    #[serde(default)]
    pub features: Vec<NativeFeature>,
    pub max_request_bytes: u32,
    pub max_response_bytes: u32,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRequirements {
    pub version: u16,
    pub model: String,
    pub operation: NodeNativeOperation,
    pub features: Vec<NativeFeature>,
    pub request_bytes: u32,
    pub output_tokens: Option<u32>,
}

impl NativeModelProfile {
    pub fn for_operation(model: impl Into<String>, operation: NodeNativeOperation) -> Self {
        Self {
            version: NATIVE_CAPABILITY_VERSION,
            model: model.into(),
            operation,
            features: vec![],
            max_request_bytes: MAX_NATIVE_BODY_BYTES as u32,
            max_response_bytes: MAX_NATIVE_BODY_BYTES as u32,
            max_output_tokens: None,
        }
    }

    pub fn plain_chat(model: impl Into<String>) -> Self {
        Self::for_operation(model, NodeNativeOperation::Chat)
    }

    pub fn with_features(mut self, features: Vec<NativeFeature>) -> Self {
        self.features = features;
        self
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != NATIVE_CAPABILITY_VERSION
            || self.model.is_empty()
            || self.model.chars().count() > 100
            || self.features.len() > 5
            || self.features.iter().collect::<BTreeSet<_>>().len() != self.features.len()
            || self.max_request_bytes == 0
            || self.max_request_bytes as usize > MAX_NATIVE_BODY_BYTES
            || self.max_response_bytes == 0
            || self.max_response_bytes as usize > MAX_NATIVE_BODY_BYTES
            || self
                .max_output_tokens
                .is_some_and(|tokens| tokens == 0 || tokens > i32::MAX as u32)
        {
            return Err("native_profile_invalid");
        }
        Ok(())
    }

    pub fn permits(&self, needed: &NativeRequirements) -> bool {
        self.validate().is_ok()
            && self.version == needed.version
            && self.model == needed.model
            && self.operation == needed.operation
            && needed
                .features
                .iter()
                .all(|feature| self.features.contains(feature))
            && self.max_request_bytes >= needed.request_bytes
            && self
                .max_output_tokens
                .is_none_or(|max| needed.output_tokens.is_some_and(|tokens| tokens <= max))
    }

    pub fn validate_result(&self, result: &NodeNativeHttpResult) -> Result<(), &'static str> {
        if native_body_size(&result.body)? > self.max_response_bytes as usize {
            return Err("native_response_profile_limit");
        }
        if let Some((_, output)) = result.validate_for(self.operation, &self.model)?
            && self.max_output_tokens.is_some_and(|max| output > max)
        {
            return Err("native_output_profile_limit");
        }
        Ok(())
    }
}

impl NativeRequirements {
    pub fn from_request(request: &NodeNativeRequest) -> Result<Self, &'static str> {
        let model = request
            .body
            .get("model")
            .and_then(Value::as_str)
            .ok_or("native_model_missing")?;
        request.validate_for(request.operation, model)?;

        let mut features = BTreeSet::new();
        inspect_features(&request.body, request.operation, &mut features);
        if request.body.get("stream").and_then(Value::as_bool) == Some(true) {
            features.insert(NativeFeature::Sse);
        }
        let output = match request.operation {
            NodeNativeOperation::Chat => request
                .body
                .get("max_completion_tokens")
                .or_else(|| request.body.get("max_tokens")),
            NodeNativeOperation::Messages => request.body.get("max_tokens"),
            NodeNativeOperation::Responses => request.body.get("max_output_tokens"),
        };
        let output_tokens = match output {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(|tokens| u32::try_from(tokens).ok())
                    .filter(|tokens| *tokens > 0 && *tokens <= i32::MAX as u32)
                    .ok_or("native_output_limit_invalid")?,
            ),
        };
        Ok(Self {
            version: NATIVE_CAPABILITY_VERSION,
            model: model.to_owned(),
            operation: request.operation,
            features: features.into_iter().collect(),
            request_bytes: native_body_size(&request.body)? as u32,
            output_tokens,
        })
    }

    pub fn selector(&self) -> Value {
        json!({
            "version": self.version,
            "model": self.model,
            "operation": self.operation,
            "features": self.features
        })
    }
}

fn enabled_thinking(value: &Value) -> bool {
    match value {
        Value::Bool(false) | Value::Null => false,
        Value::String(value) => !matches!(value.as_str(), "disabled" | "none" | "off"),
        Value::Object(object) => object
            .get("type")
            .and_then(Value::as_str)
            .is_none_or(|kind| kind != "disabled"),
        _ => true,
    }
}

fn structured_output(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Object(object) => object
            .get("type")
            .and_then(Value::as_str)
            .is_none_or(|kind| kind != "text"),
        _ => true,
    }
}

fn inspect_content(value: &Value, features: &mut BTreeSet<NativeFeature>) {
    let Some(items) = value.as_array() else {
        return;
    };
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("tool_use" | "tool_result" | "function_call" | "function_call_output") => {
                features.insert(NativeFeature::Tools);
            }
            Some("image" | "image_url" | "input_image") => {
                features.insert(NativeFeature::Vision);
            }
            Some("thinking" | "redacted_thinking" | "reasoning") => {
                features.insert(NativeFeature::Thinking);
            }
            _ => {}
        }
        if item.get("role").and_then(Value::as_str) == Some("tool")
            || item
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_some_and(|v| !v.is_empty())
        {
            features.insert(NativeFeature::Tools);
        }
        if matches!(
            item.get("type").and_then(Value::as_str),
            None | Some("message" | "tool_result")
        ) && let Some(content) = item.get("content")
        {
            inspect_content(content, features);
        }
    }
}
fn inspect_features(
    body: &Value,
    operation: NodeNativeOperation,
    features: &mut BTreeSet<NativeFeature>,
) {
    if body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
    {
        features.insert(NativeFeature::Tools);
    }
    if let Some(messages) = body.get("messages") {
        inspect_content(messages, features);
    }
    if let Some(input) = body.get("input") {
        inspect_content(input, features);
    }
    let format = if operation == NodeNativeOperation::Responses {
        body.pointer("/text/format")
    } else {
        body.get("response_format")
    };
    if format.is_some_and(structured_output) {
        features.insert(NativeFeature::StructuredOutput);
    }
    if body.get("thinking").is_some_and(enabled_thinking)
        || body
            .get("reasoning_effort")
            .or_else(|| body.pointer("/reasoning/effort"))
            .and_then(Value::as_str)
            .is_some_and(|v| !matches!(v, "none" | "disabled" | "off"))
    {
        features.insert(NativeFeature::Thinking);
    }
}

pub fn validate_profiles(
    profiles: &[NativeModelProfile],
    models: &[String],
    operations: &[NodeNativeOperation],
) -> Result<(), &'static str> {
    if profiles.len() > MAX_NATIVE_PROFILES {
        return Err("native_profile_count_exceeded");
    }
    let mut keys = BTreeSet::new();
    for profile in profiles {
        profile.validate()?;
        if !models.contains(&profile.model)
            || !operations.contains(&profile.operation)
            || !keys.insert((profile.model.as_str(), profile.operation.as_str()))
        {
            return Err("native_profile_scope_invalid");
        }
    }
    Ok(())
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
    fn features_are_operation_aware_and_disabled_options_are_free() {
        let r = request(
            NodeNativeOperation::Responses,
            json!({"model":"m","input":[{"type":"function_call_output"}],"thinking":{"type":"disabled"},"text":{"format":{"type":"text"}},"max_output_tokens":5}),
        );
        let needed = NativeRequirements::from_request(&r).unwrap();
        assert_eq!(needed.operation, NodeNativeOperation::Responses);
        assert_eq!(needed.features, vec![NativeFeature::Tools]);
        assert_eq!(needed.output_tokens, Some(5));
    }

    #[test]
    fn profiles_have_an_explicit_operation_constructor() {
        let profile = NativeModelProfile::for_operation("m", NodeNativeOperation::Messages);
        assert_eq!(profile.operation, NodeNativeOperation::Messages);
        assert_eq!(
            NativeModelProfile::plain_chat("m").operation,
            NodeNativeOperation::Chat
        );
    }

    #[test]
    fn stable_capability_fixtures_round_trip() {
        for fixture in [
            include_str!("../fixtures/node-native-capability-v1.json"),
            include_str!("../fixtures/node-native-capability-messages-v1.json"),
            include_str!("../fixtures/node-native-capability-responses-v1.json"),
        ] {
            let value: Value = serde_json::from_str(fixture).unwrap();
            let profile: NativeModelProfile = serde_json::from_value(value.clone()).unwrap();
            profile.validate().unwrap();
            assert_eq!(serde_json::to_value(profile).unwrap(), value);
        }
    }
}
