//! Focused native-stream contract checks used by the scoped ingress work.
//!
//! The durable PostgreSQL event/lease race tests live beside the gateway store
//! implementation. These tests exercise the exact frame and terminal values
//! that the HTTP owner forwards, without contacting a provider or a production
//! service.

use keycompute_types::{
    node::NodeNativeStreamEvent,
    node_stream::{BoundedSseDecoder, NativeStreamInspector, NativeStreamProtocol},
};

#[test]
fn fragmented_native_frames_remain_byte_exact_before_terminal() {
    let raw = concat!(
        "event: content_block_delta\r\n",
        "data: {\"type\":\"content_block_delta\",\"delta\":",
        "{\"thinking_delta\":\"keep\",\"vendor\":{\"x\":1}}\r\n\r\n"
    );
    let mut decoder = BoundedSseDecoder::default();
    let mut frames = Vec::new();
    for chunk in raw.as_bytes().chunks(7) {
        frames.extend(decoder.push(chunk).unwrap());
    }
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].raw, raw);
    assert_eq!(frames[0].event.as_deref(), Some("content_block_delta"));
    assert!(frames[0].data.contains("thinking_delta"));
    assert!(frames[0].data.contains("vendor"));

    let event = NodeNativeStreamEvent::Data {
        frame: frames[0].raw.clone(),
    };
    let encoded = serde_json::to_value(event).unwrap();
    assert_eq!(encoded["frame"], raw);
}

#[test]
fn responses_terminal_outcomes_are_not_projected_to_success() {
    let mut inspector = NativeStreamInspector::new(NativeStreamProtocol::Responses);
    let frames = [
        "event: response.output_text.delta\n\
         data: {\"type\":\"response.output_text.delta\",\"response\":{\"object\":\"response\",\"id\":\"resp_1\",\"model\":\"m\",\"output\":[]},\"delta\":\"tool/reasoning\"}\n\n",
        "event: response.failed\n\
         data: {\"type\":\"response.failed\",\"response\":{\"object\":\"response\",\"id\":\"resp_1\",\"model\":\"m\",\"status\":\"failed\",\"output\":[],\"error\":{\"code\":\"x\"}}}\n\n",
    ];
    for raw in frames {
        let mut decoder = BoundedSseDecoder::default();
        let frame = decoder.push(raw.as_bytes()).unwrap().pop().unwrap();
        inspector.observe_for_model(&frame, Some("m"));
    }
    assert!(inspector.is_terminal());
    assert!(inspector.is_failed());
    assert_eq!(
        inspector.summary(200, vec![]).terminal_outcome,
        Some(keycompute_types::node_stream::NativeStreamTerminalOutcome::Failed)
    );
}
