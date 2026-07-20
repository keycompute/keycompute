//! OpenAI SSE 流解析
//!
//! 将 OpenAI 的 SSE 流解析为标准化的 StreamEvent

use futures::{Stream, StreamExt};
use keycompute_provider_trait::ByteStream;
use keycompute_provider_trait::StreamEvent;
use keycompute_provider_trait::stream::sse;
use keycompute_types::{KeyComputeError, Result};
use std::pin::Pin;
use tokio::sync::mpsc;

use crate::protocol::OpenAIStreamResponse;

/// 解析 OpenAI SSE 流
///
/// 将 HTTP 传输层的字节流转换为标准化的 StreamEvent 流
pub fn parse_openai_stream(
    stream: ByteStream,
) -> Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>> {
    let (tx, rx) = mpsc::channel::<Result<StreamEvent>>(100);

    // 在 spawn 外记录日志，确保能输出
    tracing::info!("parse_openai_stream: function called, spawning task");

    tokio::spawn(async move {
        let mut buffer = String::new();
        let mut stream = stream;
        let mut chunk_count = 0u32;
        let mut total_bytes = 0usize;

        // 使用 eprintln! 确保输出到 stderr
        eprintln!("[DEBUG] parse_openai_stream: task started, waiting for chunks");

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    chunk_count += 1;
                    total_bytes += chunk.len();

                    // 前 5 个 chunk 打印详细信息
                    if chunk_count <= 5 {
                        eprintln!(
                            "[DEBUG] parse_openai_stream: chunk #{} len={} total_bytes={}",
                            chunk_count,
                            chunk.len(),
                            total_bytes
                        );
                    }

                    let text = String::from_utf8_lossy(&chunk);
                    buffer.push_str(&text);

                    // 处理缓冲区中的完整行
                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].to_string();
                        buffer.drain(..=pos);

                        // 处理可能的 \r\n
                        let line = line.trim_end_matches('\r');

                        if !line.is_empty() && chunk_count <= 5 {
                            eprintln!(
                                "[DEBUG] parse_openai_stream: line len={} preview={:?}",
                                line.len(),
                                line.chars().take(100).collect::<String>()
                            );
                        }

                        if let Some(data) = sse::parse_sse_line(line) {
                            if sse::is_done_marker(&data) {
                                eprintln!(
                                    "[DEBUG] parse_openai_stream: [DONE] marker received, chunks={}, bytes={}",
                                    chunk_count, total_bytes
                                );
                                let _ = tx.send(Ok(StreamEvent::done())).await;
                                return;
                            }

                            // 解析 JSON 数据
                            match parse_openai_event(&data) {
                                Ok(Some(event)) => {
                                    if tx.send(Ok(event)).await.is_err() {
                                        eprintln!(
                                            "[DEBUG] parse_openai_stream: receiver dropped, exiting"
                                        );
                                        return;
                                    }
                                }
                                Ok(None) => continue,
                                Err(e) => {
                                    eprintln!("[DEBUG] parse_openai_stream: parse error: {:?}", e);
                                    let _ = tx.send(Err(e)).await;
                                    return;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[DEBUG] parse_openai_stream: chunk error: {}", e);
                    let _ = tx
                        .send(Err(KeyComputeError::ProviderError(e.to_string())))
                        .await;
                    return;
                }
            }
        }

        // 流结束
        eprintln!(
            "[DEBUG] parse_openai_stream: stream ended, chunks={}, bytes={}",
            chunk_count, total_bytes
        );
        let _ = tx.send(Ok(StreamEvent::done())).await;
    });

    Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}

/// 解析 OpenAI 流事件 JSON
fn parse_openai_event(data: &str) -> Result<Option<StreamEvent>> {
    let response: OpenAIStreamResponse = serde_json::from_str(data).map_err(|e| {
        KeyComputeError::ProviderError(format!("Failed to parse OpenAI stream event: {}", e))
    })?;

    // 检查是否有用量信息（通常在流结束时）
    if let Some(usage) = response.usage {
        return Ok(Some(StreamEvent::usage(
            usage.prompt_tokens as u32,
            usage.completion_tokens as u32,
        )));
    }

    // 处理选择结果
    if let Some(choice) = response.choices.first() {
        let delta = &choice.delta;

        // 检查是否有内容增量
        if let Some(content) = &delta.content {
            return Ok(Some(StreamEvent::Delta {
                content: content.clone(),
                finish_reason: choice.finish_reason.clone(),
            }));
        }

        // 检查是否有角色信息（通常是第一条消息）
        if delta.role.is_some() && delta.content.is_none() {
            // 角色消息，通常不包含内容，跳过
            return Ok(None);
        }

        // 检查是否结束
        if choice.finish_reason.is_some() {
            return Ok(Some(StreamEvent::Delta {
                content: String::new(),
                finish_reason: choice.finish_reason.clone(),
            }));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_openai_event_with_content() {
        let data = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "created": 1694268190,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "delta": {"content": "Hello"},
                "finish_reason": null
            }]
        }"#;

        let event = parse_openai_event(data).unwrap();
        assert!(matches!(event, Some(StreamEvent::Delta { content, .. }) if content == "Hello"));
    }

    #[test]
    fn test_parse_openai_event_with_usage() {
        let data = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "created": 1694268190,
            "model": "gpt-4o",
            "choices": [],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30
            }
        }"#;

        let event = parse_openai_event(data).unwrap();
        assert!(matches!(
            event,
            Some(StreamEvent::Usage {
                input_tokens: 10,
                output_tokens: 20
            })
        ));
    }

    #[test]
    fn test_parse_openai_event_finish() {
        let data = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "created": 1694268190,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        }"#;

        let event = parse_openai_event(data).unwrap();
        assert!(
            matches!(event, Some(StreamEvent::Delta { content, finish_reason: Some(reason) }) 
            if content.is_empty() && reason == "stop")
        );
    }
}
