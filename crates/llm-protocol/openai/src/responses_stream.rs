//! OpenAI Responses typed SSE parser.
//!
//! Responses streams are not Chat Completions streams: every frame has an
//! explicit event type such as `response.created`, `response.output_text.delta`
//! or `response.completed`, and the protocol does not terminate with `[DONE]`.
//! Native events retain their parsed JSON in `StreamEvent::Native` while
//! normalized usage and completion events keep KeyCompute's billing and
//! lifecycle machinery intact.

use futures::{Stream, StreamExt};
use keycompute_types::{KeyComputeError, Result};
use llm_protocol_provider::{
    ByteStream, LARGE_JSON_BODY_ADMISSION_BYTES, LARGE_JSON_WORKING_SET_ADMISSION_BYTES,
    LARGE_NATIVE_EVENT_CHANNEL_CAPACITY, LargeBodyPermit, MAX_JSON_PASSTHROUGH_WORKING_SET_BYTES,
    NativeStreamEvent, StreamEvent, estimated_json_parse_working_set_bytes,
    try_acquire_large_body_permit,
};
use serde_json::Value;
use std::pin::Pin;
use tokio::sync::mpsc;

#[derive(Default)]
struct ResponsesStreamState {
    terminal_received: bool,
    event_received: bool,
    response_id: Option<String>,
    event_name: Option<String>,
    data: Option<String>,
    event_admission: Option<LargeBodyPermit>,
}

const MAX_RESPONSES_SSE_LINE_BYTES: usize = 96 * 1024 * 1024;
const MAX_RESPONSES_SSE_EVENT_BYTES: usize = 96 * 1024 * 1024;
const LARGE_RESPONSES_SSE_CAPACITY_ERROR: &str = "Responses SSE large-event capacity is exhausted";

/// Resource identifiers are opaque OpenAI strings. Keep only an operational
/// storage bound and reject control characters that cannot safely cross HTTP
/// and PostgreSQL text boundaries; callers must percent-encode URL segments.
pub const MAX_OPENAI_RESOURCE_ID_BYTES: usize = 2048;

pub fn valid_openai_resource_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_OPENAI_RESOURCE_ID_BYTES && !id.chars().any(char::is_control)
}

pub(crate) fn valid_response_status(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "in_progress" | "cancelled" | "queued" | "incomplete"
    )
}

fn lifecycle_event_status(body_type: &str) -> Option<&'static str> {
    match body_type {
        "response.created" | "response.in_progress" => Some("in_progress"),
        "response.queued" => Some("queued"),
        "response.completed" => Some("completed"),
        "response.failed" => Some("failed"),
        "response.incomplete" => Some("incomplete"),
        _ => None,
    }
}

fn validate_response_lifecycle_event<'a>(
    body_type: &str,
    body: &'a Value,
) -> std::result::Result<Option<&'a str>, &'static str> {
    let Some(expected_status) = lifecycle_event_status(body_type) else {
        return Ok(None);
    };

    let response = body
        .get("response")
        .and_then(Value::as_object)
        .ok_or("Responses lifecycle event is missing its response object")?;
    let response_id = response
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| valid_openai_resource_id(id))
        .ok_or("Responses lifecycle event has an invalid response id")?;
    if response.get("object").and_then(Value::as_str) != Some("response") {
        return Err("Responses lifecycle event has an invalid response object type");
    }
    let status = response
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| valid_response_status(status))
        .ok_or("Responses lifecycle event is missing a valid response status")?;
    if status != expected_status {
        return Err("Responses lifecycle event status does not match its event type");
    }
    // Failed responses still need a stable resource identity for tracing, but
    // providers may omit output when execution fails before producing any.
    if body_type != "response.failed" && !response.get("output").is_some_and(Value::is_array) {
        return Err("Responses lifecycle event response output must be an array");
    }
    Ok(Some(response_id))
}

/// Parse an OpenAI Responses typed SSE stream while preserving every event.
pub fn parse_responses_stream(
    stream: ByteStream,
) -> Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>> {
    parse_responses_stream_with_limits(
        stream,
        MAX_RESPONSES_SSE_LINE_BYTES,
        MAX_RESPONSES_SSE_EVENT_BYTES,
        MAX_JSON_PASSTHROUGH_WORKING_SET_BYTES,
        LARGE_JSON_BODY_ADMISSION_BYTES,
    )
}

/// Parse a resumed Responses retrieval stream, where a clean EOF before any
/// event means the requested cursor is already caught up. Once any event is
/// received, the same terminal-event requirement as a create stream applies.
pub fn parse_responses_stream_allow_empty(
    stream: ByteStream,
) -> Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>> {
    parse_responses_stream_with_limits_and_empty_eof(
        stream,
        MAX_RESPONSES_SSE_LINE_BYTES,
        MAX_RESPONSES_SSE_EVENT_BYTES,
        MAX_JSON_PASSTHROUGH_WORKING_SET_BYTES,
        LARGE_JSON_BODY_ADMISSION_BYTES,
        true,
    )
}

fn parse_responses_stream_with_limits(
    stream: ByteStream,
    max_line_bytes: usize,
    max_event_bytes: usize,
    max_json_working_set_bytes: usize,
    admission_threshold: usize,
) -> Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>> {
    parse_responses_stream_with_limits_and_empty_eof(
        stream,
        max_line_bytes,
        max_event_bytes,
        max_json_working_set_bytes,
        admission_threshold,
        false,
    )
}

fn parse_responses_stream_with_limits_and_empty_eof(
    stream: ByteStream,
    max_line_bytes: usize,
    max_event_bytes: usize,
    max_json_working_set_bytes: usize,
    admission_threshold: usize,
    allow_empty_eof: bool,
) -> Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>> {
    let (tx, rx) = mpsc::channel(LARGE_NATIVE_EVENT_CHANNEL_CAPACITY);

    tokio::spawn(async move {
        let mut stream = stream;
        let mut buffer = Vec::new();
        let mut state = ResponsesStreamState::default();

        loop {
            let Some(chunk) = (tokio::select! {
                _ = tx.closed() => return,
                chunk = stream.next() => chunk,
            }) else {
                break;
            };
            match chunk {
                Ok(chunk) => {
                    let retained_bytes = pending_sse_bytes(&buffer, &state);
                    if state.event_admission.is_none()
                        && chunk.len() > admission_threshold.saturating_sub(retained_bytes)
                    {
                        let Some(admission) = try_acquire_large_body_permit() else {
                            // Waiting here would retain up to the ordinary-event
                            // allowance per request and turn the semaphore into
                            // an unbounded memory queue. Load-shed before adding
                            // the chunk to the parser buffer instead.
                            send_error(&tx, LARGE_RESPONSES_SSE_CAPACITY_ERROR).await;
                            return;
                        };
                        state.event_admission = Some(admission);
                    }
                    buffer.extend_from_slice(&chunk);
                    while let Some(line) = take_next_sse_line(&mut buffer, false) {
                        if !handle_line_bytes(
                            &tx,
                            &line,
                            &mut state,
                            max_line_bytes,
                            max_event_bytes,
                            max_json_working_set_bytes,
                            admission_threshold,
                        )
                        .await
                        {
                            return;
                        }
                    }
                    if pending_sse_line_bytes(&buffer) > max_line_bytes {
                        send_error(
                            &tx,
                            format!("Responses SSE line exceeds the {max_line_bytes}-byte limit"),
                        )
                        .await;
                        return;
                    }
                    release_small_parser_admission(&mut buffer, &mut state, admission_threshold);
                }
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            }
        }

        while let Some(line) = take_next_sse_line(&mut buffer, true) {
            if !handle_line_bytes(
                &tx,
                &line,
                &mut state,
                max_line_bytes,
                max_event_bytes,
                max_json_working_set_bytes,
                admission_threshold,
            )
            .await
            {
                return;
            }
        }
        if !buffer.is_empty() {
            let line = std::mem::take(&mut buffer);
            if !handle_line_bytes(
                &tx,
                &line,
                &mut state,
                max_line_bytes,
                max_event_bytes,
                max_json_working_set_bytes,
                admission_threshold,
            )
            .await
            {
                return;
            }
        }

        // A final frame does not have to be followed by a blank line. Flush a
        // completely received data field at EOF, but never turn EOF itself
        // into success.
        if let Some(data) = state.data.take() {
            let event_name = state.event_name.take();
            let admission = state.event_admission.take();
            if !dispatch_event(
                &tx,
                event_name.as_deref(),
                &data,
                admission,
                &mut state,
                max_json_working_set_bytes,
            )
            .await
            {
                return;
            }
        }
        if !state.terminal_received && !(allow_empty_eof && !state.event_received) {
            send_error(
                &tx,
                "Responses stream ended without a terminal response.completed, response.failed, response.incomplete, or error event",
            )
            .await;
        }
    });

    Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}

/// Take one SSE line while treating CR, LF, and CRLF as terminators. A CR at
/// the end of a network chunk is held until the next chunk distinguishes bare
/// CR from a split CRLF sequence.
fn take_next_sse_line(buffer: &mut Vec<u8>, eof: bool) -> Option<Vec<u8>> {
    let position = buffer
        .iter()
        .position(|byte| matches!(*byte, b'\r' | b'\n'))?;
    let delimiter_bytes = match buffer[position] {
        b'\r' if position + 1 == buffer.len() && !eof => return None,
        b'\r' if buffer.get(position + 1) == Some(&b'\n') => 2,
        _ => 1,
    };
    let mut line = buffer
        .drain(..position.saturating_add(delimiter_bytes))
        .collect::<Vec<_>>();
    line.truncate(position);
    Some(line)
}

fn pending_sse_line_bytes(buffer: &[u8]) -> usize {
    buffer
        .len()
        .saturating_sub(usize::from(buffer.last() == Some(&b'\r')))
}

async fn handle_line_bytes(
    tx: &mpsc::Sender<Result<StreamEvent>>,
    line: &[u8],
    state: &mut ResponsesStreamState,
    max_line_bytes: usize,
    max_event_bytes: usize,
    max_json_working_set_bytes: usize,
    admission_threshold: usize,
) -> bool {
    if line.len() > max_line_bytes {
        send_error(
            tx,
            format!("Responses SSE line exceeds the {max_line_bytes}-byte limit"),
        )
        .await;
        return false;
    }
    let line = match std::str::from_utf8(line) {
        Ok(line) => line,
        Err(error) => {
            send_error(
                tx,
                format!("Responses stream contained invalid UTF-8: {error}"),
            )
            .await;
            return false;
        }
    };
    handle_line(
        tx,
        line,
        state,
        max_event_bytes,
        max_json_working_set_bytes,
        admission_threshold,
    )
    .await
}

fn pending_sse_bytes(buffer: &[u8], state: &ResponsesStreamState) -> usize {
    buffer
        .len()
        .saturating_add(state.data.as_ref().map_or(0, String::len))
        .saturating_add(state.event_name.as_ref().map_or(0, String::len))
}

fn release_small_parser_admission(
    buffer: &mut Vec<u8>,
    state: &mut ResponsesStreamState,
    admission_threshold: usize,
) {
    if state.event_admission.is_none() || pending_sse_bytes(buffer, state) > admission_threshold {
        return;
    }
    // `drain` retains the old allocation even after a complete frame leaves the
    // parser. Release that capacity before releasing the guard, otherwise an
    // early large chunk can keep a large unaccounted Vec alive for the rest of
    // a slow stream.
    buffer.shrink_to_fit();
    if let Some(data) = state.data.as_mut() {
        data.shrink_to_fit();
    }
    state.event_admission = None;
}

async fn handle_line(
    tx: &mpsc::Sender<Result<StreamEvent>>,
    line: &str,
    state: &mut ResponsesStreamState,
    max_event_bytes: usize,
    max_json_working_set_bytes: usize,
    admission_threshold: usize,
) -> bool {
    if line.is_empty() {
        let event_name = state.event_name.take();
        let Some(data) = state.data.take() else {
            return true;
        };
        // The same upstream chunk may contain this complete event followed by
        // another large partial frame. Share the permit with the emitted event
        // until the caller confirms that the parser's own retained bytes are
        // small enough to release its copy.
        let admission = state.event_admission.clone();
        return dispatch_event(
            tx,
            event_name.as_deref(),
            &data,
            admission,
            state,
            max_json_working_set_bytes,
        )
        .await;
    }
    // SSE comments/keep-alives and fields unrelated to event/data are ignored.
    if line.starts_with(':') {
        return true;
    }
    if let Some(name) = line.strip_prefix("event:") {
        state.event_name = Some(name.trim().to_string());
    } else if let Some(data) = line.strip_prefix("data:") {
        let data = data.strip_prefix(' ').unwrap_or(data);
        if data.trim() == "[DONE]" {
            // `[DONE]` is not part of the Responses protocol. Compatibility
            // gateways may append it after the actual terminal event; it must
            // not manufacture a second Done event.
            return true;
        }
        let separator_bytes = usize::from(state.data.is_some());
        let current_bytes = state.data.as_ref().map_or(0, String::len);
        let Some(next_bytes) = current_bytes
            .checked_add(separator_bytes)
            .and_then(|bytes| bytes.checked_add(data.len()))
        else {
            send_error(tx, "Responses SSE event size overflow").await;
            return false;
        };
        if next_bytes > max_event_bytes {
            send_error(
                tx,
                format!("Responses SSE event exceeds the {max_event_bytes}-byte limit"),
            )
            .await;
            return false;
        }
        if state.event_admission.is_none() && next_bytes > admission_threshold {
            let Some(admission) = try_acquire_large_body_permit() else {
                send_error(tx, LARGE_RESPONSES_SSE_CAPACITY_ERROR).await;
                return false;
            };
            state.event_admission = Some(admission);
        }
        let buffered = state.data.get_or_insert_with(String::new);
        let additional_bytes = separator_bytes.saturating_add(data.len());
        if buffered.try_reserve_exact(additional_bytes).is_err() {
            send_error(tx, "Responses SSE event allocation failed").await;
            return false;
        }
        if separator_bytes != 0 {
            buffered.push('\n');
        }
        buffered.push_str(data);
    }
    true
}

async fn dispatch_event(
    tx: &mpsc::Sender<Result<StreamEvent>>,
    event_header: Option<&str>,
    data: &str,
    mut admission: Option<LargeBodyPermit>,
    state: &mut ResponsesStreamState,
    max_json_working_set_bytes: usize,
) -> bool {
    let working_set_bytes = estimated_json_parse_working_set_bytes(data.as_bytes());
    if working_set_bytes > max_json_working_set_bytes {
        send_error(
            tx,
            format!(
                "Responses SSE JSON exceeds the {max_json_working_set_bytes}-byte working-set limit"
            ),
        )
        .await;
        return false;
    }
    if admission.is_none() && working_set_bytes > LARGE_JSON_WORKING_SET_ADMISSION_BYTES {
        let Some(acquired) = try_acquire_large_body_permit() else {
            send_error(tx, LARGE_RESPONSES_SSE_CAPACITY_ERROR).await;
            return false;
        };
        admission = Some(acquired);
    }
    let body: Value = match serde_json::from_str(data) {
        Ok(body) => body,
        Err(error) => {
            send_error(
                tx,
                format!("Failed to parse Responses stream event: {error}"),
            )
            .await;
            return false;
        }
    };
    let Some(body_type) = body.get("type").and_then(Value::as_str) else {
        send_error(tx, "Responses stream event missing type").await;
        return false;
    };
    let event = match event_header.filter(|name| !name.is_empty()) {
        Some(event) if event != body_type => {
            send_error(tx, "Responses SSE event name does not match data.type").await;
            return false;
        }
        Some(event) => event,
        None => body_type,
    };
    state.event_received = true;
    let lifecycle_response_id = match validate_response_lifecycle_event(body_type, &body) {
        Ok(response_id) => response_id,
        Err(message) => {
            send_error(tx, message).await;
            return false;
        }
    };
    if let Some(response_id) = lifecycle_response_id {
        match state.response_id.as_deref() {
            Some(expected) if expected != response_id => {
                send_error(tx, "Responses lifecycle events changed response id").await;
                return false;
            }
            None => state.response_id = Some(response_id.to_string()),
            Some(_) => {}
        }
    }
    let usage = match response_usage(&body) {
        Ok(usage) => usage,
        Err(error) => {
            let _ = tx.send(Err(error)).await;
            return false;
        }
    };
    let is_terminal = matches!(
        body_type,
        "response.completed" | "response.failed" | "response.incomplete"
    );
    if body_type == "error" {
        // Keep the complete structured event. The gateway recognizes this
        // native error as non-committing, defers it while trying fallbacks and
        // publishes only the final attempted account's event.
        state.terminal_received = true;
        let native = StreamEvent::native(NativeStreamEvent::OpenAiResponsesSse {
            event: event.to_string(),
            data: body,
            admission,
        });
        let _ = tx.send(Ok(native)).await;
        return false;
    }

    let native = StreamEvent::native(NativeStreamEvent::OpenAiResponsesSse {
        event: event.to_string(),
        data: body,
        admission,
    });
    if tx.send(Ok(native)).await.is_err() {
        return false;
    }

    if let Some((input_tokens, output_tokens)) = usage
        && tx
            .send(Ok(StreamEvent::Usage {
                input_tokens,
                output_tokens,
            }))
            .await
            .is_err()
    {
        return false;
    }

    if is_terminal {
        state.terminal_received = true;
        let _ = tx.send(Ok(StreamEvent::Done)).await;
        false
    } else {
        true
    }
}

/// Extract exact billable token totals from either a response object or a
/// typed terminal event containing a response object.
pub(crate) fn response_usage(value: &Value) -> Result<Option<(u32, u32)>> {
    let Some(usage) = value
        .pointer("/response/usage")
        .or_else(|| value.get("usage"))
    else {
        return Ok(None);
    };
    if usage.is_null() {
        return Ok(None);
    }
    let usage = usage.as_object().ok_or_else(|| {
        KeyComputeError::ProviderError("Responses usage must be an object".to_string())
    })?;
    let tokens = |field: &str| -> Result<u32> {
        let value = usage.get(field).and_then(Value::as_u64).ok_or_else(|| {
            KeyComputeError::ProviderError(format!(
                "Responses usage.{field} must be an unsigned integer"
            ))
        })?;
        u32::try_from(value).map_err(|_| {
            KeyComputeError::ProviderError(format!("Responses usage.{field} exceeds u32"))
        })
    };
    Ok(Some((tokens("input_tokens")?, tokens("output_tokens")?)))
}

async fn send_error(tx: &mpsc::Sender<Result<StreamEvent>>, message: impl Into<String>) {
    let _ = tx
        .send(Err(KeyComputeError::ProviderError(message.into())))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    #[test]
    fn resource_id_validation_is_format_agnostic_but_bounded() {
        assert!(valid_openai_resource_id("future.response/id:1"));
        assert!(valid_openai_resource_id("资源.一"));
        assert!(!valid_openai_resource_id(""));
        assert!(!valid_openai_resource_id("bad\nresource"));
        assert!(valid_openai_resource_id(
            &"x".repeat(MAX_OPENAI_RESOURCE_ID_BYTES)
        ));
        assert!(!valid_openai_resource_id(
            &"x".repeat(MAX_OPENAI_RESOURCE_ID_BYTES + 1)
        ));
    }

    #[test]
    fn response_status_validation_tracks_lifecycle_event_semantics() {
        for status in [
            "completed",
            "failed",
            "in_progress",
            "cancelled",
            "queued",
            "incomplete",
        ] {
            assert!(valid_response_status(status));
        }
        assert!(!valid_response_status("future_status"));

        for (event, status) in [
            ("response.created", "in_progress"),
            ("response.queued", "queued"),
            ("response.in_progress", "in_progress"),
            ("response.completed", "completed"),
            ("response.failed", "failed"),
            ("response.incomplete", "incomplete"),
        ] {
            assert_eq!(lifecycle_event_status(event), Some(status));
        }
        assert_eq!(lifecycle_event_status("response.output_text.delta"), None);
    }

    #[tokio::test]
    async fn stops_prefetching_when_the_single_large_event_slot_is_full() {
        let source_polls = Arc::new(AtomicUsize::new(0));
        let observed_polls = Arc::clone(&source_polls);
        let source = futures::stream::unfold(0usize, move |index| {
            let observed_polls = Arc::clone(&observed_polls);
            async move {
                (index < 4).then(|| {
                    observed_polls.fetch_add(1, Ordering::SeqCst);
                    let chunk = format!(
                        "event: response.output_text.delta\ndata: {{\"type\":\"response.output_text.delta\",\"delta\":\"{index}\"}}\n\n"
                    );
                    (Ok(bytes::Bytes::from(chunk)), index + 1)
                })
            }
        });

        let parsed = parse_responses_stream(Box::pin(source));
        for _ in 0..100 {
            if source_polls.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::task::yield_now().await;
        }

        assert_eq!(
            source_polls.load(Ordering::SeqCst),
            2,
            "one event may be queued and one may await the occupied slot, but the parser must not prefetch another chunk"
        );
        drop(parsed);
    }

    #[tokio::test]
    async fn large_sse_events_fail_fast_when_the_process_wide_budget_is_full() {
        fn large_event(label: &str) -> ByteStream {
            let chunk = format!(
                "event: response.output_text.delta\ndata: {{\"type\":\"response.output_text.delta\",\"delta\":\"{label}-abcdefghijklmnopqrstuvwxyz\"}}\n\n"
            );
            Box::pin(futures::stream::once(async move {
                Ok::<_, KeyComputeError>(bytes::Bytes::from(chunk))
            }))
        }
        fn fragmented_large_event() -> ByteStream {
            Box::pin(futures::stream::iter(vec![
                Ok::<_, KeyComputeError>(bytes::Bytes::from_static(
                    b"event: response.output_text.delta\n",
                )),
                Ok(bytes::Bytes::from_static(
                    b"data: {\"type\":\"response.output_text.delta\",\n",
                )),
                Ok(bytes::Bytes::from_static(
                    b"data: \"delta\":\"abcdefghijklmnopqrstuvwxyz\"}\n",
                )),
                Ok(bytes::Bytes::from_static(b"\n")),
            ]))
        }
        fn stalled_large_tail(waiting: Arc<AtomicUsize>) -> ByteStream {
            let chunk = format!(": keepalive\n\n{}", "x".repeat(128));
            let initial = futures::stream::once(async move {
                Ok::<_, KeyComputeError>(bytes::Bytes::from(chunk))
            });
            let mut notified = false;
            let stalled = futures::stream::poll_fn(move |_| {
                if !notified {
                    waiting.fetch_add(1, Ordering::SeqCst);
                    notified = true;
                }
                std::task::Poll::Pending
            });
            Box::pin(initial.chain(stalled))
        }
        fn parse(stream: ByteStream) -> Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>> {
            parse_responses_stream_with_limits(stream, 1024, 1024, usize::MAX, 64)
        }

        let mut first = parse(large_event("first"));
        let mut second = parse(large_event("second"));
        let mut third = parse(large_event("third"));
        let first_event = first.next().await.unwrap().unwrap();
        let second_event = second.next().await.unwrap().unwrap();
        assert!(matches!(
            &first_event,
            StreamEvent::Native {
                event: NativeStreamEvent::OpenAiResponsesSse {
                    admission: Some(_),
                    ..
                }
            }
        ));
        assert!(matches!(
            &second_event,
            StreamEvent::Native {
                event: NativeStreamEvent::OpenAiResponsesSse {
                    admission: Some(_),
                    ..
                }
            }
        ));

        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), third.next())
                .await
                .expect("an over-capacity parser must not wait while retaining its buffer"),
            Some(Err(KeyComputeError::ProviderError(message)))
                if message.contains("capacity is exhausted")
        ));

        let mut fragmented = parse(fragmented_large_event());
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), fragmented.next())
                .await
                .expect("a fragmented over-capacity event must not wait while retaining data"),
            Some(Err(KeyComputeError::ProviderError(message)))
                if message.contains("capacity is exhausted")
        ));

        drop(first_event);
        let mut after_release = parse(large_event("after-release"));
        let admitted_event = tokio::time::timeout(Duration::from_secs(1), after_release.next())
            .await
            .expect("a released admission slot should admit a new parser")
            .expect("a parser should produce an event after capacity is released")
            .expect("the event after release should be valid");
        assert!(matches!(
            admitted_event,
            StreamEvent::Native {
                event: NativeStreamEvent::OpenAiResponsesSse {
                    admission: Some(_),
                    ..
                }
            }
        ));
        drop(second_event);
        drop(admitted_event);

        let waiting = Arc::new(AtomicUsize::new(0));
        let retained_first = parse(stalled_large_tail(Arc::clone(&waiting)));
        let retained_second = parse(stalled_large_tail(Arc::clone(&waiting)));
        for _ in 0..100 {
            if waiting.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            waiting.load(Ordering::SeqCst),
            2,
            "both parsers should retain their partial tail while waiting upstream"
        );

        let mut over_capacity = parse(large_event("after-empty-frame"));
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), over_capacity.next())
                .await
                .expect("a large partial tail must continue to consume admission"),
            Some(Err(KeyComputeError::ProviderError(message)))
                if message.contains("capacity is exhausted")
        ));
        drop(retained_first);
        drop(retained_second);
    }

    #[tokio::test]
    async fn preserves_typed_events_usage_and_unicode_across_chunks() {
        let source = futures::stream::iter(vec![
            Ok(bytes::Bytes::from_static(
                b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"\xe4",
            )),
            Ok(bytes::Bytes::from_static(
                b"\xbd\xa0\",\"sequence_number\":1}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":12,\"output_tokens\":7,\"total_tokens\":19}}}\n\n",
            )),
        ]);
        let mut parsed = parse_responses_stream(Box::pin(source));

        let StreamEvent::Native {
            event: NativeStreamEvent::OpenAiResponsesSse { event, data, .. },
        } = parsed.next().await.unwrap().unwrap()
        else {
            panic!("expected typed delta");
        };
        assert_eq!(event, "response.output_text.delta");
        assert_eq!(data["delta"], "你");

        assert!(matches!(
            parsed.next().await,
            Some(Ok(StreamEvent::Native {
                event: NativeStreamEvent::OpenAiResponsesSse { .. }
            }))
        ));
        assert!(matches!(
            parsed.next().await,
            Some(Ok(StreamEvent::Usage {
                input_tokens: 12,
                output_tokens: 7
            }))
        ));
        assert!(matches!(parsed.next().await, Some(Ok(StreamEvent::Done))));
        assert!(parsed.next().await.is_none());
    }

    #[tokio::test]
    async fn accepts_bare_cr_and_fragmented_crlf_line_endings() {
        let bare_cr: ByteStream = Box::pin(futures::stream::once(async {
            Ok::<_, KeyComputeError>(bytes::Bytes::from_static(
                b"event: response.completed\rdata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_cr\",\"object\":\"response\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":2,\"total_tokens\":5}}}\r\r",
            ))
        }));
        let fragmented_crlf: ByteStream = Box::pin(futures::stream::iter([
            Ok::<_, KeyComputeError>(bytes::Bytes::from_static(
                b"event: response.completed\r",
            )),
            Ok(bytes::Bytes::from_static(
                b"\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_crlf\",\"object\":\"response\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":2,\"total_tokens\":5}}}\r",
            )),
            Ok(bytes::Bytes::from_static(b"\n\r")),
            Ok(bytes::Bytes::from_static(b"\n")),
        ]));

        for source in [bare_cr, fragmented_crlf] {
            let events = parse_responses_stream(source).collect::<Vec<_>>().await;
            assert!(matches!(
                events.as_slice(),
                [
                    Ok(StreamEvent::Native { .. }),
                    Ok(StreamEvent::Usage {
                        input_tokens: 3,
                        output_tokens: 2
                    }),
                    Ok(StreamEvent::Done)
                ]
            ));
        }
    }

    #[tokio::test]
    async fn accepts_official_null_usage_until_the_terminal_event() {
        let source = futures::stream::iter(vec![Ok(bytes::Bytes::from_static(
            br#"event: response.created
data: {"type":"response.created","response":{"id":"resp_1","object":"response","status":"in_progress","output":[],"usage":null}}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1","object":"response","status":"completed","output":[],"usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}}}

"#,
        ))]);
        let mut parsed = parse_responses_stream(Box::pin(source));

        assert!(matches!(
            parsed.next().await,
            Some(Ok(StreamEvent::Native {
                event: NativeStreamEvent::OpenAiResponsesSse { .. }
            }))
        ));
        assert!(matches!(
            parsed.next().await,
            Some(Ok(StreamEvent::Native {
                event: NativeStreamEvent::OpenAiResponsesSse { .. }
            }))
        ));
        assert!(matches!(
            parsed.next().await,
            Some(Ok(StreamEvent::Usage {
                input_tokens: 3,
                output_tokens: 2
            }))
        ));
        assert!(matches!(parsed.next().await, Some(Ok(StreamEvent::Done))));
        assert!(parsed.next().await.is_none());
    }

    #[tokio::test]
    async fn accepts_opaque_response_resource_ids() {
        let source = futures::stream::once(async {
            Ok::<_, KeyComputeError>(bytes::Bytes::from_static(
                br#"event: response.created
data: {"type":"response.created","response":{"id":"future.response/id:1","object":"response","status":"in_progress","output":[]}}

event: response.completed
data: {"type":"response.completed","response":{"id":"future.response/id:1","object":"response","status":"completed","output":[],"usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}}}

"#,
            ))
        });
        let events = parse_responses_stream(Box::pin(source))
            .collect::<Vec<_>>()
            .await;

        assert!(events.iter().all(Result::is_ok));
        assert!(matches!(events.last(), Some(Ok(StreamEvent::Done))));
    }

    #[tokio::test]
    async fn rejects_lifecycle_events_that_change_response_identity() {
        let source = futures::stream::once(async {
            Ok::<_, KeyComputeError>(bytes::Bytes::from_static(
                br#"event: response.created
data: {"type":"response.created","response":{"id":"resp_first","object":"response","status":"in_progress","output":[]}}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_second","object":"response","status":"completed","output":[],"usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}}}

"#,
            ))
        });
        let mut parsed = parse_responses_stream(Box::pin(source));

        assert!(matches!(
            parsed.next().await,
            Some(Ok(StreamEvent::Native {
                event: NativeStreamEvent::OpenAiResponsesSse { .. }
            }))
        ));
        assert!(matches!(
            parsed.next().await,
            Some(Err(KeyComputeError::ProviderError(message)))
                if message.contains("changed response id")
        ));
        assert!(parsed.next().await.is_none());
    }

    #[tokio::test]
    async fn rejects_lifecycle_events_without_a_persistable_response_resource() {
        for data in [
            r#"{"type":"response.completed"}"#,
            r#"{"type":"response.completed","response":{"id":"","object":"response","status":"completed","output":[]}}"#,
            r#"{"type":"response.completed","response":{"id":"bad\nresource","object":"response","status":"completed","output":[]}}"#,
            r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed","output":[]}}"#,
            r#"{"type":"response.completed","response":{"id":"resp_1","object":"response","status":"completed"}}"#,
            r#"{"type":"response.completed","response":{"id":"resp_1","object":"response","status":"future_status","output":[]}}"#,
            r#"{"type":"response.completed","response":{"id":"resp_1","object":"response","status":"in_progress","output":[]}}"#,
        ] {
            let source = futures::stream::once(async move {
                Ok::<_, KeyComputeError>(bytes::Bytes::from(format!(
                    "event: response.completed\ndata: {data}\n\n"
                )))
            });
            let mut parsed = parse_responses_stream(Box::pin(source));

            assert!(matches!(
                parsed.next().await,
                Some(Err(KeyComputeError::ProviderError(message)))
                    if message.contains("Responses lifecycle event")
            ));
            assert!(parsed.next().await.is_none());
        }
    }

    #[tokio::test]
    async fn empty_eof_is_only_accepted_for_resumed_retrieval() {
        let empty = || {
            Box::pin(futures::stream::empty::<
                std::result::Result<bytes::Bytes, KeyComputeError>,
            >()) as ByteStream
        };

        let mut strict = parse_responses_stream(empty());
        assert!(matches!(
            strict.next().await,
            Some(Err(KeyComputeError::ProviderError(message)))
                if message.contains("terminal")
        ));

        let mut resumed = parse_responses_stream_allow_empty(empty());
        assert!(resumed.next().await.is_none());
    }

    #[tokio::test]
    async fn non_empty_eof_without_a_terminal_event_is_always_truncated() {
        let source = futures::stream::iter(vec![Ok(bytes::Bytes::from_static(
            b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
        ))]);
        let mut parsed = parse_responses_stream(Box::pin(source));
        assert!(matches!(
            parsed.next().await,
            Some(Ok(StreamEvent::Native {
                event: NativeStreamEvent::OpenAiResponsesSse { .. }
            }))
        ));
        assert!(matches!(
            parsed.next().await,
            Some(Err(KeyComputeError::ProviderError(message)))
                if message.contains("terminal")
        ));

        let source = futures::stream::iter(vec![Ok(bytes::Bytes::from_static(
            b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
        ))]);
        let mut resumed = parse_responses_stream_allow_empty(Box::pin(source));
        assert!(matches!(
            resumed.next().await,
            Some(Ok(StreamEvent::Native {
                event: NativeStreamEvent::OpenAiResponsesSse { .. }
            }))
        ));
        assert!(matches!(
            resumed.next().await,
            Some(Err(KeyComputeError::ProviderError(message)))
                if message.contains("terminal")
        ));
    }

    #[tokio::test]
    async fn rejects_sse_event_header_and_body_type_mismatches() {
        for frame in [
            b"event: response.completed\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n"
                .as_slice(),
            b"event: response.output_text.delta\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\"}}\n\n"
                .as_slice(),
        ] {
            let source = futures::stream::once(async move {
                Ok::<_, KeyComputeError>(bytes::Bytes::copy_from_slice(frame))
            });
            let mut parsed = parse_responses_stream(Box::pin(source));

            assert!(matches!(
                parsed.next().await,
                Some(Err(KeyComputeError::ProviderError(message)))
                    if message.contains("does not match data.type")
            ));
            assert!(parsed.next().await.is_none());
        }
    }

    #[tokio::test]
    async fn official_error_frame_remains_uncommitted_for_gateway_fallback() {
        let source = futures::stream::iter(vec![Ok(bytes::Bytes::from_static(
            b"event: error\ndata: {\"type\":\"error\",\"code\":\"server_error\",\"message\":\"boom\",\"sequence_number\":2}\n\n",
        ))]);
        let mut parsed = parse_responses_stream(Box::pin(source));
        let Some(Ok(StreamEvent::Native {
            event: NativeStreamEvent::OpenAiResponsesSse { event, data, .. },
        })) = parsed.next().await
        else {
            panic!("expected the structured upstream error event");
        };
        assert_eq!(event, "error");
        assert_eq!(data["code"], "server_error");
        assert_eq!(data["message"], "boom");
        assert_eq!(data["sequence_number"], 2);
        assert!(parsed.next().await.is_none());
    }

    #[tokio::test]
    async fn rejects_dense_sse_json_before_tree_allocation() {
        let data = r#"{"type":"response.created","items":[0,0,0,0]}"#;
        let source = futures::stream::once(async move {
            Ok::<_, KeyComputeError>(bytes::Bytes::from(format!("data: {data}\n\n")))
        });
        let max_working_set = estimated_json_parse_working_set_bytes(data.as_bytes()) - 1;
        let mut parsed = parse_responses_stream_with_limits(
            Box::pin(source),
            1024,
            1024,
            max_working_set,
            LARGE_JSON_BODY_ADMISSION_BYTES,
        );

        assert!(matches!(
            parsed.next().await,
            Some(Err(KeyComputeError::ProviderError(message)))
                if message.contains("working-set limit")
        ));
        assert!(parsed.next().await.is_none());
    }

    #[tokio::test]
    async fn rejects_a_line_that_never_terminates_before_the_limit() {
        let source =
            futures::stream::iter(vec![Ok(bytes::Bytes::from_static(b"data: 12345678901"))]);
        let mut parsed = parse_responses_stream_with_limits(
            Box::pin(source),
            16,
            64,
            usize::MAX,
            LARGE_JSON_BODY_ADMISSION_BYTES,
        );

        assert!(matches!(
            parsed.next().await,
            Some(Err(KeyComputeError::ProviderError(message)))
                if message.contains("line exceeds")
        ));
        assert!(parsed.next().await.is_none());
    }

    #[tokio::test]
    async fn rejects_multiline_event_data_over_the_aggregate_limit() {
        let source = futures::stream::iter(vec![Ok(bytes::Bytes::from_static(
            b"data: 12345\ndata: 67890\n\n",
        ))]);
        let mut parsed = parse_responses_stream_with_limits(
            Box::pin(source),
            32,
            10,
            usize::MAX,
            LARGE_JSON_BODY_ADMISSION_BYTES,
        );

        assert!(matches!(
            parsed.next().await,
            Some(Err(KeyComputeError::ProviderError(message)))
                if message.contains("event exceeds")
        ));
        assert!(parsed.next().await.is_none());
    }

    #[tokio::test]
    async fn empty_data_lines_are_bounded_by_the_aggregate_limit() {
        let source = futures::stream::once(async {
            Ok::<_, KeyComputeError>(bytes::Bytes::from_static(
                b"data:\ndata:\ndata:\ndata:\ndata:\n",
            ))
        });
        let mut parsed = parse_responses_stream_with_limits(
            Box::pin(source),
            32,
            3,
            usize::MAX,
            LARGE_JSON_BODY_ADMISSION_BYTES,
        );

        assert!(matches!(
            parsed.next().await,
            Some(Err(KeyComputeError::ProviderError(message)))
                if message.contains("event exceeds")
        ));
        assert!(parsed.next().await.is_none());
    }
}
