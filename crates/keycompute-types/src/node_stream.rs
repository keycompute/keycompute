//! Bounded, lossless SSE framing and independent native protocol accounting.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fmt};
pub const MAX_NATIVE_SSE_FRAME_BYTES: usize = 256 * 1024;
pub const MAX_NATIVE_SSE_EVENTS: usize = 4096;
pub const MAX_NATIVE_SSE_TOTAL_BYTES: usize = 8 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeStreamProtocol {
    Chat,
    Messages,
    Responses,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeStreamUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
    #[serde(default = "default_true")]
    pub complete: bool,
    #[serde(default)]
    pub estimated: bool,
    #[serde(default)]
    pub input_exact: bool,
    #[serde(default)]
    pub output_exact: bool,
}
fn default_true() -> bool {
    true
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeStreamTerminalOutcome {
    Complete,
    Incomplete,
    Failed,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeNativeStreamSummary {
    pub protocol: NativeStreamProtocol,
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_body: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<NativeStreamUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_outcome: Option<NativeStreamTerminalOutcome>,
}
impl fmt::Debug for NodeNativeStreamSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeNativeStreamSummary")
            .field("protocol", &self.protocol)
            .field("status", &self.status)
            .field("usage", &self.usage)
            .field("outcome", &self.terminal_outcome)
            .field("payload", &"<redacted>")
            .finish()
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub raw: String,
    pub event: Option<String>,
    pub data: String,
}
impl fmt::Debug for SseFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SseFrame(<redacted>)")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseDecodeError {
    InvalidUtf8,
    FrameTooLarge,
    EventLimit,
    TotalLimit,
    Truncated,
}
#[derive(Default)]
pub struct BoundedSseDecoder {
    buffer: Vec<u8>,
    total_bytes: usize,
    events: usize,
    scan_from: usize,
}
impl fmt::Debug for BoundedSseDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundedSseDecoder")
            .field("buffer_bytes", &self.buffer.len())
            .field("total_bytes", &self.total_bytes)
            .field("events", &self.events)
            .finish()
    }
}
impl BoundedSseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseFrame>, SseDecodeError> {
        self.total_bytes = self
            .total_bytes
            .checked_add(bytes.len())
            .ok_or(SseDecodeError::TotalLimit)?;
        if self.total_bytes > MAX_NATIVE_SSE_TOTAL_BYTES {
            return Err(SseDecodeError::TotalLimit);
        }
        let mut frames = Vec::new();
        for chunk in bytes.chunks(16 * 1024) {
            self.buffer.extend_from_slice(chunk);
            while let Some(end) = delimiter_end(&self.buffer, self.scan_from, false) {
                frames.push(self.take_frame(end)?);
            }
            if self.buffer.len() > MAX_NATIVE_SSE_FRAME_BYTES {
                return Err(SseDecodeError::FrameTooLarge);
            }
            // Re-scan only the possible split delimiter, not the entire partial frame.
            self.scan_from = self.buffer.len().saturating_sub(4);
            if self.scan_from > 0
                && self.buffer[self.scan_from - 1] == b'\r'
                && self.buffer[self.scan_from] == b'\n'
            {
                self.scan_from -= 1;
            }
        }
        Ok(frames)
    }
    fn take_frame(&mut self, end: usize) -> Result<SseFrame, SseDecodeError> {
        if end > MAX_NATIVE_SSE_FRAME_BYTES {
            return Err(SseDecodeError::FrameTooLarge);
        }
        self.events += 1;
        if self.events > MAX_NATIVE_SSE_EVENTS {
            return Err(SseDecodeError::EventLimit);
        }
        let raw = String::from_utf8(self.buffer.drain(..end).collect())
            .map_err(|_| SseDecodeError::InvalidUtf8)?;
        self.scan_from = 0;
        Ok(parse_frame(raw, self.events == 1))
    }
    pub fn finish(&mut self) -> Result<Option<SseFrame>, SseDecodeError> {
        if self.buffer.is_empty() {
            return Ok(None);
        }
        if let Some(end) = delimiter_end(&self.buffer, 0, true)
            && end == self.buffer.len()
        {
            return self.take_frame(end).map(Some);
        }
        Err(SseDecodeError::Truncated)
    }
}
fn eol(bytes: &[u8], at: usize, eof: bool) -> Option<usize> {
    match bytes.get(at) {
        Some(b'\n') => Some(1),
        Some(b'\r') if bytes.get(at + 1) == Some(&b'\n') => Some(2),
        Some(b'\r') if at + 1 < bytes.len() || eof => Some(1),
        _ => None,
    }
}
fn delimiter_end(bytes: &[u8], from: usize, eof: bool) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        if let Some(first) = eol(bytes, i, eof) {
            if let Some(second) = eol(bytes, i + first, eof) {
                return Some(i + first + second);
            }
            i += first;
        } else {
            i += 1;
        }
    }
    None
}
fn parse_frame(raw: String, first: bool) -> SseFrame {
    let mut event = None;
    let mut data = Vec::new();
    let mut at = if first && raw.starts_with('\u{feff}') {
        3
    } else {
        0
    };
    while at < raw.len() {
        let end = (at..raw.len())
            .find(|&i| eol(raw.as_bytes(), i, true).is_some())
            .unwrap_or(raw.len());
        let line = &raw[at..end];
        if !line.starts_with(':') {
            let (name, value) = line
                .split_once(':')
                .map_or((line, ""), |(n, v)| (n, v.strip_prefix(' ').unwrap_or(v)));
            match name {
                "event" => event = Some(value.to_owned()),
                "data" => data.push(value.to_owned()),
                _ => {}
            }
        }
        if end == raw.len() {
            break;
        }
        at = end + eol(raw.as_bytes(), end, true).unwrap();
    }
    SseFrame {
        raw,
        event,
        data: data.join("\n"),
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct NativeStreamInspector {
    protocol: NativeStreamProtocol,
    seen_payload: bool,
    #[serde(default)]
    chat_choices: BTreeMap<u32, bool>,
    #[serde(default)]
    response_id: Option<String>,
    input: Option<u32>,
    output: Option<u32>,
    output_final: bool,
    output_bytes: u64,
    terminal_event: Option<String>,
    final_body: Option<Value>,
    outcome: Option<NativeStreamTerminalOutcome>,
}
impl fmt::Debug for NativeStreamInspector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeStreamInspector")
            .field("protocol", &self.protocol)
            .field("outcome", &self.outcome)
            .finish()
    }
}
impl NativeStreamInspector {
    pub fn new(protocol: NativeStreamProtocol) -> Self {
        Self {
            protocol,
            seen_payload: false,
            chat_choices: BTreeMap::new(),
            response_id: None,
            input: None,
            output: None,
            output_final: false,
            output_bytes: 0,
            terminal_event: None,
            final_body: None,
            outcome: None,
        }
    }
    pub fn observe(&mut self, frame: &SseFrame) {
        self.observe_for_model(frame, None)
    }
    pub fn observe_for_model(&mut self, frame: &SseFrame, expected: Option<&str>) {
        if frame.data.is_empty() {
            return;
        }
        if self.outcome.is_some() {
            self.fail(frame);
            return;
        }
        if self.protocol == NativeStreamProtocol::Chat && frame.data == "[DONE]" {
            if self.seen_payload
                && !self.chat_choices.is_empty()
                && self.chat_choices.values().all(|done| *done)
            {
                self.finish(frame, NativeStreamTerminalOutcome::Complete);
            } else {
                self.fail(frame);
            }
            return;
        }
        let parsed = serde_json::from_str::<Value>(&frame.data);
        let event = frame.event.as_deref().filter(|e| !e.is_empty());
        let known = event.is_none_or(|e| {
            e == "message"
                || e == "error"
                || e.starts_with("message_")
                || e.starts_with("content_block_")
                || e.starts_with("response.")
        });
        if !known {
            return;
        }
        let Ok(value) = parsed else {
            self.fail(frame);
            return;
        };
        if !value.is_object() {
            self.fail(frame);
            return;
        }
        let body_type = value.get("type").and_then(Value::as_str);
        if event.is_some_and(|e| e != "message" && body_type.is_some_and(|t| t != e)) {
            self.fail(frame);
            return;
        }
        let kind = event.or(body_type).unwrap_or("message");
        if kind == "error" {
            self.fail(frame);
            return;
        }
        let payload = match self.protocol {
            NativeStreamProtocol::Messages => value.get("message").unwrap_or(&value),
            NativeStreamProtocol::Responses => value.get("response").unwrap_or(&value),
            NativeStreamProtocol::Chat => &value,
        };
        if let (Some(expected), Some(actual)) =
            (expected, payload.get("model").and_then(Value::as_str))
            && expected != actual
        {
            self.fail(frame);
            return;
        }
        if let Some(id) = payload.get("id").or_else(|| value.get("response_id")) {
            let Some(id) = id.as_str().filter(|s| !s.is_empty()) else {
                self.fail(frame);
                return;
            };
            if self.response_id.as_deref().is_some_and(|old| old != id) {
                self.fail(frame);
                return;
            }
            self.response_id = Some(id.to_owned());
        }
        self.count_output(&value);
        let authoritative_usage = match self.protocol {
            NativeStreamProtocol::Chat => value
                .get("choices")
                .and_then(Value::as_array)
                .is_some_and(|choices| {
                    choices.is_empty()
                        || (choices
                            .iter()
                            .all(|c| c.get("finish_reason").is_some_and(|r| !r.is_null()))
                            && self.chat_choices.iter().all(|(index, done)| {
                                *done
                                    || choices.iter().any(|c| {
                                        c.get("index").and_then(Value::as_u64)
                                            == Some(u64::from(*index))
                                    })
                            }))
                }),
            NativeStreamProtocol::Messages => {
                kind == "message_delta"
                    && value
                        .pointer("/delta/stop_reason")
                        .is_some_and(|r| !r.is_null())
            }
            NativeStreamProtocol::Responses => matches!(
                kind,
                "response.completed" | "response.incomplete" | "response.failed"
            ),
        };
        if !self.observe_usage(
            payload.get("usage").or_else(|| value.get("usage")),
            authoritative_usage,
        ) {
            self.fail(frame);
            return;
        }
        match self.protocol {
            NativeStreamProtocol::Chat => {
                if value.get("error").is_some() {
                    self.fail(frame);
                    return;
                }
                let valid = value.get("object").and_then(Value::as_str)
                    == Some("chat.completion.chunk")
                    && value.get("model").and_then(Value::as_str).is_some()
                    && value.get("choices").is_some_and(Value::is_array);
                if !valid {
                    self.fail(frame);
                    return;
                }
                self.seen_payload = true;
                for choice in value["choices"].as_array().unwrap() {
                    let Some(index) = choice
                        .get("index")
                        .and_then(Value::as_u64)
                        .and_then(|n| u32::try_from(n).ok())
                    else {
                        self.fail(frame);
                        return;
                    };
                    let done = choice.get("finish_reason").is_some_and(|v| !v.is_null());
                    *self.chat_choices.entry(index).or_insert(false) |= done;
                }
            }
            NativeStreamProtocol::Messages => match kind {
                "message_start" => {
                    if self.seen_payload
                        || payload.get("id").and_then(Value::as_str).is_none()
                        || payload.get("model").and_then(Value::as_str).is_none()
                    {
                        self.fail(frame);
                        return;
                    }
                    self.seen_payload = true;
                }
                "message_stop" => {
                    if !self.seen_payload || body_type != Some("message_stop") {
                        self.fail(frame);
                        return;
                    }
                    self.finish(frame, NativeStreamTerminalOutcome::Complete);
                }
                "error" => self.fail(frame),
                _ => {}
            },
            NativeStreamProtocol::Responses => {
                if let Some(response) = value.get("response") {
                    if !response.is_object() {
                        self.fail(frame);
                        return;
                    }
                    self.final_body = Some(response.clone());
                    self.seen_payload = true;
                }
                let terminal = match kind {
                    "response.completed" => {
                        Some(("completed", NativeStreamTerminalOutcome::Complete))
                    }
                    "response.incomplete" => {
                        Some(("incomplete", NativeStreamTerminalOutcome::Incomplete))
                    }
                    "response.failed" => Some(("failed", NativeStreamTerminalOutcome::Failed)),
                    "error" => {
                        self.fail(frame);
                        return;
                    }
                    _ => None,
                };
                if let Some((status, outcome)) = terminal {
                    let valid = body_type == Some(kind)
                        && self.seen_payload
                        && payload.get("object").and_then(Value::as_str) == Some("response")
                        && payload.get("status").and_then(Value::as_str) == Some(status)
                        && payload.get("id").and_then(Value::as_str).is_some()
                        && payload.get("model").and_then(Value::as_str).is_some()
                        && payload.get("output").is_some_and(Value::is_array);
                    if !valid {
                        self.fail(frame);
                        return;
                    }
                    self.finish(frame, outcome);
                }
            }
        }
    }
    fn observe_usage(&mut self, usage: Option<&Value>, authoritative: bool) -> bool {
        let Some(usage) = usage.filter(|u| !u.is_null()) else {
            return true;
        };
        if !usage.is_object() {
            return false;
        }
        for (native, alias) in [
            ("input_tokens", "prompt_tokens"),
            ("output_tokens", "completion_tokens"),
        ] {
            if let (Some(a), Some(b)) = (usage.get(native), usage.get(alias))
                && a != b
            {
                return false;
            }
        }
        let number = |a: &str, b: &str| -> Result<Option<u32>, ()> {
            usage
                .get(a)
                .or_else(|| usage.get(b))
                .map(|v| {
                    v.as_u64()
                        .filter(|n| *n <= i32::MAX as u64)
                        .map(|n| n as u32)
                        .ok_or(())
                })
                .transpose()
        };
        let (Ok(input), Ok(output), Ok(total)) = (
            number("input_tokens", "prompt_tokens"),
            number("output_tokens", "completion_tokens"),
            number("total_tokens", "total_tokens"),
        ) else {
            return false;
        };
        if let (Some(i), Some(o), Some(t)) = (input, output, total)
            && i.checked_add(o) != Some(t)
        {
            return false;
        }
        if let Some(i) = input {
            if self.input.is_some_and(|old| old != i) {
                return false;
            }
            self.input = Some(i);
        }
        if let Some(o) = output {
            if self.output.is_some_and(|old| o < old) {
                return false;
            }
            self.output = Some(o);
        }
        if output.is_some() {
            self.output_final = authoritative;
        }
        true
    }
    fn count_output(&mut self, value: &Value) {
        fn strings(value: &Value) -> u64 {
            match value {
                Value::String(s) => s.len() as u64,
                Value::Array(a) => a.iter().map(strings).sum(),
                Value::Object(m) => m.values().map(strings).sum(),
                _ => 0,
            }
        }
        let count = match self.protocol {
            NativeStreamProtocol::Chat => value
                .get("choices")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|c| c.get("delta")).map(strings).sum())
                .unwrap_or(0),
            NativeStreamProtocol::Messages | NativeStreamProtocol::Responses => {
                value.get("delta").map(strings).unwrap_or(0)
            }
        };
        if count > 0 {
            self.output_final = false;
        }
        self.output_bytes = self.output_bytes.saturating_add(count);
    }
    fn fail(&mut self, frame: &SseFrame) {
        self.finish(frame, NativeStreamTerminalOutcome::Failed)
    }
    fn finish(&mut self, frame: &SseFrame, outcome: NativeStreamTerminalOutcome) {
        self.terminal_event = Some(frame.raw.clone());
        self.outcome = Some(outcome);
    }
    pub fn is_terminal(&self) -> bool {
        self.outcome.is_some()
    }
    pub fn is_failed(&self) -> bool {
        self.outcome == Some(NativeStreamTerminalOutcome::Failed)
    }
    pub fn summary(&self, status: u16, headers: Vec<(String, String)>) -> NodeNativeStreamSummary {
        let output = self
            .output
            .filter(|_| self.output_final)
            .unwrap_or_else(|| {
                self.output
                    .unwrap_or(0)
                    .max(((self.output_bytes.saturating_add(3)) / 4).min(i32::MAX as u64) as u32)
            });
        let estimated = self.input.is_none() || !self.output_final;
        let usage =
            (self.input.is_some() || self.output.is_some() || self.output_bytes > 0).then(|| {
                NativeStreamUsage {
                    input_tokens: self.input.unwrap_or(0),
                    output_tokens: output,
                    total_tokens: self.input.unwrap_or(0).checked_add(output),
                    complete: !estimated,
                    estimated,
                    input_exact: self.input.is_some(),
                    output_exact: self.output_final,
                }
            });
        NodeNativeStreamSummary {
            protocol: self.protocol,
            status,
            headers,
            terminal_event: self.terminal_event.clone(),
            final_body: self.final_body.clone(),
            usage,
            terminal_outcome: self.outcome,
        }
    }
}
