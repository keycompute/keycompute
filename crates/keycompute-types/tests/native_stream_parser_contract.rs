use keycompute_types::node_stream::{
    BoundedSseDecoder, NativeStreamInspector, NativeStreamProtocol,
};
use serde_json::{Value, json};
fn frame(event: &str, value: Value) -> String {
    format!("event: {event}\r\ndata: {value}\r\n\r\n")
}
#[test]
fn mixed_sse_newlines_and_fragmented_utf8_preserve_exact_events() {
    for newline in ["\n\n", "\r\n\r\n", "\r\n\n", "\n\r\n", "\r\r"] {
        let text = format!("data: 终端{newline}");
        let mut decoder = BoundedSseDecoder::default();
        let mut frames = vec![];
        for byte in text.as_bytes() {
            frames.extend(decoder.push(&[*byte]).unwrap());
        }
        if let Some(last) = decoder.finish().unwrap() {
            frames.push(last);
        }
        assert_eq!(frames.len(), 1, "delimiter {newline:?}");
        assert_eq!(frames[0].data, "终端");
        assert_eq!(frames[0].raw, text);
    }
}
#[test]
fn message_usage_combines_initial_input_with_terminal_output() {
    let mut decoder = BoundedSseDecoder::default();
    let mut inspect = NativeStreamInspector::new(NativeStreamProtocol::Messages);
    let events = frame(
        "message_start",
        json!({"type":"message_start","message":{"id":"m","model":"test","role":"assistant","content":[],"usage":{"input_tokens":7,"output_tokens":0}}}),
    ) + &frame(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}),
    ) + &frame("message_stop", json!({"type":"message_stop"}));
    for f in decoder.push(events.as_bytes()).unwrap() {
        inspect.observe(&f);
    }
    assert!(inspect.is_terminal());
    let usage = inspect.summary(200, vec![]).usage.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (7, 3));
}
#[test]
fn response_terminal_extracts_nested_usage_and_exact_final_body() {
    let body = json!({"id":"r","object":"response","model":"test","status":"completed","output":[],"usage":{"input_tokens":7,"output_tokens":3,"total_tokens":10},"extra":{"untouched":true}});
    let mut decoder = BoundedSseDecoder::default();
    let mut inspect = NativeStreamInspector::new(NativeStreamProtocol::Responses);
    for f in decoder
        .push(
            frame(
                "response.completed",
                json!({"type":"response.completed","response":body}),
            )
            .as_bytes(),
        )
        .unwrap()
    {
        inspect.observe(&f);
    }
    let summary = inspect.summary(200, vec![]);
    assert_eq!(summary.final_body, Some(body));
    let usage = summary.usage.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (7, 3));
}
#[test]
fn chat_finish_reason_is_not_done_and_later_usage_is_retained() {
    let mut decoder = BoundedSseDecoder::default();
    let mut inspect = NativeStreamInspector::new(NativeStreamProtocol::Chat);
    let first = format!(
        "data: {}\n\n",
        json!({"id":"c","object":"chat.completion.chunk","model":"test","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
    );
    for f in decoder.push(first.as_bytes()).unwrap() {
        inspect.observe(&f);
    }
    assert!(
        !inspect.is_terminal(),
        "finish_reason precedes optional usage chunk and [DONE]"
    );
    let last = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"id":"c","object":"chat.completion.chunk","model":"test","choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}})
    );
    for f in decoder.push(last.as_bytes()).unwrap() {
        inspect.observe(&f);
    }
    assert!(inspect.is_terminal());
    assert_eq!(inspect.summary(200, vec![]).usage.unwrap().output_tokens, 3);
}
#[test]
fn truncated_event_is_not_successfully_dispatched_at_eof() {
    let mut decoder = BoundedSseDecoder::default();
    assert!(
        decoder
            .push(b"event: response.completed\ndata: {\"response\":")
            .unwrap()
            .is_empty()
    );
    assert!(
        decoder.finish().is_err(),
        "unfinished protocol event must not become a valid terminal frame"
    );
}
#[test]
fn terminal_messages_and_responses_validate_nested_model_and_usage() {
    use keycompute_types::node_stream::NativeStreamTerminalOutcome;
    for (protocol, event, body) in [
        (
            NativeStreamProtocol::Messages,
            "message_start",
            json!({"type":"message_start","message":{"id":"m","model":"wrong","usage":{"input_tokens":7,"output_tokens":0}}}),
        ),
        (
            NativeStreamProtocol::Responses,
            "response.completed",
            json!({"type":"response.completed","response":{"id":"r","object":"response","model":"wrong","status":"completed","output":[],"usage":{"input_tokens":7,"output_tokens":3,"total_tokens":10}}}),
        ),
        (
            NativeStreamProtocol::Responses,
            "response.completed",
            json!({"type":"response.completed","response":{"id":"r","object":"response","model":"test","status":"completed","output":[],"usage":{"input_tokens":7,"output_tokens":3,"total_tokens":999}}}),
        ),
    ] {
        let mut d = BoundedSseDecoder::default();
        let mut i = NativeStreamInspector::new(protocol);
        for f in d.push(frame(event, body).as_bytes()).unwrap() {
            i.observe_for_model(&f, Some("test"));
        }
        assert_eq!(
            i.summary(200, vec![]).terminal_outcome,
            Some(NativeStreamTerminalOutcome::Failed)
        );
    }
}

#[test]
fn every_chunk_width_preserves_bom_comments_multiline_and_mixed_boundaries() {
    let text = "\u{feff}event: message\r\ndata: {\"x\":\r\ndata: \"终端\"}\r\n\r\n: keepalive\n\nevent: extension\rdata: unchanged\r\r";
    let mut reference = BoundedSseDecoder::default();
    let mut expected = reference.push(text.as_bytes()).unwrap();
    if let Some(last) = reference.finish().unwrap() {
        expected.push(last);
    }
    assert_eq!(expected.len(), 3);
    assert_eq!(expected[0].event.as_deref(), Some("message"));
    assert_eq!(expected[0].data, "{\"x\":\n\"终端\"}");
    for width in 1..=text.len() {
        let mut decoder = BoundedSseDecoder::default();
        let mut actual = Vec::new();
        for bytes in text.as_bytes().chunks(width) {
            actual.extend(decoder.push(bytes).unwrap());
        }
        if let Some(last) = decoder.finish().unwrap() {
            actual.push(last);
        }
        assert_eq!(actual, expected, "transport width {width}");
        assert_eq!(
            actual.iter().map(|f| f.raw.as_str()).collect::<String>(),
            text
        );
    }
}

#[test]
fn exact_input_and_estimated_output_remain_distinguishable() {
    let mut decoder = BoundedSseDecoder::default();
    let mut inspector = NativeStreamInspector::new(NativeStreamProtocol::Messages);
    let text = frame(
        "message_start",
        json!({"type":"message_start","message":{"id":"m","model":"test","usage":{"input_tokens":7,"output_tokens":0}}}),
    ) + &frame(
        "content_block_delta",
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}),
    );
    for f in decoder.push(text.as_bytes()).unwrap() {
        inspector.observe_for_model(&f, Some("test"));
    }
    let usage = inspector.summary(200, vec![]).usage.unwrap();
    assert!(usage.input_exact && !usage.output_exact && usage.estimated && !usage.complete);
    assert_eq!(usage.input_tokens, 7);
    assert!(usage.output_tokens > 0);
}

#[test]
fn bounds_reject_oversized_frames_totals_counts_and_invalid_utf8() {
    use keycompute_types::node_stream::{
        MAX_NATIVE_SSE_EVENTS, MAX_NATIVE_SSE_FRAME_BYTES, MAX_NATIVE_SSE_TOTAL_BYTES,
        SseDecodeError,
    };
    let mut d = BoundedSseDecoder::default();
    assert_eq!(
        d.push(&vec![b'x'; MAX_NATIVE_SSE_FRAME_BYTES + 1]),
        Err(SseDecodeError::FrameTooLarge)
    );
    let mut d = BoundedSseDecoder::default();
    assert_eq!(
        d.push(&vec![b'x'; MAX_NATIVE_SSE_TOTAL_BYTES + 1]),
        Err(SseDecodeError::TotalLimit)
    );
    let mut d = BoundedSseDecoder::default();
    for _ in 0..MAX_NATIVE_SSE_EVENTS {
        assert_eq!(d.push(b": comment\n\n").unwrap().len(), 1);
    }
    assert_eq!(d.push(b": comment\n\n"), Err(SseDecodeError::EventLimit));
    assert_eq!(
        BoundedSseDecoder::default().push(b"data: \xff\n\n"),
        Err(SseDecodeError::InvalidUtf8)
    );
}

#[test]
fn large_frame_fragmented_bytewise_remains_lossless() {
    let raw = format!("data: {}\n\n", "x".repeat(128 * 1024));
    let mut d = BoundedSseDecoder::default();
    let mut frames = Vec::new();
    for byte in raw.as_bytes() {
        frames.extend(d.push(&[*byte]).unwrap());
    }
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].raw, raw);
}

#[test]
fn comments_and_unknown_extension_events_do_not_complete_or_add_usage() {
    let mut d = BoundedSseDecoder::default();
    let mut i = NativeStreamInspector::new(NativeStreamProtocol::Responses);
    for f in d
        .push(b": keepalive\n\nevent: vendor_extension\ndata: untouched non-JSON\n\n")
        .unwrap()
    {
        i.observe(&f);
    }
    let summary = i.summary(200, vec![]);
    assert!(!i.is_terminal());
    assert!(summary.usage.is_none());
}

#[test]
fn a_finished_chat_choice_does_not_complete_an_unfinished_peer() {
    let chunk = json!({"id":"c","object":"chat.completion.chunk","model":"test","choices":[
        {"index":0,"delta":{},"finish_reason":"stop"},
        {"index":1,"delta":{"content":"partial"},"finish_reason":null}
    ]});
    let text = format!("data: {chunk}\n\ndata: [DONE]\n\n");
    let mut d = BoundedSseDecoder::default();
    let mut i = NativeStreamInspector::new(NativeStreamProtocol::Chat);
    for f in d.push(text.as_bytes()).unwrap() {
        i.observe(&f);
    }
    assert!(
        i.is_failed(),
        "all emitted choices must terminate before DONE"
    );
}

#[test]
fn completed_response_identity_must_match_its_start() {
    let mut response =
        json!({"id":"first","object":"response","model":"test","status":"in_progress","output":[]});
    let mut text = frame(
        "response.created",
        json!({"type":"response.created","response":response}),
    );
    response["id"] = "different".into();
    response["status"] = "completed".into();
    text += &frame(
        "response.completed",
        json!({"type":"response.completed","response":response}),
    );
    let mut d = BoundedSseDecoder::default();
    let mut i = NativeStreamInspector::new(NativeStreamProtocol::Responses);
    for f in d.push(text.as_bytes()).unwrap() {
        i.observe_for_model(&f, Some("test"));
    }
    assert!(
        i.is_failed(),
        "events for separate responses cannot be combined"
    );
}

#[test]
fn new_output_after_a_partial_count_is_not_reported_as_final_usage() {
    let mut d = BoundedSseDecoder::default();
    let mut i = NativeStreamInspector::new(NativeStreamProtocol::Messages);
    let text = frame(
        "message_start",
        json!({"type":"message_start","message":{"id":"m","model":"test","usage":{"input_tokens":7,"output_tokens":0}}}),
    ) + &frame(
        "message_delta",
        json!({"type":"message_delta","usage":{"output_tokens":1}}),
    ) + &frame(
        "content_block_delta",
        json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"additional text after the partial count"}}),
    );
    for f in d.push(text.as_bytes()).unwrap() {
        i.observe(&f);
    }
    let usage = i.summary(200, vec![]).usage.unwrap();
    assert!(usage.input_exact && !usage.output_exact && usage.estimated);
    assert!(usage.output_tokens >= 1);
}

#[test]
fn conflicting_usage_aliases_are_rejected() {
    let body = json!({"id":"c","object":"chat.completion.chunk","model":"test","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
        "usage":{"input_tokens":7,"prompt_tokens":9,"output_tokens":3,"completion_tokens":3,"total_tokens":10}});
    let mut d = BoundedSseDecoder::default();
    let mut i = NativeStreamInspector::new(NativeStreamProtocol::Chat);
    for f in d.push(format!("data: {body}\n\n").as_bytes()).unwrap() {
        i.observe(&f);
    }
    assert!(i.is_failed());
}

#[test]
fn chat_error_event_cannot_masquerade_as_a_successful_chunk() {
    let body = json!({"id":"c","object":"chat.completion.chunk","model":"test","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
    let text = frame("error", body) + "data: [DONE]\n\n";
    let mut decoder = BoundedSseDecoder::default();
    let mut inspector = NativeStreamInspector::new(NativeStreamProtocol::Chat);
    for f in decoder.push(text.as_bytes()).unwrap() {
        inspector.observe(&f);
    }
    assert!(inspector.is_failed());
}

#[test]
fn nonterminal_usage_is_not_promoted_to_authoritative_completion() {
    let text = frame(
        "message_start",
        json!({"type":"message_start","message":{"id":"m","model":"test","usage":{"input_tokens":7,"output_tokens":0}}}),
    ) + &frame(
        "content_block_delta",
        json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"},"usage":{"output_tokens":2}}),
    ) + &frame("message_stop", json!({"type":"message_stop"}));
    let mut decoder = BoundedSseDecoder::default();
    let mut inspector = NativeStreamInspector::new(NativeStreamProtocol::Messages);
    for f in decoder.push(text.as_bytes()).unwrap() {
        inspector.observe(&f);
    }
    assert!(inspector.is_terminal());
    let usage = inspector.summary(200, vec![]).usage.unwrap();
    assert!(usage.input_exact && !usage.output_exact && usage.estimated);
}
