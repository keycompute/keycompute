//! Ollama NDJSON 流解析
//!
//! 将 Ollama 的 JSON 行流解析为标准化的 StreamEvent
//!
//! Ollama 原生格式（/api/chat 端点）流式格式：每行一个完整 JSON 对象
//! ```json
//! {"model":"llama2","created_at":"...","message":{"role":"assistant","content":"Hello"},"done":false}
//! {"model":"llama2","created_at":"...","message":{"role":"assistant","content":" there"},"done":false}
//! {"model":"llama2","created_at":"...","done":true,"eval_count":5}
//! ```
//!
//! 注：Ollama 的 /v1/chat/completions 端点使用 SSE 格式，复用 keycompute-openai 的流解析。

use futures::{Stream, StreamExt};
use keycompute_provider_trait::ByteStream;
use keycompute_provider_trait::StreamEvent;
use keycompute_types::{KeyComputeError, Result};
use std::pin::Pin;
use tokio::sync::mpsc;

use crate::protocol::OllamaStreamResponse;

/// 解析 Ollama NDJSON 流
///
/// 将 HTTP 传输层的字节流转换为标准化的 StreamEvent 流
pub fn parse_ollama_stream(
    stream: ByteStream,
) -> Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>> {
    let (tx, rx) = mpsc::channel::<Result<StreamEvent>>(100);

    tokio::spawn(async move {
        let mut buffer = String::new();
        let mut stream = stream;

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    let text = String::from_utf8_lossy(&chunk);
                    buffer.push_str(&text);

                    // 处理缓冲区中的完整行（NDJSON 格式）
                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].to_string();
                        buffer.drain(..=pos);

                        // 去除可能的 \r
                        let line = line.trim_end_matches('\r');

                        // 跳过空行
                        if line.is_empty() {
                            continue;
                        }

                        // 解析 JSON 行
                        match parse_ollama_json_line(line) {
                            Ok(Some(event)) => {
                                if tx.send(Ok(event)).await.is_err() {
                                    return;
                                }
                            }
                            Ok(None) => continue,
                            Err(e) => {
                                let _ = tx.send(Err(e)).await;
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx
                        .send(Err(KeyComputeError::ProviderError(e.to_string())))
                        .await;
                    return;
                }
            }
        }

        // 流结束
        let _ = tx.send(Ok(StreamEvent::done())).await;
    });

    Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}

/// 解析单个 Ollama JSON 行
fn parse_ollama_json_line(line: &str) -> Result<Option<StreamEvent>> {
    // 尝试解析为流式响应
    let response: OllamaStreamResponse = serde_json::from_str(line).map_err(|e| {
        KeyComputeError::ProviderError(format!("Failed to parse Ollama stream event: {}", e))
    })?;

    // 检查是否完成
    if response.done {
        // 检查是否有用量信息
        if let (Some(prompt_eval), Some(eval)) = (response.prompt_eval_count, response.eval_count) {
            // 发送用量信息
            return Ok(Some(StreamEvent::usage(prompt_eval, eval)));
        }
        // 否则发送 Done 事件
        return Ok(Some(StreamEvent::done()));
    }

    // 提取增量内容
    if let Some(message) = response.message {
        let content = message.content;
        if !content.is_empty() {
            return Ok(Some(StreamEvent::delta(content)));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    #[test]
    fn test_parse_ollama_json_line_content() {
        let line = r#"{"model":"llama2","created_at":"2023-08-04T08:52:19.385406455Z","message":{"role":"assistant","content":"Hello"},"done":false}"#;
        let event = parse_ollama_json_line(line).unwrap();

        assert!(matches!(event, Some(StreamEvent::Delta { content, .. }) if content == "Hello"));
    }

    #[test]
    fn test_parse_ollama_json_line_done() {
        let line =
            r#"{"model":"llama2","created_at":"2023-08-04T08:52:19.385406455Z","done":true}"#;
        let event = parse_ollama_json_line(line).unwrap();

        assert!(matches!(event, Some(StreamEvent::Done)));
    }

    #[test]
    fn test_parse_ollama_json_line_usage() {
        let line = r#"{"model":"llama2","created_at":"2023-08-04T08:52:19.385406455Z","done":true,"prompt_eval_count":10,"eval_count":5}"#;
        let event = parse_ollama_json_line(line).unwrap();

        assert!(matches!(
            event,
            Some(StreamEvent::Usage {
                input_tokens: 10,
                output_tokens: 5
            })
        ));
    }

    #[tokio::test]
    async fn test_parse_ollama_stream() {
        // 模拟 NDJSON 数据
        let ndjson_data = vec![
            Ok(bytes::Bytes::from(
                r#"{"model":"llama2","created_at":"...","message":{"role":"assistant","content":"Hello"},"done":false}
"#,
            )),
            Ok(bytes::Bytes::from(
                r#"{"model":"llama2","created_at":"...","message":{"role":"assistant","content":" World"},"done":false}
"#,
            )),
            Ok(bytes::Bytes::from(
                r#"{"model":"llama2","created_at":"...","done":true,"prompt_eval_count":10,"eval_count":2}
"#,
            )),
        ];

        let byte_stream: ByteStream = Box::pin(stream::iter(ndjson_data));
        let mut stream = parse_ollama_stream(byte_stream);

        // 收集所有事件
        let mut events = Vec::new();
        while let Some(result) = stream.next().await {
            if let Ok(event) = result {
                events.push(event);
            }
        }

        // 验证事件序列
        assert!(events.len() >= 3); // delta + delta + usage + done
        assert!(matches!(&events[0], StreamEvent::Delta { content, .. } if content == "Hello"));
        assert!(matches!(&events[1], StreamEvent::Delta { content, .. } if content == " World"));
        // 最后应该是 usage 或 done
        let last = events.last().unwrap();
        assert!(matches!(
            last,
            StreamEvent::Usage { .. } | StreamEvent::Done
        ));
    }

    #[test]
    fn test_parse_empty_line() {
        let result = parse_ollama_json_line("");
        // 空行会导致 JSON 解析错误
        assert!(result.is_err() || result.unwrap().is_none());
    }
}
