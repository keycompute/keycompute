//! Forward-compatible request projection and validation.

use super::*;

pub(super) struct ResponsesRoutingFields {
    pub(super) model: String,
    pub(super) stream: bool,
    pub(super) max_output_tokens: Option<u32>,
    pub(super) temperature: Option<f32>,
    pub(super) top_p: Option<f32>,
    pub(super) messages: Vec<Message>,
}

impl ResponsesRoutingFields {
    pub(super) fn parse(body: &Value) -> Result<Self> {
        Self::parse_fields(body)
    }

    pub(super) fn parse_input_tokens(body: &Value) -> Result<Self> {
        Self::parse_fields(body)
    }

    fn parse_fields(body: &Value) -> Result<Self> {
        let object = validate_responses_reference_fields(body)?;
        let model = match object.get("model") {
            Some(Value::String(model)) if !model.trim().is_empty() => model.clone(),
            None | Some(Value::Null) => String::new(),
            _ => {
                return Err(ApiError::BadRequest(
                    "model must be a non-empty string".to_string(),
                ));
            }
        };
        let stream = match object.get("stream") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(stream)) => *stream,
            Some(_) => {
                return Err(ApiError::BadRequest(
                    "stream must be a boolean or null".to_string(),
                ));
            }
        };
        let max_output_tokens = optional_u32(object.get("max_output_tokens"), "max_output_tokens")?;
        if max_output_tokens.is_some_and(|tokens| tokens < OPENAI_RESPONSES_MIN_MAX_OUTPUT_TOKENS) {
            return Err(ApiError::BadRequest(format!(
                "max_output_tokens must be at least {OPENAI_RESPONSES_MIN_MAX_OUTPUT_TOKENS}"
            )));
        }
        let temperature = optional_f32(object.get("temperature"), "temperature")?;
        if let Some(value) = temperature
            && !(0.0..=2.0).contains(&value)
        {
            return Err(ApiError::BadRequest(
                "temperature must be between 0.0 and 2.0".to_string(),
            ));
        }
        let top_p = optional_f32(object.get("top_p"), "top_p")?;
        if let Some(value) = top_p
            && !(0.0..=1.0).contains(&value)
        {
            return Err(ApiError::BadRequest(
                "top_p must be between 0.0 and 1.0".to_string(),
            ));
        }

        Ok(Self {
            model,
            stream,
            max_output_tokens,
            temperature,
            top_p,
            messages: context_messages(body),
        })
    }
}

pub(super) fn validate_responses_reference_fields(
    body: &Value,
) -> Result<&serde_json::Map<String, Value>> {
    let object = body.as_object().ok_or_else(|| {
        ApiError::BadRequest("Responses request body must be a JSON object".to_string())
    })?;
    if object
        .get("previous_response_id")
        .is_some_and(|value| !value.is_null())
        && object
            .get("conversation")
            .is_some_and(|value| !value.is_null())
    {
        return Err(ApiError::BadRequest(
            "previous_response_id and conversation cannot be used together".to_string(),
        ));
    }
    Ok(object)
}

pub(super) fn validate_effective_responses_model(upstream_path: &str, model: &str) -> Result<()> {
    if upstream_path == "/responses/compact" && model.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "model is required for /v1/responses/compact when it cannot be resolved from previous_response_id"
                .to_string(),
        ));
    }
    Ok(())
}

/// Validate the known top-level Responses create fields that KeyCompute may
/// acknowledge without contacting an upstream. Unknown fields remain accepted
/// for forward compatibility and retain their complete JSON values.
///
/// WebSocket `generate:false` warmups do not execute the HTTP pipeline, so an
/// upstream cannot reject malformed known fields on KeyCompute's behalf.
pub(in crate::handlers) fn validate_responses_request(body: &Value) -> Result<()> {
    ResponsesRoutingFields::parse(body)?;
    let object = body
        .as_object()
        .expect("ResponsesRoutingFields validated the request as an object");

    for field in ["background", "parallel_tool_calls", "store", "stream"] {
        validate_optional_responses_field(object, field, "a boolean", Value::is_boolean)?;
    }
    for field in [
        "moderation",
        "prompt",
        "prompt_cache_options",
        "reasoning",
        "stream_options",
        "text",
    ] {
        validate_optional_responses_field(object, field, "an object", Value::is_object)?;
    }
    for field in [
        "instructions",
        "previous_response_id",
        "prompt_cache_key",
        "prompt_cache_retention",
        "service_tier",
        "truncation",
        "user",
    ] {
        validate_optional_responses_field(object, field, "a string", Value::is_string)?;
    }
    for field in ["context_management", "tools"] {
        validate_optional_responses_field(object, field, "an array of objects", |value| {
            value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_object))
        })?;
    }
    validate_optional_responses_field(object, "conversation", "a string or object", |value| {
        value.is_string()
            || value
                .as_object()
                .and_then(|conversation| conversation.get("id"))
                .is_some_and(Value::is_string)
    })?;
    validate_optional_responses_field(object, "include", "an array of strings", |value| {
        value
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
    })?;
    validate_optional_responses_field(object, "input", "a string or array of objects", |value| {
        value.is_string()
            || value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_object))
    })?;
    validate_optional_responses_field(object, "max_tool_calls", "an unsigned integer", |value| {
        value.as_u64().is_some()
    })?;
    validate_optional_responses_field(object, "metadata", "an object of string values", |value| {
        value.as_object().is_some_and(|metadata| {
            metadata.len() <= 16
                && metadata.iter().all(|(key, value)| {
                    key.chars().count() <= 64
                        && value
                            .as_str()
                            .is_some_and(|value| value.chars().count() <= 512)
                })
        })
    })?;
    validate_optional_responses_field(
        object,
        "safety_identifier",
        "a string of at most 64 characters",
        |value| {
            value
                .as_str()
                .is_some_and(|value| value.chars().count() <= 64)
        },
    )?;
    validate_optional_responses_field(object, "tool_choice", "a string or object", |value| {
        value.is_string() || value.is_object()
    })?;
    validate_optional_responses_field(
        object,
        "top_logprobs",
        "an integer between 0 and 20",
        |value| value.as_u64().is_some_and(|value| value <= 20),
    )?;
    Ok(())
}

fn validate_optional_responses_field(
    object: &serde_json::Map<String, Value>,
    field: &str,
    expected: &str,
    valid: impl FnOnce(&Value) -> bool,
) -> Result<()> {
    if let Some(value) = object.get(field)
        && !value.is_null()
        && !valid(value)
    {
        return Err(ApiError::BadRequest(format!(
            "{field} must be {expected} or null"
        )));
    }
    Ok(())
}

pub(super) fn response_store_enabled(body: &Value) -> bool {
    body.get("store").and_then(Value::as_bool).unwrap_or(true)
}

pub(super) fn response_affinity_storage_enabled(upstream_path: &str, body: &Value) -> bool {
    upstream_path == "/responses" && response_store_enabled(body)
}
