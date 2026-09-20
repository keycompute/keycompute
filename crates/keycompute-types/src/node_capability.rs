//! Versioned per-model native capabilities. Shared wire shape with node-token.
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
    pub fn plain_chat(model: impl Into<String>) -> Self {
        Self {
            version: NATIVE_CAPABILITY_VERSION,
            model: model.into(),
            operation: NodeNativeOperation::Chat,
            features: vec![],
            max_request_bytes: MAX_NATIVE_BODY_BYTES as u32,
            max_response_bytes: MAX_NATIVE_BODY_BYTES as u32,
            max_output_tokens: None,
        }
    }
    pub fn with_features(mut self, features: Vec<NativeFeature>) -> Self {
        self.features = features;
        self
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != NATIVE_CAPABILITY_VERSION
            || self.model.is_empty()
            || self.model.chars().count() > 100
            || self.features.len() > 4
            || self.features.iter().collect::<BTreeSet<_>>().len() != self.features.len()
            || self.max_request_bytes == 0
            || self.max_request_bytes as usize > MAX_NATIVE_BODY_BYTES
            || self.max_response_bytes == 0
            || self.max_response_bytes as usize > MAX_NATIVE_BODY_BYTES
            || self
                .max_output_tokens
                .is_some_and(|n| n == 0 || n > i32::MAX as u32)
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
            && needed.features.iter().all(|f| self.features.contains(f))
            && self.max_request_bytes >= needed.request_bytes
            && self
                .max_output_tokens
                .is_none_or(|max| needed.output_tokens.is_some_and(|n| n <= max))
    }
    pub fn validate_result(&self, result: &NodeNativeHttpResult) -> Result<(), &'static str> {
        if native_body_size(&result.body)? > self.max_response_bytes as usize {
            return Err("native_response_profile_limit");
        }
        if let Some((_, output)) = result.validate(&self.model)?
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
        request.validate(model)?;
        let mut features = BTreeSet::new();
        if request
            .body
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|v| !v.is_empty())
        {
            features.insert(NativeFeature::Tools);
        }
        if request
            .body
            .get("response_format")
            .is_some_and(|v| !v.is_null())
        {
            features.insert(NativeFeature::StructuredOutput);
        }
        if request.body.get("thinking").is_some_and(|v| !v.is_null())
            || request
                .body
                .get("reasoning_effort")
                .is_some_and(|v| !v.is_null())
        {
            features.insert(NativeFeature::Thinking);
        }
        inspect_content(&request.body["messages"], &mut features);
        let output = request
            .body
            .get("max_completion_tokens")
            .or_else(|| request.body.get("max_tokens"));
        let output_tokens = match output {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .filter(|n| *n > 0)
                    .ok_or("native_output_limit_invalid")?,
            ),
        };
        Ok(Self {
            version: NATIVE_CAPABILITY_VERSION,
            model: model.into(),
            operation: request.operation,
            features: features.into_iter().collect(),
            request_bytes: native_body_size(&request.body)? as u32,
            output_tokens,
        })
    }
    pub fn selector(&self) -> Value {
        json!({"version":self.version,"model":self.model,"operation":self.operation,"features":self.features})
    }
}
fn inspect_content(value: &Value, features: &mut BTreeSet<NativeFeature>) {
    match value {
        Value::Array(values) => {
            for value in values {
                inspect_content(value, features);
            }
        }
        Value::Object(object) => {
            if object.get("role").and_then(Value::as_str) == Some("tool")
                || object.get("tool_calls").is_some_and(|v| !v.is_null())
            {
                features.insert(NativeFeature::Tools);
            }
            if matches!(
                object.get("type").and_then(Value::as_str),
                Some("image_url" | "image" | "input_image")
            ) {
                features.insert(NativeFeature::Vision);
            }
            if let Some(content) = object.get("content") {
                inspect_content(content, features);
            }
        }
        _ => {}
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
    fn request() -> NodeNativeRequest {
        NodeNativeRequest {
            operation: NodeNativeOperation::Chat,
            headers: vec![],
            body: json!({"model":"m","messages":[{"role":"user","content":"x"}],"max_tokens":5}),
        }
    }
    #[test]
    fn features_and_limits_are_real_dispatch_requirements() {
        let mut r = request();
        r.body["tools"] = json!([{"type":"function","function":{"name":"f"}}]);
        let needed = NativeRequirements::from_request(&r).unwrap();
        let mut p = NativeModelProfile::plain_chat("m");
        assert!(!p.permits(&needed));
        p.features.push(NativeFeature::Tools);
        assert!(p.permits(&needed));
        p.max_output_tokens = Some(4);
        assert!(!p.permits(&needed));
        p.max_output_tokens = Some(5);
        assert!(p.permits(&needed));
        p.max_request_bytes = 1;
        assert!(!p.permits(&needed));
    }
    #[test]
    fn profiles_cannot_extend_registration_scope_or_duplicate_entries() {
        let p = NativeModelProfile::plain_chat("m");
        assert!(
            validate_profiles(
                std::slice::from_ref(&p),
                &["m".into()],
                &[NodeNativeOperation::Chat]
            )
            .is_ok()
        );
        assert!(
            validate_profiles(std::slice::from_ref(&p), &[], &[NodeNativeOperation::Chat]).is_err()
        );
        assert!(
            validate_profiles(&[p.clone(), p], &["m".into()], &[NodeNativeOperation::Chat])
                .is_err()
        );
    }
    #[test]
    fn stable_capability_fixture_round_trips() {
        let value: Value =
            serde_json::from_str(include_str!("../fixtures/node-native-capability-v1.json"))
                .unwrap();
        let profile: NativeModelProfile = serde_json::from_value(value.clone()).unwrap();
        profile.validate().unwrap();
        assert_eq!(serde_json::to_value(profile).unwrap(), value);
    }
}
