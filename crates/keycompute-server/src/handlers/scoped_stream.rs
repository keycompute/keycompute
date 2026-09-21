//! Native protocol streaming for the scoped node and passthrough routes.
//!
//! The response body is deliberately owned by a detached task.  This keeps the
//! node task, provider receiver, admission permits, and settlement guards alive
//! when Axum drops the request future after a client disconnect.

use super::{
    ClientResponseGuard, GenerationBalanceReservation, GenerationTpmReservation,
    finalize_immediate_settlement_logged, record_final_client_first_content,
};
use crate::{
    error::{ApiError, Result},
    state::{AppState, GenerationHttpBodyPermit},
};
use axum::{body::Body, http::StatusCode, response::Response};
use bytes::Bytes;
use keycompute_db::models::NodeTask;
use keycompute_types::{
    ClientResponseOutcome, ExecutionPlan, ModelAccessMode, RequestContext,
    RequestLifecycleRecorder,
    node::{NodeNativeStreamEvent, NodeTaskPayload},
    node_native::{NodeNativeHttpResult, NodeNativeOperation as Op, NodeNativeRequest},
    node_stream::{NativeStreamTerminalOutcome, NodeNativeStreamSummary},
};
use llm_protocol_provider::{NativeStreamEvent, StreamEvent};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

const BODY_CHANNEL_CAPACITY: usize = 1;
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
const NODE_COMPLETION_GRACE: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) struct Prepared {
    pub state: AppState,
    pub mode: ModelAccessMode,
    pub op: Op,
    pub model: String,
    pub ctx: Arc<RequestContext>,
    pub plan: ExecutionPlan,
    pub native: Option<NodeNativeRequest>,
    pub billing_provider: String,
    pub account_id: Uuid,
    pub lifecycle: Arc<dyn RequestLifecycleRecorder>,
    pub body_permit: Option<GenerationHttpBodyPermit>,
    pub balance: GenerationBalanceReservation,
    pub tpm: GenerationTpmReservation,
    pub guard: ClientResponseGuard,
    pub managed: Option<Arc<crate::scoped_state::execution::ManagedExecution>>,
}

struct Head {
    status: u16,
    headers: Vec<(String, String)>,
    initial_body: Option<Bytes>,
}

type Chunk = std::result::Result<Bytes, std::io::Error>;

pub(crate) async fn serve(mut input: Prepared) -> Result<Response> {
    let mut delivery_guard = ClientResponseGuard::new(input.lifecycle.clone(), input.ctx.clone());
    input.guard.disarm();
    let body_permit = input.body_permit.take().map(Arc::new);
    let executor_permit = body_permit.clone();
    let timeout = if input.mode == ModelAccessMode::NodeDispatch {
        input
            .state
            .node_gateway
            .as_ref()
            .map(|g| g.task_deadline())
            .unwrap_or(Duration::from_secs(1))
    } else {
        Duration::from_secs(input.state.gateway_config.timeout_secs.max(1))
    };
    let execution_deadline = tokio::time::Instant::now() + timeout;
    let (outcome_tx, outcome_rx) = oneshot::channel();
    let (head_tx, head_rx) = oneshot::channel();
    let (body_tx, mut body_rx) = mpsc::channel(BODY_CHANNEL_CAPACITY);
    tokio::spawn(async move {
        let owner = Owner {
            input,
            head_tx: Some(head_tx),
            body_tx,
            settled: false,
            task_id: None,
            outcome_tx: Some(outcome_tx),
            _body_permit: executor_permit,
            execution_deadline,
            stream_started: false,
        };
        owner.run().await;
    });

    let head = match head_rx.await {
        Ok(head) => head,
        Err(_) => {
            delivery_guard
                .finish_with_outcome(ClientResponseOutcome::ResponseFailed)
                .await;
            return Err(ApiError::Internal(
                "native stream stopped before sending an HTTP head".into(),
            ));
        }
    };
    let streaming = head.initial_body.is_none();
    let initial = head.initial_body;
    let stream = async_stream::stream! {
        let _body_permit=body_permit;
        if let Some(body)=initial {yield Ok::<Bytes,std::io::Error>(body);}
        else {while let Some(chunk)=body_rx.recv().await {yield chunk;}}
        let outcome=outcome_rx.await.unwrap_or(ClientResponseOutcome::ResponseFailed);
        delivery_guard.finish_with_outcome(outcome).await;
        if streaming && outcome!=ClientResponseOutcome::Succeeded {
            yield Err(std::io::Error::other("incomplete native event stream"));
        }
    };
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = StatusCode::from_u16(head.status)
        .map_err(|_| ApiError::Internal("native stream returned an invalid HTTP status".into()))?;
    for (name, value) in head.headers {
        let normalized_name = name.to_ascii_lowercase();
        let allowed =
            keycompute_types::node_native::native_response_header_allowed(&normalized_name, &value)
                || (head.status == 200
                    && normalized_name == "content-type"
                    && value
                        .split(';')
                        .next()
                        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
                    && value.len() <= 256
                    && value.bytes().all(|b| (32..127).contains(&b)));
        if allowed
            && let (Ok(name), Ok(value)) = (
                axum::http::HeaderName::from_bytes(normalized_name.as_bytes()),
                axum::http::HeaderValue::from_str(&value),
            )
        {
            response.headers_mut().insert(name, value);
        }
    }
    Ok(response)
}

struct Owner {
    input: Prepared,
    head_tx: Option<oneshot::Sender<Head>>,
    body_tx: mpsc::Sender<Chunk>,
    settled: bool,
    task_id: Option<Uuid>,
    outcome_tx: Option<oneshot::Sender<ClientResponseOutcome>>,
    _body_permit: Option<Arc<GenerationHttpBodyPermit>>,
    execution_deadline: tokio::time::Instant,
    stream_started: bool,
}

impl Owner {
    async fn run(mut self) {
        // Keep the decoded request resident until the detached executor has
        // either settled or released it.
        let _body_permit = self.input.body_permit.take();
        self.input.ctx.set_input_tokens_estimate(
            llm_gateway::GatewayExecutor::estimate_context_input_tokens(&self.input.ctx),
        );
        if self.input.mode == ModelAccessMode::NodeDispatch {
            self.run_node().await;
        } else {
            self.run_passthrough().await;
        }
        if !self.settled {
            self.finish_node(false, "error", ClientResponseOutcome::ResponseFailed)
                .await;
        }
    }

    async fn run_node(&mut self) {
        let Some(gateway) = self.input.state.node_gateway.clone() else {
            self.prehead_failure("node_gateway_unavailable", 503).await;
            return;
        };
        let task = match gateway
            .enqueue_native_stream(
                self.input.ctx.tenant_id,
                self.input.ctx.user_id,
                self.input.model.clone(),
                NodeTaskPayload {
                    request_id: self.input.ctx.request_id,
                    native: self.input.native.clone(),
                    chat: None,
                    image_generation: None,
                    image_edit: None,
                },
            )
            .await
        {
            Ok(task) => task,
            Err(error) => {
                tracing::warn!(request_id=%self.input.ctx.request_id,%error,"native task enqueue failed");
                self.prehead_failure("Native task queue is temporarily unavailable", 503)
                    .await;
                return;
            }
        };

        self.task_id = Some(task.id);
        let deadline = self.execution_deadline;
        let mut after = None;
        let mut observed = false;
        let mut first_content = false;
        let mut terminal_summary = None;
        let mut terminal_failed = false;

        loop {
            if tokio::time::Instant::now() >= deadline {
                // An unclaimed task has never produced a response head. Keep
                // its deadline error explicit instead of dropping the oneshot
                // and exposing a generic internal-error response.
                self.send_error_head("Native stream execution timed out", 504)
                    .await;
                self.cancel_node(&gateway, task.id, "native_stream_deadline")
                    .await;
                self.finish_node(observed, "incomplete", ClientResponseOutcome::TimedOut)
                    .await;
                return;
            }

            if self.head_tx.is_some()
                && let Some(result) = self.native_error_result(&gateway, task.id).await
            {
                match result {
                    Ok(response) => {
                        let initial_body = match serde_json::to_vec(&response.body) {
                            Ok(body) => Bytes::from(body),
                            Err(_) => {
                                self.prehead_failure("invalid native error body", 502).await;
                                return;
                            }
                        };
                        self.send_head(Head {
                            status: response.status,
                            headers: response.headers,
                            initial_body: Some(initial_body),
                        })
                        .await;
                        self.finish_node(false, "error", ClientResponseOutcome::ResponseFailed)
                            .await;
                    }
                    Err(message) => self.prehead_failure(&message, 502).await,
                }
                return;
            }

            let events = match tokio::time::timeout(
                Duration::from_secs(3),
                gateway.store.read_native_stream_events(task.id, after),
            )
            .await
            .unwrap_or_else(|_| {
                Err(keycompute_db::DbError::Other(
                    "native_stream_read_timeout".into(),
                ))
            }) {
                Ok(events) => events,
                Err(error) => {
                    if self.head_tx.is_none() {
                        let _ = self
                            .send_chunk(Err(std::io::Error::other("native stream storage failed")))
                            .await;
                    } else {
                        self.send_error_head("native stream storage failed", 503)
                            .await;
                    }
                    self.cancel_node(&gateway, task.id, "native_stream_storage_failed")
                        .await;
                    self.finish_node(observed, "error", ClientResponseOutcome::ResponseFailed)
                        .await;
                    tracing::warn!(task_id=%task.id, %error, "native stream event read failed");
                    return;
                }
            };

            if events.is_empty() {
                if let Some(result) = self.native_error_result(&gateway, task.id).await {
                    if self.head_tx.is_some() {
                        match result {
                            Ok(response) => {
                                let initial_body = serde_json::to_vec(&response.body)
                                    .map(Bytes::from)
                                    .unwrap_or_else(|_| Bytes::from_static(b"{}"));
                                self.send_head(Head {
                                    status: response.status,
                                    headers: response.headers,
                                    initial_body: Some(initial_body),
                                })
                                .await;
                            }
                            Err(message) => {
                                self.prehead_failure(&message, 502).await;
                                return;
                            }
                        }
                        self.finish_node(false, "error", ClientResponseOutcome::ResponseFailed)
                            .await;
                        return;
                    }
                    // Completion can fail after Start without a separate Failed
                    // event (delivery rejection, revoked profile, interrupted
                    // transport). Stop the HTTP stream on that durable failure,
                    // not only at its original task deadline.
                    let _ = self
                        .send_chunk(Err(std::io::Error::other(
                            "native task terminated before a stream terminal",
                        )))
                        .await;
                    self.cancel_node(&gateway, task.id, "native_stream_task_failed")
                        .await;
                    self.finish_node(observed, "error", ClientResponseOutcome::ResponseFailed)
                        .await;
                    return;
                }
                if self.head_tx.is_none() && self.body_tx.is_closed() {
                    self.cancel_node(&gateway, task.id, "client_disconnected")
                        .await;
                    self.finish_node(
                        observed,
                        "incomplete",
                        ClientResponseOutcome::ClientDisconnected,
                    )
                    .await;
                    return;
                }
                tokio::select! {
                    _ = self.input.ctx.wait_for_client_disconnect() => {
                        self.cancel_node(&gateway, task.id, "client_disconnected").await;
                        self.finish_node(observed, "incomplete", ClientResponseOutcome::ClientDisconnected).await;
                        return;
                    }
                    _ = async {
                        if let Some(head_tx) = &mut self.head_tx { let _ = head_tx.closed().await; }
                    }, if self.head_tx.is_some() => {
                        self.cancel_node(&gateway, task.id, "client_disconnected").await;
                        self.input.ctx.mark_client_disconnected();
                        self.finish_node(observed, "incomplete", ClientResponseOutcome::ClientDisconnected).await;
                        return;
                    }
                    _ = tokio::time::sleep(POLL_INTERVAL) => {}
                }
                continue;
            }

            for stored in events {
                after = Some(stored.seq);
                match stored.event {
                    NodeNativeStreamEvent::Start {
                        status,
                        headers,
                        body,
                    } => {
                        if status != 200 || body.is_some() || stored.seq != 0 {
                            self.send_error_head("invalid native SSE start", 502).await;
                            self.cancel_node(&gateway, task.id, "invalid_native_stream_start")
                                .await;
                            self.finish_node(
                                observed,
                                "error",
                                ClientResponseOutcome::ResponseFailed,
                            )
                            .await;
                            return;
                        }
                        self.input.ctx.mark_upstream_response_accepted();
                        self.send_head(Head {
                            status,
                            headers: with_sse_content_type(headers),
                            initial_body: None,
                        })
                        .await;
                        observed = true;
                        if !self
                            .ack_event(&gateway, task.id, stored.lease_id, stored.seq)
                            .await
                        {
                            self.cancel_node(&gateway, task.id, "native_stream_ack_failed")
                                .await;
                            self.finish_node(
                                observed,
                                "error",
                                ClientResponseOutcome::ResponseFailed,
                            )
                            .await;
                            return;
                        }
                    }
                    NodeNativeStreamEvent::Data { frame } => {
                        if self.head_tx.is_some() {
                            self.send_error_head("native SSE data preceded start", 502)
                                .await;
                            self.cancel_node(&gateway, task.id, "native_stream_start_missing")
                                .await;
                            self.finish_node(false, "error", ClientResponseOutcome::ResponseFailed)
                                .await;
                            return;
                        }
                        if !self.send_chunk(Ok(Bytes::from(frame))).await {
                            self.cancel_node(&gateway, task.id, "client_disconnected")
                                .await;
                            self.input.ctx.mark_client_disconnected();
                            self.finish_node(
                                observed,
                                "incomplete",
                                ClientResponseOutcome::ClientDisconnected,
                            )
                            .await;
                            return;
                        }
                        observed = true;
                        if !first_content {
                            first_content = true;
                            let _ = record_final_client_first_content(
                                &self.input.lifecycle,
                                self.input.ctx.request_id,
                            )
                            .await;
                        }
                        if !self
                            .ack_event(&gateway, task.id, stored.lease_id, stored.seq)
                            .await
                        {
                            self.cancel_node(&gateway, task.id, "native_stream_ack_failed")
                                .await;
                            self.finish_node(
                                observed,
                                "error",
                                ClientResponseOutcome::ResponseFailed,
                            )
                            .await;
                            return;
                        }
                    }
                    NodeNativeStreamEvent::Terminal { summary } => {
                        terminal_failed =
                            summary.terminal_outcome == Some(NativeStreamTerminalOutcome::Failed);
                        terminal_summary = Some(summary);
                        let _ = self
                            .ack_event(&gateway, task.id, stored.lease_id, stored.seq)
                            .await;
                        break;
                    }
                    NodeNativeStreamEvent::Failed { usage, .. } => {
                        if let Some(usage) = usage {
                            if usage.input_exact {
                                self.input.ctx.set_input_tokens(usage.input_tokens);
                            }
                            if usage.output_exact {
                                self.input.ctx.set_output_tokens(usage.output_tokens);
                            } else {
                                self.input
                                    .ctx
                                    .set_output_tokens_estimate(usage.output_tokens);
                            }
                        }
                        terminal_failed = true;
                        terminal_summary = gateway
                            .store
                            .native_stream_summary(task.id)
                            .await
                            .ok()
                            .flatten();
                        let _ = self
                            .ack_event(&gateway, task.id, stored.lease_id, stored.seq)
                            .await;
                        break;
                    }
                }
            }

            if terminal_failed && terminal_summary.is_none() {
                self.wait_for_task_terminal(&gateway, task.id).await;
                if self.head_tx.is_some() {
                    self.send_head(Head {
                        status: 502,
                        headers: vec![("content-type".into(), "application/json".into())],
                        initial_body: Some(Bytes::from_static(
                            br#"{"error":{"message":"native stream failed"}}"#,
                        )),
                    })
                    .await;
                } else {
                    let _ = self
                        .send_chunk(Err(std::io::Error::other("native stream failed")))
                        .await;
                }
                self.finish_node(observed, "error", ClientResponseOutcome::ResponseFailed)
                    .await;
                return;
            }

            if let Some(summary) = terminal_summary.take() {
                self.apply_summary(&summary);
                let committed = self.wait_for_task_terminal(&gateway, task.id).await;
                let durable_success = committed.as_ref().is_some_and(|task| {
                    task.status == "succeeded"
                        && task.result_json.as_ref() == serde_json::to_value(&summary).ok().as_ref()
                });
                if !durable_success && !terminal_failed {
                    self.cancel_node(&gateway, task.id, "native_stream_completion_unconfirmed")
                        .await;
                }
                let failed = terminal_failed
                    || !durable_success
                    || summary_outcome(&summary) == NativeStreamTerminalOutcome::Failed;
                let outcome = if !failed {
                    ClientResponseOutcome::Succeeded
                } else {
                    ClientResponseOutcome::ResponseFailed
                };
                if failed && self.head_tx.is_some() {
                    self.send_head(Head {
                        status: 502,
                        headers: vec![("content-type".into(), "application/json".into())],
                        initial_body: Some(Bytes::from_static(
                            br#"{"error":{"message":"native stream failed"}}"#,
                        )),
                    })
                    .await;
                } else if failed {
                    let _ = self
                        .send_chunk(Err(std::io::Error::other("native stream failed")))
                        .await;
                }
                let status = if failed {
                    "error"
                } else {
                    match summary_outcome(&summary) {
                        NativeStreamTerminalOutcome::Incomplete => "incomplete",
                        _ => "success",
                    }
                };
                self.finish_node(observed, status, outcome).await;
                return;
            }

            if self.input.ctx.is_client_disconnected() {
                self.cancel_node(&gateway, task.id, "client_disconnected")
                    .await;
                self.finish_node(
                    observed,
                    "incomplete",
                    ClientResponseOutcome::ClientDisconnected,
                )
                .await;
                return;
            }
        }
    }

    async fn run_passthrough(&mut self) {
        let receiver = match tokio::time::timeout(
            Duration::from_secs(self.input.state.gateway_config.timeout_secs),
            self.input.state.gateway.execute_with_recorder(
                self.input.ctx.clone(),
                self.input.plan.clone(),
                self.input.state.account_states.clone(),
                Some(self.input.state.provider_health.clone()),
                self.input.lifecycle.clone(),
            ),
        )
        .await
        {
            Ok(Ok(receiver)) => receiver,
            Ok(Err(error)) => {
                self.prehead_provider_error(error.to_string()).await;
                return;
            }
            Err(_) => {
                self.prehead_failure("native upstream setup timed out", 504)
                    .await;
                return;
            }
        };
        self.run_provider_events(receiver).await;
    }

    async fn run_provider_events(&mut self, mut receiver: mpsc::Receiver<StreamEvent>) {
        let mut observed = false;
        let mut first_content = false;
        let mut terminal = None;
        loop {
            let event = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(self.execution_deadline) => {
                    self.drain_provider_after_cancel(&mut receiver,ClientResponseOutcome::TimedOut).await;
                    self.prehead_failure("native stream deadline exceeded",504).await;
                    self.finish_node(observed,"incomplete",ClientResponseOutcome::TimedOut).await;
                    return;
                }
                _ = self.input.ctx.wait_for_client_disconnect() => {
                    self.drain_provider_after_cancel(&mut receiver,ClientResponseOutcome::ClientDisconnected).await;
                    self.finish_node(observed, "incomplete", ClientResponseOutcome::ClientDisconnected).await;
                    return;
                }
                _ = async {
                    if let Some(head_tx) = &mut self.head_tx { let _ = head_tx.closed().await; }
                }, if self.head_tx.is_some() => {
                    self.input.ctx.mark_client_disconnected();
                    self.drain_provider_after_cancel(&mut receiver,ClientResponseOutcome::ClientDisconnected).await;
                    self.finish_node(observed, "incomplete", ClientResponseOutcome::ClientDisconnected).await;
                    return;
                }
                _ = self.body_tx.closed(), if self.head_tx.is_none() => {
                    self.input.ctx.mark_client_disconnected();
                    self.drain_provider_after_cancel(&mut receiver,ClientResponseOutcome::ClientDisconnected).await;
                    self.finish_node(observed, "incomplete", ClientResponseOutcome::ClientDisconnected).await;
                    return;
                }
                event = receiver.recv() => event,
            };
            let Some(event) = event else {
                let outcome = terminal.unwrap_or(NativeStreamTerminalOutcome::Incomplete);
                if self.head_tx.is_none() && outcome == NativeStreamTerminalOutcome::Failed {
                    let _ = self
                        .send_chunk(Err(std::io::Error::other("native stream failed")))
                        .await;
                } else if self.head_tx.is_some() {
                    self.send_error_head("native upstream ended before an SSE frame", 502)
                        .await;
                }
                self.finish_node(
                    observed,
                    if outcome == NativeStreamTerminalOutcome::Complete {
                        "success"
                    } else {
                        "incomplete"
                    },
                    if outcome == NativeStreamTerminalOutcome::Complete {
                        ClientResponseOutcome::Succeeded
                    } else {
                        ClientResponseOutcome::ResponseFailed
                    },
                )
                .await;
                return;
            };
            match event {
                // Normalized deltas accompany the native event for common accounting.
                // The native envelope, not this text projection, is delivered to callers.
                StreamEvent::Delta { .. } => {}
                StreamEvent::InputUsage { input_tokens } => {
                    self.input.ctx.set_input_tokens(input_tokens);
                }
                StreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                } => {
                    self.input.ctx.set_input_tokens(input_tokens);
                    self.input.ctx.set_output_tokens(output_tokens);
                }
                StreamEvent::Raw { data, admission } if self.input.op == Op::Messages => {
                    let _admission = admission;
                    let Some(frame) = anthropic_envelope_to_sse(&data) else {
                        self.drain_provider_after_cancel(
                            &mut receiver,
                            ClientResponseOutcome::ResponseFailed,
                        )
                        .await;
                        self.provider_failure("invalid Anthropic native SSE envelope", observed)
                            .await;
                        return;
                    };
                    if !self
                        .send_provider_frame(frame, &mut observed, &mut first_content)
                        .await
                    {
                        self.drain_provider_after_cancel(
                            &mut receiver,
                            ClientResponseOutcome::ClientDisconnected,
                        )
                        .await;
                        self.finish_node(
                            observed,
                            "incomplete",
                            ClientResponseOutcome::ClientDisconnected,
                        )
                        .await;
                        return;
                    }
                    if anthropic_envelope_is_error(&data) {
                        terminal = Some(NativeStreamTerminalOutcome::Failed);
                    }
                }
                StreamEvent::Native {
                    event:
                        NativeStreamEvent::OpenAiResponsesSse {
                            event,
                            data,
                            admission,
                        },
                } if self.input.op == Op::Responses => {
                    let _admission = admission;
                    let failed = data.get("type").and_then(Value::as_str)
                        == Some("response.failed")
                        || event == "response.failed";
                    let incomplete = data.get("type").and_then(Value::as_str)
                        == Some("response.incomplete")
                        || event == "response.incomplete";
                    let Some(frame) = responses_event_to_sse(&event, &data) else {
                        self.drain_provider_after_cancel(
                            &mut receiver,
                            ClientResponseOutcome::ResponseFailed,
                        )
                        .await;
                        self.provider_failure("native SSE event exceeds the frame limit", observed)
                            .await;
                        return;
                    };
                    if !self
                        .send_provider_frame(frame, &mut observed, &mut first_content)
                        .await
                    {
                        self.drain_provider_after_cancel(
                            &mut receiver,
                            ClientResponseOutcome::ClientDisconnected,
                        )
                        .await;
                        self.finish_node(
                            observed,
                            "incomplete",
                            ClientResponseOutcome::ClientDisconnected,
                        )
                        .await;
                        return;
                    }
                    if failed {
                        terminal = Some(NativeStreamTerminalOutcome::Failed);
                    } else if incomplete {
                        terminal = Some(NativeStreamTerminalOutcome::Incomplete);
                    }
                }
                StreamEvent::Native {
                    event:
                        NativeStreamEvent::OpenAiResponsesHttpError {
                            status,
                            headers,
                            body,
                        },
                } if self.head_tx.is_some() => {
                    self.send_head(Head {
                        status,
                        headers,
                        initial_body: Some(Bytes::from(body)),
                    })
                    .await;
                    self.finish_node(false, "error", ClientResponseOutcome::ResponseFailed)
                        .await;
                    return;
                }
                StreamEvent::Done => {
                    let outcome = terminal.unwrap_or(NativeStreamTerminalOutcome::Complete);
                    if outcome == NativeStreamTerminalOutcome::Failed && self.head_tx.is_some() {
                        self.send_error_head("native Responses stream failed before a frame", 502)
                            .await;
                    } else if outcome == NativeStreamTerminalOutcome::Failed {
                        let _ = self
                            .send_chunk(Err(std::io::Error::other("native stream failed")))
                            .await;
                    }
                    self.finish_node(
                        observed,
                        match outcome {
                            NativeStreamTerminalOutcome::Complete => "success",
                            NativeStreamTerminalOutcome::Incomplete => "incomplete",
                            NativeStreamTerminalOutcome::Failed => "error",
                        },
                        if outcome != NativeStreamTerminalOutcome::Failed {
                            ClientResponseOutcome::Succeeded
                        } else {
                            ClientResponseOutcome::ResponseFailed
                        },
                    )
                    .await;
                    return;
                }
                StreamEvent::Error { message } => {
                    self.drain_provider_after_cancel(
                        &mut receiver,
                        ClientResponseOutcome::ResponseFailed,
                    )
                    .await;
                    if self.head_tx.is_some() {
                        self.prehead_provider_error(message).await;
                    } else {
                        let _ = self
                            .send_chunk(Err(std::io::Error::other("native upstream stream failed")))
                            .await;
                        self.finish_node(observed, "error", ClientResponseOutcome::ResponseFailed)
                            .await;
                    }
                    return;
                }
                _ => {
                    self.drain_provider_after_cancel(
                        &mut receiver,
                        ClientResponseOutcome::ResponseFailed,
                    )
                    .await;
                    self.provider_failure("unexpected native provider event", observed)
                        .await;
                    return;
                }
            }
        }
    }

    async fn drain_provider_after_cancel(
        &self,
        receiver: &mut mpsc::Receiver<StreamEvent>,
        outcome: ClientResponseOutcome,
    ) {
        self.input.ctx.cancel_upstream(outcome);
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(event) = receiver.recv().await {
                match event {
                    StreamEvent::InputUsage { input_tokens } => {
                        self.input.ctx.set_input_tokens(input_tokens)
                    }
                    StreamEvent::Usage {
                        input_tokens,
                        output_tokens,
                    } => {
                        self.input.ctx.set_input_tokens(input_tokens);
                        self.input.ctx.set_output_tokens(output_tokens);
                    }
                    _ => {}
                }
            }
        })
        .await;
    }

    async fn send_provider_frame(
        &mut self,
        frame: Bytes,
        observed: &mut bool,
        first_content: &mut bool,
    ) -> bool {
        if self.head_tx.is_some() {
            self.input.ctx.mark_upstream_response_accepted();
            self.send_head(Head {
                status: 200,
                headers: with_sse_content_type(Vec::new()),
                initial_body: None,
            })
            .await;
        }
        if !self.send_chunk(Ok(frame)).await {
            self.input.ctx.mark_client_disconnected();
            return false;
        }
        *observed = true;
        if !*first_content {
            *first_content = true;
            let _ =
                record_final_client_first_content(&self.input.lifecycle, self.input.ctx.request_id)
                    .await;
        }
        true
    }

    async fn provider_failure(&mut self, message: &str, observed: bool) {
        if self.head_tx.is_some() {
            self.prehead_provider_error(message.to_owned()).await;
        } else {
            let _ = self
                .send_chunk(Err(std::io::Error::other(message.to_owned())))
                .await;
            self.finish_node(observed, "error", ClientResponseOutcome::ResponseFailed)
                .await;
        }
    }

    async fn prehead_provider_error(&mut self, message: String) {
        if let Some(response) = self.input.ctx.client_upstream_response() {
            self.send_head(Head {
                status: response.status,
                headers: response.headers,
                initial_body: Some(Bytes::from(response.body)),
            })
            .await;
        } else {
            self.send_error_head(&message, 502).await;
        }
    }

    async fn prehead_failure(&mut self, message: &str, status: u16) {
        self.send_error_head(message, status).await;
    }

    async fn send_error_head(&mut self, message: &str, status: u16) {
        if self.head_tx.is_some() {
            self.send_head(Head {
                status,
                headers: vec![("content-type".into(), "application/json".into())],
                initial_body: Some(Bytes::from(
                    serde_json::to_vec(&json!({"error":{"message":message}}))
                        .unwrap_or_else(|_| b"{}".to_vec()),
                )),
            })
            .await;
        }
    }

    async fn send_head(&mut self, head: Head) {
        if let Some(managed) = &self.input.managed
            && let Some(body) = &head.initial_body
            && let Ok(value) = serde_json::from_slice::<Value>(body)
        {
            // Preserve definite pre-stream HTTP failures in the resource record;
            // the detached owner still settles before marking it terminal.
            if managed
                .capture_http(&self.input.ctx, head.status, &head.headers, &value)
                .await
                .is_err()
            {
                self.input
                    .ctx
                    .cancel_upstream(ClientResponseOutcome::ResponseFailed);
            }
        }
        self.stream_started |= head.status == 200 && head.initial_body.is_none();
        if let Some(sender) = self.head_tx.take()
            && sender.send(head).is_err()
        {
            self.input.ctx.mark_client_disconnected();
        }
    }

    async fn send_chunk(&self, chunk: Chunk) -> bool {
        let chunk = if let (Some(managed), Ok(bytes)) = (&self.input.managed, &chunk) {
            let Ok(raw) = std::str::from_utf8(bytes) else {
                self.input
                    .ctx
                    .cancel_upstream(ClientResponseOutcome::ResponseFailed);
                return false;
            };
            match managed.frame(&self.input.ctx, raw).await {
                Ok(Some(frame)) => Ok(Bytes::from(frame)),
                Ok(None) => return true, // Terminal is emitted after durable settlement/state commit.
                Err(_) => {
                    self.input
                        .ctx
                        .cancel_upstream(ClientResponseOutcome::ResponseFailed);
                    return false;
                }
            }
        } else {
            chunk
        };
        self.send_raw_chunk(chunk).await
    }

    async fn send_raw_chunk(&self, chunk: Chunk) -> bool {
        tokio::time::timeout_at(
            self.execution_deadline
                .min(tokio::time::Instant::now() + SEND_TIMEOUT),
            self.body_tx.send(chunk),
        )
        .await
        .is_ok_and(|result| result.is_ok())
    }

    async fn ack_event(
        &self,
        gateway: &node_gateway::NodeGatewayService,
        task_id: Uuid,
        lease_id: Uuid,
        seq: u64,
    ) -> bool {
        tokio::time::timeout(
            SEND_TIMEOUT,
            gateway
                .store
                .acknowledge_native_stream_event(task_id, lease_id, seq),
        )
        .await
        .is_ok_and(|result| result.is_ok())
    }

    async fn native_error_result(
        &self,
        gateway: &node_gateway::NodeGatewayService,
        task_id: Uuid,
    ) -> Option<std::result::Result<NodeNativeHttpResult, String>> {
        let task = tokio::time::timeout(
            Duration::from_secs(2),
            NodeTask::find_by_id(gateway.store.pool().write_conn(), task_id),
        )
        .await
        .ok()?
        .ok()??;
        if !task.is_terminal() {
            return None;
        }
        let Some(value) = task.result_json else {
            return matches!(task.status.as_str(), "failed" | "expired")
                .then(|| Err("native task completed without a result".into()));
        };
        if value.get("body").is_none() {
            return matches!(task.status.as_str(), "failed" | "expired")
                .then(|| Err("stream task completed without an HTTP result".into()));
        }
        let response: NodeNativeHttpResult = match serde_json::from_value(value) {
            Ok(response) => response,
            Err(_) => return Some(Err("invalid native HTTP result".into())),
        };
        match response.validate_for(self.input.op, &self.input.model) {
            Ok(None) if (400..=599).contains(&response.status) => Some(Ok(response)),
            Ok(_) => Some(Err(
                "stream task returned an unexpected native result".into()
            )),
            Err(error) => Some(Err(format!("invalid native HTTP result: {error}"))),
        }
    }

    async fn wait_for_task_terminal(
        &self,
        gateway: &node_gateway::NodeGatewayService,
        task_id: Uuid,
    ) -> Option<NodeTask> {
        let deadline = tokio::time::Instant::now() + NODE_COMPLETION_GRACE;
        while tokio::time::Instant::now() < deadline {
            if let Ok(Ok(Some(task))) = tokio::time::timeout_at(
                deadline,
                NodeTask::find_by_id(gateway.store.pool().write_conn(), task_id),
            )
            .await
                && task.is_terminal()
            {
                return Some(task);
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        tracing::warn!(%task_id,"native stream completion was not durably confirmed");
        None
    }

    async fn cancel_node(
        &self,
        gateway: &node_gateway::NodeGatewayService,
        task_id: Uuid,
        reason: &str,
    ) {
        if !tokio::time::timeout(
            Duration::from_secs(3),
            gateway.cancel_native_stream(task_id, reason),
        )
        .await
        .is_ok_and(|r| r.is_ok())
        {
            tracing::warn!(%task_id,"native stream cancellation awaits deadline reconciliation");
        }
    }

    fn apply_summary(&self, summary: &NodeNativeStreamSummary) {
        if let Some(usage) = &summary.usage {
            if usage.input_exact || usage.complete {
                self.input.ctx.set_input_tokens(usage.input_tokens);
            } else if usage.input_tokens > 0 {
                self.input.ctx.set_input_tokens_estimate(usage.input_tokens);
            }
            if usage.output_exact || usage.complete {
                self.input.ctx.set_output_tokens(usage.output_tokens);
            } else {
                self.input
                    .ctx
                    .set_output_tokens_estimate(usage.output_tokens);
            }
        }
    }

    async fn finish_node(
        &mut self,
        observed: bool,
        status: &str,
        mut outcome: ClientResponseOutcome,
    ) {
        if std::mem::replace(&mut self.settled, true) {
            return;
        }
        // Event accounting is independent of whether the HTTP consumer read it.
        // Cancellation serializes with event persistence before this final read.
        if let (Some(task_id), Some(gateway)) =
            (self.task_id, self.input.state.node_gateway.as_ref())
            && let Ok(Ok(Some(summary))) = tokio::time::timeout(
                Duration::from_secs(3),
                gateway.store.native_stream_summary(task_id),
            )
            .await
        {
            self.input.ctx.mark_upstream_response_accepted();
            self.apply_summary(&summary);
        }
        let observed = observed || self.input.ctx.is_upstream_response_accepted();
        let mut durable = true;
        if let Some(managed) = &self.input.managed {
            durable = managed.checkpoint(&self.input.ctx).await.is_ok();
        }
        if observed {
            self.input.balance.transfer_to_settlement();
            self.input.tpm.transfer_to_settlement();
            let settlement = super::ImmediateSettlementServices::from_state(&self.input.state);
            let secured = finalize_immediate_settlement_logged(
                &settlement,
                &self.input.ctx,
                &self.input.billing_provider,
                self.input.account_id,
                status,
                self.input.op.protocol(),
            )
            .await;
            durable &= secured;
            if !secured {
                outcome = ClientResponseOutcome::ResponseFailed;
            }
        } else {
            self.input.balance.release().await;
            self.input.tpm.release().await;
        }
        if let Some(managed) = &self.input.managed {
            match managed.finish(outcome, durable).await {
                Ok((response, terminal)) => {
                    if !matches!(
                        response["status"].as_str(),
                        Some("completed" | "incomplete")
                    ) {
                        outcome = ClientResponseOutcome::ResponseFailed;
                    }
                    if self.stream_started
                        && let Some(terminal) = terminal
                    {
                        // All model I/O has ended. Permit a short bounded final
                        // delivery after the execution deadline, without retrying inference.
                        if !tokio::time::timeout(
                            Duration::from_secs(5),
                            self.body_tx.send(Ok(Bytes::from(terminal))),
                        )
                        .await
                        .is_ok_and(|r| r.is_ok())
                        {
                            outcome = ClientResponseOutcome::ClientDisconnected;
                        }
                    }
                }
                Err(_) => {
                    outcome = ClientResponseOutcome::ResponseFailed;
                }
            }
        }
        if let Some(sender) = self.outcome_tx.take() {
            let _ = sender.send(outcome);
        }
    }
}

fn summary_outcome(summary: &NodeNativeStreamSummary) -> NativeStreamTerminalOutcome {
    summary
        .terminal_outcome
        .unwrap_or(NativeStreamTerminalOutcome::Incomplete)
}

fn with_sse_content_type(mut headers: Vec<(String, String)>) -> Vec<(String, String)> {
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        headers.push(("content-type".into(), "text/event-stream".into()));
    }
    headers
}

fn anthropic_envelope_to_sse(data: &str) -> Option<Bytes> {
    let value: Value = serde_json::from_str(data).ok()?;
    if value.get("kind").and_then(Value::as_str) != Some("anthropic_sse") {
        return None;
    }
    responses_event_to_sse(value.get("event")?.as_str()?, value.get("data")?)
}

fn responses_event_to_sse(event: &str, data: &Value) -> Option<Bytes> {
    use std::io::Write;
    if event.len() > 256 || event.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
        return None;
    }
    struct BoundedFrame(Vec<u8>);
    impl std::io::Write for BoundedFrame {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len())
                > keycompute_types::node_stream::MAX_NATIVE_SSE_FRAME_BYTES
            {
                return Err(std::io::Error::other("native SSE frame limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut frame = BoundedFrame(Vec::with_capacity(1024));
    write!(&mut frame, "event: {event}\ndata: ").ok()?;
    serde_json::to_writer(&mut frame, data).ok()?;
    frame.write_all(b"\n\n").ok()?;
    Some(Bytes::from(frame.0))
}

fn anthropic_envelope_is_error(data: &str) -> bool {
    serde_json::from_str::<Value>(data)
        .ok()
        .is_some_and(|value| {
            value.get("event").and_then(Value::as_str) == Some("error")
                || value.pointer("/data/type").and_then(Value::as_str) == Some("error")
        })
}
