//! Bounded, shallow translation of platform-owned Responses SSE events.
//! The executor remains native; only resource identity and replay position change.
use crate::error::{ApiError, Result};
use keycompute_types::node_stream::{BoundedSseDecoder, MAX_NATIVE_SSE_FRAME_BYTES};
use serde_json::Value;

pub struct TranslatedEvent {
    pub frame: String,
    pub final_response: Option<Value>,
    pub terminal_status: Option<&'static str>,
}
fn invalid() -> ApiError {
    ApiError::Provider("Invalid managed Responses event".into())
}

pub fn translate(raw: &str, platform_id: &str, sequence: i64) -> Result<TranslatedEvent> {
    translate_with(raw, platform_id, sequence, |_| {})
}

pub fn translate_with(
    raw: &str,
    platform_id: &str,
    sequence: i64,
    mut decorate: impl FnMut(&mut Value),
) -> Result<TranslatedEvent> {
    if sequence < 0 || platform_id.is_empty() || platform_id.len() > 128 {
        return Err(invalid());
    }
    let mut decoder = BoundedSseDecoder::default();
    let mut frames = decoder.push(raw.as_bytes()).map_err(|_| invalid())?;
    if let Some(last) = decoder.finish().map_err(|_| invalid())? {
        frames.push(last);
    }
    if frames.len() != 1 || frames[0].raw != raw {
        return Err(invalid());
    }
    let parsed = frames.remove(0);
    let mut final_response = None;
    let mut terminal_status = None;
    let mut replacement = None;
    if !parsed.data.is_empty()
        && let Ok(mut body) = serde_json::from_str::<Value>(&parsed.data)
    {
        let kind = parsed
            .event
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| body.get("type").and_then(Value::as_str))
            .unwrap_or("")
            .to_owned();
        if kind.starts_with("response.") {
            let map = body.as_object_mut().ok_or_else(invalid)?;
            map.insert("sequence_number".into(), sequence.into());
            if map.get("response_id").is_some_and(Value::is_string) {
                map.insert("response_id".into(), platform_id.into());
            }
            if let Some(response) = map.get_mut("response")
                && response.get("object").and_then(Value::as_str) == Some("response")
            {
                decorate(response);
                response["id"] = platform_id.into();
            }
            terminal_status = match kind.as_str() {
                "response.completed" => Some("completed"),
                "response.incomplete" => Some("incomplete"),
                "response.failed" => Some("failed"),
                _ => None,
            };
            if let Some(status) = terminal_status {
                let response = map.get("response").ok_or_else(invalid)?;
                if response.get("object").and_then(Value::as_str) != Some("response")
                    || response.get("status").and_then(Value::as_str) != Some(status)
                {
                    return Err(invalid());
                }
                final_response = Some(response.clone());
            }
            replacement = Some(serde_json::to_string(&body).map_err(|_| invalid())?);
        }
    }
    let newline = if raw.contains("\r\n") {
        "\r\n"
    } else if raw.contains('\n') {
        "\n"
    } else {
        "\r"
    };
    let (bom, content) = raw
        .strip_prefix('\u{feff}')
        .map_or(("", raw), |s| ("\u{feff}", s));
    let mut frame = format!("{bom}id: {sequence}{newline}");
    let mut replaced = false;
    for line in content.split(['\r', '\n']).filter(|s| !s.is_empty()) {
        if line == "id" || line.starts_with("id:") {
            continue;
        }
        let data = line == "data" || line.starts_with("data:");
        if data && let Some(replacement) = replacement.as_ref() {
            if !replaced {
                frame.push_str("data: ");
                frame.push_str(replacement);
                frame.push_str(newline);
                replaced = true;
            }
        } else {
            frame.push_str(line);
            frame.push_str(newline);
        }
    }
    frame.push_str(newline);
    if frame.len() > MAX_NATIVE_SSE_FRAME_BYTES {
        return Err(ApiError::Provider(
            "Managed SSE event exceeds the frame limit".into(),
        ));
    }
    Ok(TranslatedEvent {
        frame,
        final_response,
        terminal_status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn data(frame: &str) -> Value {
        let mut d = BoundedSseDecoder::default();
        let parsed = d.push(frame.as_bytes()).unwrap();
        serde_json::from_str(&parsed[0].data).unwrap()
    }
    #[test]
    fn platform_ids_do_not_rewrite_tool_ids_arguments_or_nested_vendor_data() {
        let native = json!({"type":"response.completed","sequence_number":8,"response":{"object":"response","id":"upstream","status":"completed","output":[{"id":"upstream","type":"function_call","call_id":"upstream","arguments":"{\"id\":\"upstream\"}"}],"vendor":{"id":"upstream","response_id":"upstream"}}});
        let raw = format!(
            ": comment\r\nid: source\r\nretry: 1000\r\nevent: response.completed\r\ndata: {native}\r\n\r\n"
        );
        let result = translate(&raw, "resp_platform", 3).unwrap();
        let decoded = data(&result.frame);
        assert_eq!(decoded["response"]["id"], "resp_platform");
        assert_eq!(decoded["sequence_number"], 3);
        assert_eq!(decoded["response"]["output"], native["response"]["output"]);
        assert_eq!(decoded["response"]["vendor"], native["response"]["vendor"]);
        assert!(
            result.frame.starts_with("id: 3\r\n")
                && result.frame.contains(": comment")
                && result.frame.contains("retry: 1000")
        );
        assert!(!result.frame.contains("id: source"));
        assert_eq!(result.final_response, Some(decoded["response"].clone()));
        assert_eq!(result.terminal_status, Some("completed"));
    }
    #[test]
    fn response_metadata_decoration_is_identical_in_event_and_stored_result() {
        let raw = "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"u\",\"object\":\"response\",\"status\":\"completed\",\"output\":[]}}\n\n";
        let result = translate_with(raw, "resp_p", 5, |v| {
            v["store"] = true.into();
            v["background"] = true.into();
        })
        .unwrap();
        let response = data(&result.frame)["response"].clone();
        assert_eq!(Some(response.clone()), result.final_response);
        assert_eq!(response["store"], true);
        assert_eq!(response["background"], true);
    }
    #[test]
    fn delta_references_are_shallow_and_unknown_events_remain_untouched() {
        let body = json!({"type":"response.output_text.delta","response_id":"upstream","item_id":"upstream","delta":"upstream","vendor":{"response_id":"upstream"}});
        let raw = format!("event: response.output_text.delta\ndata: {body}\n\n");
        let result = translate(&raw, "resp_p", 2).unwrap();
        let decoded = data(&result.frame);
        assert_eq!(decoded["response_id"], "resp_p");
        assert_eq!(decoded["item_id"], "upstream");
        assert_eq!(decoded["vendor"], body["vendor"]);
        assert_eq!(decoded["delta"], body["delta"]);
        let unknown = translate("event: vendor\ndata: opaque data\n\n", "resp_p", 3).unwrap();
        assert!(unknown.frame.contains("data: opaque data"));
        assert!(unknown.final_response.is_none());
    }
    #[test]
    fn malformed_terminal_and_unbounded_or_multiple_frames_are_rejected() {
        assert!(translate("event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"object\":\"response\",\"status\":\"failed\"}}\n\n","r",0).is_err());
        assert!(translate("data: a\n\ndata: b\n\n", "r", 0).is_err());
        assert!(translate("data: a\n\n", "r", -1).is_err());
        assert!(translate("data: a", "r", 0).is_err());
    }
}
