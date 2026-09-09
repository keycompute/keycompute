//! OpenAI Responses API WebSocket transport.
//!
//! The public wire protocol implements the `response.create` flow from OpenAI's
//! `/v1/responses` WebSocket mode. Connection-control events such as
//! `response.steer` and `response.inject` are intentionally unsupported because
//! execution reuses KeyCompute's HTTP Responses pipeline. This keeps account
//! routing, failover, billing, tracing and upstream compatibility in one place:
//! an upstream only needs the Responses HTTP/SSE protocol.

use super::responses::{
    OPENAI_RESPONSES_BODY_LIMIT_BYTES, OPENAI_RESPONSES_REQUEST_WORKING_SET_LIMIT_BYTES,
    ResponsesWarmup, estimated_context_bytes, estimated_json_bytes, estimated_map_bytes,
    persist_responses_warmup, responses_inner, stored_warmup_context, stored_warmup_context_size,
    validate_responses_request,
};
use crate::{
    error::ApiError,
    extractors::{AuthExtractor, ClientRequestId, RequestId, RequestReceivedAt},
    middleware::{
        enforce_authenticated_maintenance_mode, enforce_authenticated_rate_limit,
        openai_responses_error_fields, sanitize_openai_responses_error_code,
        sanitize_openai_responses_error_param,
    },
    state::AppState,
};
use axum::{
    body::Body,
    extract::{
        State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures::{Sink, SinkExt, Stream, StreamExt};
use keycompute_auth::Permission;
use llm_protocol_provider::estimated_json_parse_working_set_bytes;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    io::{self, Write},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc};

const MAX_ACTIVE_RESPONSES: usize = 16;
const MAX_NAMED_STREAMS: usize = 32;
const MAX_CACHED_RESPONSES: usize = 128;
const MAX_CACHED_RESPONSE_BYTES: usize = 96 * 1024 * 1024;
const MAX_RESIDENT_REQUESTS: usize = 64;
// A request is decoded from its WebSocket text frame into a serde_json tree.
// The frame and tree coexist during parsing; after the frame is released, the
// same headroom covers one logical copy retained for stateless continuation.
// The shared 192 MiB budget keeps the official 80 MiB wire limit usable while
// accounting for both simultaneously live representations.
const MAX_RESIDENT_REQUEST_BYTES: usize = OPENAI_RESPONSES_REQUEST_WORKING_SET_LIMIT_BYTES;
const MAX_SSE_LINE_BYTES: usize = 96 * 1024 * 1024;
const MAX_SSE_EVENT_BYTES: usize = 96 * 1024 * 1024;
const MAX_STREAM_ID_BYTES: usize = 256;
// A maximum-sized upstream SSE JSON value is still valid after the WebSocket
// transport adds its stream_id envelope. Keep this as total connection
// backpressure, not an additional allocation per queued event.
const MAX_STREAM_EVENT_ENVELOPE_BYTES: usize = MAX_STREAM_ID_BYTES + 32;
const MAX_RESIDENT_OUTBOUND_BYTES: usize = MAX_SSE_EVENT_BYTES + MAX_STREAM_EVENT_ENVELOPE_BYTES;
const LANE_QUEUE_CAPACITY: usize = 64;
const OUTBOUND_QUEUE_CAPACITY: usize = 256;
const OUTBOUND_SEND_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECTION_LIMIT: Duration = Duration::from_secs(60 * 60);

type LaneId = Option<String>;

#[derive(Debug)]
struct ParsedCreate {
    body: Value,
    lane: LaneId,
}

#[derive(Debug)]
struct RequestBudget {
    _request_permit: OwnedSemaphorePermit,
    _byte_permits: Vec<OwnedSemaphorePermit>,
    byte_slots: Arc<Semaphore>,
    reserved_bytes: usize,
    max_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestBudgetExtensionError {
    HardLimit,
    ConnectionCapacity,
}

struct OutboundMessage {
    message: Message,
    _byte_permit: OwnedSemaphorePermit,
}

#[derive(Clone)]
struct OutboundSender {
    tx: mpsc::Sender<OutboundMessage>,
    byte_slots: Arc<Semaphore>,
    max_bytes: usize,
}

impl OutboundSender {
    fn new(capacity: usize) -> (Self, mpsc::Receiver<OutboundMessage>) {
        Self::with_byte_limit(capacity, MAX_RESIDENT_OUTBOUND_BYTES)
    }

    fn with_byte_limit(
        capacity: usize,
        max_bytes: usize,
    ) -> (Self, mpsc::Receiver<OutboundMessage>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self {
                tx,
                byte_slots: Arc::new(Semaphore::new(max_bytes)),
                max_bytes,
            },
            rx,
        )
    }

    async fn send(&self, message: Message, bytes: usize) -> std::result::Result<(), ()> {
        let permit = self.reserve(bytes).await?;
        self.send_reserved(message, permit).await
    }

    async fn reserve(&self, bytes: usize) -> std::result::Result<OwnedSemaphorePermit, ()> {
        let bytes = u32::try_from(bytes.max(1)).map_err(|_| ())?;
        if bytes as usize > self.max_bytes {
            return Err(());
        }
        tokio::time::timeout(
            OUTBOUND_SEND_TIMEOUT,
            Arc::clone(&self.byte_slots).acquire_many_owned(bytes),
        )
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
    }

    async fn send_reserved(
        &self,
        message: Message,
        permit: OwnedSemaphorePermit,
    ) -> std::result::Result<(), ()> {
        tokio::time::timeout(
            OUTBOUND_SEND_TIMEOUT,
            self.tx.send(OutboundMessage {
                message,
                _byte_permit: permit,
            }),
        )
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
    }
}

#[derive(Debug)]
struct QueuedCreate {
    body: Value,
    lane: LaneId,
    _budget: RequestBudget,
}

#[derive(Clone, Debug)]
struct CachedResponse {
    model: Option<String>,
    context_items: Vec<Value>,
    warmup_request_state: serde_json::Map<String, Value>,
    stored: bool,
    local_warmup_persisted: bool,
    upstream_previous_response_id: Option<String>,
    lane: LaneId,
    estimated_bytes: usize,
}

impl CachedResponse {
    fn new(
        model: Option<String>,
        context_items: Vec<Value>,
        warmup_request_state: serde_json::Map<String, Value>,
        stored: bool,
        upstream_previous_response_id: Option<String>,
        lane: LaneId,
    ) -> Self {
        let estimated_bytes = estimated_context_bytes(&context_items)
            .saturating_add(model.as_ref().map_or(0, |model| model.len() + 24))
            .saturating_add(estimated_map_bytes(&warmup_request_state))
            .saturating_add(
                upstream_previous_response_id
                    .as_ref()
                    .map_or(0, |id| id.len().saturating_add(24)),
            )
            .saturating_add(lane.as_ref().map_or(0, |id| id.len().saturating_add(24)))
            .saturating_add(64);
        Self {
            model,
            context_items,
            warmup_request_state,
            stored,
            local_warmup_persisted: false,
            upstream_previous_response_id,
            lane,
            estimated_bytes,
        }
    }

    fn warmup(
        model: Option<String>,
        context_items: Vec<Value>,
        warmup_request_state: serde_json::Map<String, Value>,
        persisted: bool,
        upstream_previous_response_id: Option<String>,
        lane: LaneId,
    ) -> Self {
        let mut response = Self::new(
            model,
            context_items,
            warmup_request_state,
            false,
            upstream_previous_response_id,
            lane,
        );
        response.local_warmup_persisted = persisted;
        response
    }

    fn terminal(
        model: Option<String>,
        mut context_items: Vec<Value>,
        stored: bool,
        upstream_previous_response_id: Option<String>,
        lane: LaneId,
    ) -> Self {
        // Persisted upstream responses can be hydrated by ID, so retaining a
        // second copy of their complete multimodal context serves no purpose.
        if stored {
            context_items.clear();
        }
        Self::new(
            model,
            context_items,
            serde_json::Map::new(),
            stored,
            if stored {
                None
            } else {
                upstream_previous_response_id
            },
            lane,
        )
    }
}

#[derive(Default, Debug)]
struct ConnectionCache {
    responses: HashMap<String, CachedResponse>,
    insertion_order: VecDeque<String>,
    estimated_bytes: usize,
}

impl ConnectionCache {
    fn get(&self, response_id: &str) -> Option<&CachedResponse> {
        self.responses.get(response_id)
    }

    #[cfg(test)]
    fn insert(&mut self, response_id: String, response: CachedResponse) -> bool {
        self.insert_with_limits(
            response_id,
            response,
            MAX_CACHED_RESPONSES,
            MAX_CACHED_RESPONSE_BYTES,
        )
    }

    fn replace_lane(&mut self, response_id: String, response: CachedResponse) -> bool {
        self.replace_lane_with_limits(
            response_id,
            response,
            MAX_CACHED_RESPONSES,
            MAX_CACHED_RESPONSE_BYTES,
        )
    }

    fn replace_lane_with_limits(
        &mut self,
        response_id: String,
        response: CachedResponse,
        max_entries: usize,
        max_bytes: usize,
    ) -> bool {
        // Validate an individually uncacheable continuation before touching the
        // current lane. A failed replacement must leave its parent retryable.
        if max_entries == 0 || cache_entry_bytes(&response_id, &response) > max_bytes {
            return false;
        }
        let lane = response.lane.clone();
        self.evict_lane(&lane);
        let inserted = self.insert_with_limits(response_id, response, max_entries, max_bytes);
        debug_assert!(inserted, "a preflighted cache entry must be insertable");
        inserted
    }

    fn insert_with_limits(
        &mut self,
        response_id: String,
        response: CachedResponse,
        max_entries: usize,
        max_bytes: usize,
    ) -> bool {
        self.remove(&response_id);
        let entry_bytes = cache_entry_bytes(&response_id, &response);
        if max_entries == 0 || entry_bytes > max_bytes {
            return false;
        }
        while self.responses.len() >= max_entries
            || self.estimated_bytes.saturating_add(entry_bytes) > max_bytes
        {
            if !self.evict_oldest() {
                return false;
            }
        }
        self.estimated_bytes = self.estimated_bytes.saturating_add(entry_bytes);
        self.insertion_order.push_back(response_id.clone());
        self.responses.insert(response_id, response);
        true
    }

    fn evict_same_lane_parent(&mut self, response_id: &str, lane: &LaneId) {
        if self
            .responses
            .get(response_id)
            .is_some_and(|cached| &cached.lane == lane)
        {
            self.remove(response_id);
        }
    }

    fn evict_lane(&mut self, lane: &LaneId) {
        let response_ids = self
            .responses
            .iter()
            .filter_map(|(response_id, response)| {
                (&response.lane == lane).then_some(response_id.clone())
            })
            .collect::<Vec<_>>();
        for response_id in response_ids {
            self.remove(&response_id);
        }
    }

    fn remove(&mut self, response_id: &str) -> Option<CachedResponse> {
        let response = self.responses.remove(response_id)?;
        self.estimated_bytes = self
            .estimated_bytes
            .saturating_sub(cache_entry_bytes(response_id, &response));
        self.insertion_order.retain(|id| id != response_id);
        Some(response)
    }

    fn evict_oldest(&mut self) -> bool {
        while let Some(response_id) = self.insertion_order.pop_front() {
            if let Some(response) = self.responses.remove(&response_id) {
                self.estimated_bytes = self
                    .estimated_bytes
                    .saturating_sub(cache_entry_bytes(&response_id, &response));
                return true;
            }
        }
        false
    }
}

fn cache_entry_bytes(response_id: &str, response: &CachedResponse) -> usize {
    response
        .estimated_bytes
        .saturating_add(response_id.len())
        .saturating_add(128)
}

fn preparse_request_working_set_bytes(text: &str) -> usize {
    estimated_json_parse_working_set_bytes(text.as_bytes())
}

fn parsed_request_working_set_bytes(text_len: usize, body: &Value) -> usize {
    text_len.saturating_add(estimated_json_bytes(body))
}

#[derive(Debug)]
struct ProtocolError {
    code: String,
    message: Box<str>,
    param: Option<String>,
    sequence_number: u64,
    lane: LaneId,
}

impl ProtocolError {
    fn invalid(
        code: impl Into<String>,
        message: impl Into<String>,
        param: Option<&str>,
        lane: LaneId,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into().into_boxed_str(),
            param: param.map(str::to_string),
            sequence_number: 0,
            lane,
        }
    }

    fn server(message: impl Into<String>, lane: LaneId) -> Self {
        Self {
            code: "server_error".to_string(),
            message: message.into().into_boxed_str(),
            param: None,
            sequence_number: 0,
            lane,
        }
    }

    fn with_lane(mut self, lane: LaneId) -> Self {
        if self.lane.is_none() {
            self.lane = lane;
        }
        self
    }

    fn with_sequence_number(mut self, sequence_number: u64) -> Self {
        self.sequence_number = sequence_number;
        self
    }

    fn event(&self) -> Value {
        let mut event = json!({
            "type": "error",
            "code": self.code,
            "message": self.message,
            "param": self.param,
            "sequence_number": self.sequence_number,
        });
        attach_stream_id(&mut event, &self.lane);
        event
    }
}

/// GET /v1/responses with `Upgrade: websocket`.
///
/// Accepts `response.create` client events only. Mid-turn control events require
/// an upstream WebSocket lifecycle and are rejected explicitly.
pub async fn responses_websocket(
    State(state): State<AppState>,
    auth: AuthExtractor,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(connection_permit) = state
        .responses_websocket_admission
        .try_acquire(auth.tenant_id)
        .await
    else {
        return ApiError::RateLimit("Too many active Responses WebSocket connections".to_string())
            .into_response();
    };
    upgrade
        .max_message_size(OPENAI_RESPONSES_BODY_LIMIT_BYTES)
        // Tungstenite otherwise keeps its 16 MiB single-frame default. Standard
        // clients are allowed to send a valid large response.create in one
        // frame, including the official inline-skill maximum.
        .max_frame_size(OPENAI_RESPONSES_BODY_LIMIT_BYTES)
        .on_upgrade(move |socket| async move {
            let _connection_permit = connection_permit;
            serve_connection(socket, state, auth, headers).await;
        })
}

async fn serve_connection(
    socket: WebSocket,
    state: AppState,
    _auth: AuthExtractor,
    headers: HeaderMap,
) {
    let (sink, source) = socket.split();
    serve_connection_parts(sink, source, state, headers).await;
}

async fn serve_connection_parts<S, R, E>(
    mut sink: S,
    mut source: R,
    state: AppState,
    headers: HeaderMap,
) where
    S: Sink<Message> + Send + Unpin + 'static,
    S::Error: Send,
    R: Stream<Item = std::result::Result<Message, E>> + Unpin,
    E: std::fmt::Display,
{
    let (outbound_tx, mut outbound_rx) = OutboundSender::new(OUTBOUND_QUEUE_CAPACITY);
    let mut writer = tokio::spawn(async move {
        while let Some(message) = outbound_rx.recv().await {
            if !matches!(
                tokio::time::timeout(OUTBOUND_SEND_TIMEOUT, sink.send(message.message)).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
    });

    let cache = Arc::new(Mutex::new(ConnectionCache::default()));
    let active = Arc::new(Semaphore::new(MAX_ACTIVE_RESPONSES));
    let resident_requests = Arc::new(Semaphore::new(MAX_RESIDENT_REQUESTS));
    let resident_request_bytes = Arc::new(Semaphore::new(MAX_RESIDENT_REQUEST_BYTES));
    let mut lanes: HashMap<LaneId, mpsc::Sender<QueuedCreate>> = HashMap::new();
    let mut lane_workers = Vec::new();
    let connection_limit = tokio::time::sleep(CONNECTION_LIMIT);
    tokio::pin!(connection_limit);
    let mut writer_finished = false;

    loop {
        tokio::select! {
            biased;
            result = &mut writer => {
                writer_finished = true;
                if let Err(error) = result {
                    tracing::debug!(%error, "Responses WebSocket writer task failed");
                }
                break;
            }
            _ = &mut connection_limit => {
                let error = ProtocolError::invalid(
                    "websocket_connection_limit_reached",
                    "Responses websocket connection limit reached (60 minutes). Create a new websocket connection to continue.",
                    None,
                    None,
                );
                let _ = send_json(&outbound_tx, error.event()).await;
                let _ = outbound_tx.send(
                    Message::Close(Some(CloseFrame {
                        code: 1000,
                        reason: "Responses websocket connection limit reached".into(),
                    })),
                    64,
                ).await;
                break;
            }
            incoming = source.next() => {
                let Some(incoming) = incoming else { break };
                let message = match incoming {
                    Ok(message) => message,
                    Err(error) => {
                        tracing::debug!(%error, "Responses WebSocket client disconnected");
                        break;
                    }
                };

                match message {
                    Message::Text(text) => {
                        let preflight_bytes = preparse_request_working_set_bytes(text.as_str());
                        if preflight_bytes > MAX_RESIDENT_REQUEST_BYTES {
                            let lane = response_create_lane_hint(text.as_str());
                            let error = ProtocolError::invalid(
                                "request_too_large",
                                "The decoded response.create event exceeds this server's memory limit.",
                                None,
                                lane,
                            );
                            let _ = send_json(&outbound_tx, error.event()).await;
                            continue;
                        }
                        let mut budget = match try_reserve_request_budget(
                            &resident_requests,
                            &resident_request_bytes,
                            MAX_RESIDENT_REQUEST_BYTES,
                            preflight_bytes,
                        ) {
                            Ok(budget) => budget,
                            Err(error) => {
                                let lane = response_create_lane_hint(text.as_str());
                                let error = request_budget_protocol_error(error, lane);
                                let _ = send_json(&outbound_tx, error.event()).await;
                                continue;
                            }
                        };
                        let parsed = match parse_response_create(text.as_str()) {
                            Ok(parsed) => parsed,
                            Err(error) => {
                                let _ = send_json(&outbound_tx, error.event()).await;
                                continue;
                            }
                        };
                        let estimated_bytes =
                            parsed_request_working_set_bytes(text.len(), &parsed.body);
                        if estimated_bytes > MAX_RESIDENT_REQUEST_BYTES {
                            let error = ProtocolError::invalid(
                                "request_too_large",
                                "The decoded response.create event exceeds this server's memory limit.",
                                None,
                                parsed.lane.clone(),
                            );
                            let _ = send_json(&outbound_tx, error.event()).await;
                            continue;
                        }
                        drop(text);
                        if let Err(error) = try_extend_request_budget(
                            &mut budget,
                            estimated_bytes.saturating_sub(preflight_bytes),
                        ) {
                            let error = request_budget_protocol_error(error, parsed.lane.clone());
                            let _ = send_json(&outbound_tx, error.event()).await;
                            continue;
                        }
                        let queued = QueuedCreate {
                            body: parsed.body,
                            lane: parsed.lane,
                            _budget: budget,
                        };

                        if queued.lane.is_some()
                            && !lanes.contains_key(&queued.lane)
                            && lanes.keys().filter(|lane| lane.is_some()).count() >= MAX_NAMED_STREAMS
                        {
                            let error = ProtocolError::invalid(
                                "websocket_stream_limit_reached",
                                "This WebSocket connection has reached its maximum number of distinct stream IDs (32). Reuse an existing stream_id or open a new WebSocket connection.",
                                Some("stream_id"),
                                queued.lane.clone(),
                            );
                            let _ = send_json(&outbound_tx, error.event()).await;
                            continue;
                        }

                        let lane_sender = if let Some(sender) = lanes.get(&queued.lane) {
                            sender.clone()
                        } else {
                            let (lane_tx, lane_rx) = mpsc::channel(LANE_QUEUE_CAPACITY);
                            let worker = tokio::spawn(run_lane(
                                lane_rx,
                                state.clone(),
                                headers.clone(),
                                Arc::clone(&cache),
                                Arc::clone(&active),
                                outbound_tx.clone(),
                            ));
                            lanes.insert(queued.lane.clone(), lane_tx.clone());
                            lane_workers.push(worker);
                            lane_tx
                        };
                        match lane_sender.try_send(queued) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(queued)) => {
                                let lane = queued.lane.clone();
                                drop(queued);
                                let error = request_budget_protocol_error(
                                    RequestBudgetExtensionError::ConnectionCapacity,
                                    lane,
                                );
                                let _ = send_json(&outbound_tx, error.event()).await;
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => break,
                        }
                    }
                    Message::Binary(_) => {
                        let error = ProtocolError::invalid(
                            "invalid_event",
                            "WebSocket events must be UTF-8 JSON text messages.",
                            None,
                            None,
                        );
                        let _ = send_json(&outbound_tx, error.event()).await;
                    }
                    Message::Ping(payload) => {
                        let bytes = payload.len();
                        if outbound_tx.send(Message::Pong(payload), bytes).await.is_err() {
                            break;
                        }
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => break,
                }
            }
        }
    }

    lanes.clear();
    for worker in lane_workers {
        worker.abort();
    }
    drop(outbound_tx);
    if !writer_finished {
        let _ = writer.await;
    }
}

async fn run_lane(
    mut requests: mpsc::Receiver<QueuedCreate>,
    state: AppState,
    headers: HeaderMap,
    cache: Arc<Mutex<ConnectionCache>>,
    active: Arc<Semaphore>,
    outbound: OutboundSender,
) {
    while let Some(request) = requests.recv().await {
        let permit = match Arc::clone(&active).acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return,
        };
        process_response_create(
            request,
            state.clone(),
            headers.clone(),
            Arc::clone(&cache),
            outbound.clone(),
        )
        .await;
        drop(permit);
    }
}

fn try_reserve_request_budget(
    request_slots: &Arc<Semaphore>,
    byte_slots: &Arc<Semaphore>,
    max_bytes: usize,
    body_bytes: usize,
) -> std::result::Result<RequestBudget, RequestBudgetExtensionError> {
    if body_bytes > max_bytes {
        return Err(RequestBudgetExtensionError::HardLimit);
    }
    // The sole socket reader must never wait for request capacity: doing so
    // would prevent it from dispatching other lanes or handling Ping/Close.
    // Reject only this request when the bounded connection queue is full.
    let request_permit = Arc::clone(request_slots)
        .try_acquire_owned()
        .map_err(|_| RequestBudgetExtensionError::ConnectionCapacity)?;
    let body_bytes =
        u32::try_from(body_bytes.max(1)).map_err(|_| RequestBudgetExtensionError::HardLimit)?;
    let byte_permit = Arc::clone(byte_slots)
        .try_acquire_many_owned(body_bytes)
        .map_err(|_| RequestBudgetExtensionError::ConnectionCapacity)?;
    Ok(RequestBudget {
        _request_permit: request_permit,
        _byte_permits: vec![byte_permit],
        byte_slots: Arc::clone(byte_slots),
        reserved_bytes: body_bytes as usize,
        max_bytes,
    })
}

/// Extract a valid named lane without materializing the complete JSON tree.
/// This is used only when admission fails before the normal request parser can
/// safely run, so the request-scoped capacity error can still carry stream_id.
fn response_create_lane_hint(text: &str) -> LaneId {
    #[derive(Deserialize)]
    struct LaneHint<'a> {
        #[serde(default, borrow)]
        stream_id: Option<&'a str>,
    }

    serde_json::from_str::<LaneHint<'_>>(text)
        .ok()
        .and_then(|hint| {
            hint.stream_id
                .filter(|stream_id| valid_stream_id(stream_id))
        })
        .map(str::to_string)
}

fn try_extend_request_budget(
    budget: &mut RequestBudget,
    additional_bytes: usize,
) -> std::result::Result<(), RequestBudgetExtensionError> {
    if additional_bytes == 0 {
        return Ok(());
    }
    if budget.reserved_bytes.saturating_add(additional_bytes) > budget.max_bytes {
        return Err(RequestBudgetExtensionError::HardLimit);
    }
    let Ok(additional_bytes) = u32::try_from(additional_bytes) else {
        return Err(RequestBudgetExtensionError::HardLimit);
    };
    // Never wait for more byte permits while retaining this request's initial
    // permits. Parallel lanes can otherwise each hold part of the connection
    // budget and wait forever for capacity that only those same lanes can
    // release. A request-scoped capacity error leaves the connection and other
    // lanes usable, and the client may retry after in-flight work completes.
    let permit = Arc::clone(&budget.byte_slots)
        .try_acquire_many_owned(additional_bytes)
        .map_err(|_| RequestBudgetExtensionError::ConnectionCapacity)?;
    budget._byte_permits.push(permit);
    budget.reserved_bytes = budget
        .reserved_bytes
        .saturating_add(additional_bytes as usize);
    Ok(())
}

fn continuation_context_copy_count(generate: bool, requested_store: bool) -> usize {
    // Generated responses must keep one copy in the upstream request and a
    // second until the terminal response confirms that upstream storage is
    // available. Stored warmups likewise need headroom for durable encoding.
    if generate || requested_store { 2 } else { 1 }
}

fn snapshot_cached_parent(
    cache: &ConnectionCache,
    previous_response_id: Option<&str>,
    continuation_context_copies: usize,
) -> (Option<CachedResponse>, Option<usize>) {
    let parent = previous_response_id
        .and_then(|response_id| cache.get(response_id))
        .cloned();
    let additional_bytes = parent
        .as_ref()
        .filter(|parent| !parent.stored)
        .map(|parent| {
            parent
                .estimated_bytes
                .saturating_mul(continuation_context_copies)
        });
    (parent, additional_bytes)
}

fn request_budget_protocol_error(
    error: RequestBudgetExtensionError,
    lane: LaneId,
) -> ProtocolError {
    match error {
        RequestBudgetExtensionError::HardLimit => ProtocolError::invalid(
            "request_too_large",
            "The decoded response.create event and its continuation context exceed this server's memory limit.",
            None,
            lane,
        ),
        RequestBudgetExtensionError::ConnectionCapacity => ProtocolError {
            code: "websocket_request_capacity_exceeded".to_string(),
            message: "This WebSocket connection does not currently have enough request memory capacity. Retry after another in-flight response finishes."
                .into(),
            param: None,
            sequence_number: 0,
            lane,
        },
    }
}

fn response_cache_capacity_protocol_error(lane: LaneId) -> ProtocolError {
    ProtocolError {
        code: "response_cache_capacity_exceeded".to_string(),
        message: "The completed stateless response context exceeds this WebSocket connection's continuation cache limit."
            .into(),
        param: None,
        sequence_number: 0,
        lane,
    }
}

fn parse_response_create(text: &str) -> Result<ParsedCreate, ProtocolError> {
    let body: Value = serde_json::from_str(text).map_err(|_| {
        ProtocolError::invalid(
            "invalid_event",
            "WebSocket message must be a valid JSON object.",
            None,
            None,
        )
    })?;
    let object = body.as_object().ok_or_else(|| {
        ProtocolError::invalid(
            "invalid_event",
            "WebSocket message must be a JSON object.",
            None,
            None,
        )
    })?;
    if object.get("type").and_then(Value::as_str) != Some("response.create") {
        return Err(ProtocolError::invalid(
            "invalid_event",
            "KeyCompute WebSocket mode supports 'response.create' events only; 'response.steer' and 'response.inject' are not supported.",
            Some("type"),
            None,
        ));
    }

    let lane = match object.get("stream_id") {
        None => None,
        Some(Value::String(stream_id)) if valid_stream_id(stream_id) => Some(stream_id.clone()),
        Some(_) => {
            return Err(ProtocolError::invalid(
                "invalid_stream_id",
                "The 'stream_id' field must be a non-empty string with at most 256 characters and may only contain letters, numbers, underscores, hyphens, and periods.",
                Some("stream_id"),
                None,
            ));
        }
    };

    Ok(ParsedCreate { body, lane })
}

fn valid_stream_id(stream_id: &str) -> bool {
    !stream_id.is_empty()
        && stream_id.len() <= MAX_STREAM_ID_BYTES
        && stream_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

async fn process_response_create(
    request: QueuedCreate,
    state: AppState,
    headers: HeaderMap,
    cache: Arc<Mutex<ConnectionCache>>,
    outbound: OutboundSender,
) {
    let QueuedCreate {
        mut body,
        lane,
        _budget: mut request_budget,
    } = request;
    let previous_response_id = body
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    // Authentication, validation, and rate-limit failures occur before a
    // response is started and leave a stateless parent available for retry.
    // Once the Responses pipeline returns an HTTP response, either a non-2xx
    // response or a later SSE failure invalidates a same-lane parent.
    let mut response_stream_accepted = false;

    let result = async {
        // The handshake only authenticates the upgrade. Revalidate the
        // original credential for every response.create so revocation, expiry,
        // user disablement and tenant changes take effect during a 60-minute
        // connection.
        let auth = AuthExtractor::from_header_with_auth(&headers, state.auth.as_ref())
            .await
            .map_err(|error| api_error(error, lane.clone()))?;
        if !auth.has_permission(&Permission::UseApi) {
            return Err(api_error(
                ApiError::Forbidden(
                    "API-use permission is required for Responses WebSocket events".to_string(),
                ),
                lane.clone(),
            ));
        }
        enforce_authenticated_maintenance_mode(&state, &auth)
            .await
            .map_err(|error| api_error(error, lane.clone()))?;
        validate_transport_fields(&mut body, lane.clone())?;
        validate_responses_request(&body).map_err(|error| api_error(error, lane.clone()))?;

        let generate = body
            .get("generate")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        enforce_authenticated_rate_limit(&state, &auth)
            .await
            .map_err(|error| api_error(error, lane.clone()))?;
        let stored = body.get("store").and_then(Value::as_bool).unwrap_or(true);
        let explicit_model = body
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.trim().is_empty())
            .map(str::to_string);
        let continuation_context_copies = continuation_context_copy_count(generate, stored);

        let (cached_parent, cached_parent_budget) = {
            let guard = cache.lock().await;
            snapshot_cached_parent(
                &guard,
                previous_response_id.as_deref(),
                continuation_context_copies,
            )
        };
        if let Some(additional_bytes) = cached_parent_budget {
            try_extend_request_budget(&mut request_budget, additional_bytes)
                .map_err(|error| request_budget_protocol_error(error, lane.clone()))?;
        }
        let (
            parent_context,
            parent_warmup_request_state,
            replay_parent,
            upstream_previous_response_id,
            local_parent_persisted,
            parent_model,
        ) = if let Some(parent) = cached_parent {
            if parent.stored {
                (
                    Vec::new(),
                    serde_json::Map::new(),
                    false,
                    previous_response_id.clone(),
                    false,
                    parent.model,
                )
            } else {
                (
                    parent.context_items,
                    parent.warmup_request_state,
                    true,
                    parent.upstream_previous_response_id,
                    parent.local_warmup_persisted,
                    parent.model,
                )
            }
        } else if let Some(previous_response_id) = previous_response_id.as_deref() {
            if let Some(context_bytes) =
                stored_warmup_context_size(&state, auth.tenant_id, previous_response_id)
                    .await
                    .map_err(|error| api_error(error, lane.clone()))?
            {
                let context_budget = context_bytes.saturating_mul(continuation_context_copies);
                try_extend_request_budget(&mut request_budget, context_budget)
                    .map_err(|error| request_budget_protocol_error(error, lane.clone()))?;
            }
            match stored_warmup_context(&state, auth.tenant_id, previous_response_id)
                .await
                .map_err(|error| api_error(error, lane.clone()))?
            {
                Some(context) => (
                    context.items,
                    context.request_state,
                    true,
                    context.upstream_previous_response_id,
                    true,
                    context.model,
                ),
                None => {
                    if let Some(error) =
                        missing_local_previous_response_error(previous_response_id, lane.clone())
                    {
                        return Err(error);
                    }
                    (
                        Vec::new(),
                        serde_json::Map::new(),
                        false,
                        Some(previous_response_id.to_string()),
                        false,
                        None,
                    )
                }
            }
        } else {
            (Vec::new(), serde_json::Map::new(), false, None, false, None)
        };
        // A generated replay needs one context copy for the upstream body and
        // one until the terminal response confirms whether upstream storage is
        // actually available. Move the current input before cloning that
        // completed context so the original body cannot temporarily retain an
        // unbudgeted third copy. An unstored warmup can move it as well because
        // it has no persistence request body to preserve.
        let request_input = if replay_parent && (generate || !stored) {
            take_normalized_input(&mut body)
        } else {
            normalize_input(body.get("input"))
        };
        let model = explicit_model.or(parent_model);

        if !generate {
            let response_id = format!("resp_ws_{}", uuid::Uuid::new_v4().simple());
            let mut context_items = parent_context;
            context_items.extend(request_input);
            let mut request_state = parent_warmup_request_state;
            request_state.extend(warmup_request_state(&body));
            let persisted_response = warmup_response_object(
                &response_id,
                model.as_deref(),
                stored,
                previous_response_id.as_deref(),
            );
            if stored {
                prepare_warmup_persistence_request_body(
                    &mut body,
                    replay_parent,
                    local_parent_persisted,
                    upstream_previous_response_id.as_deref(),
                );
                persist_responses_warmup(
                    &state,
                    &auth,
                    ResponsesWarmup {
                        request_id: uuid::Uuid::new_v4(),
                        response_id: &response_id,
                        request_body: body,
                        response: persisted_response.clone(),
                        context_items: &context_items,
                        request_state: &request_state,
                        upstream_previous_response_id: upstream_previous_response_id.as_deref(),
                    },
                )
                .await
                .map_err(|error| api_error(error, lane.clone()))?;
            }
            let mut cache = cache.lock().await;
            // Official WebSocket continuation keeps the latest cached response
            // for each lane. Cross-lane parents remain available because their
            // cached lane differs from this one.
            let cached = cache.replace_lane(
                response_id.clone(),
                CachedResponse::warmup(
                    model.clone(),
                    context_items,
                    request_state,
                    stored,
                    upstream_previous_response_id,
                    lane.clone(),
                ),
            );
            if !cached {
                if stored {
                    // The durable local warmup can be replayed from the
                    // database, so the connection-local copy is optional.
                    cache.evict_lane(&lane);
                } else {
                    drop(cache);
                    send_json(
                        &outbound,
                        response_cache_capacity_protocol_error(lane.clone()).event(),
                    )
                    .await?;
                    return Ok(());
                }
            }
            drop(cache);
            emit_warmup_events(&outbound, &lane, &persisted_response).await?;
            return Ok(());
        }

        let mut full_context = parent_context;
        full_context.extend(request_input);
        let client_previous_response_id = previous_response_id.clone();
        if replay_parent {
            apply_warmup_request_state(&mut body, &parent_warmup_request_state);
            let object = body
                .as_object_mut()
                .expect("response.create was validated as an object");
            object.insert("input".to_string(), Value::Array(full_context.clone()));
            if object.get("model").is_none_or(Value::is_null)
                && let Some(model) = model.as_ref()
            {
                object.insert("model".to_string(), Value::String(model.clone()));
            }
            if let Some(upstream_previous_response_id) = upstream_previous_response_id.as_deref() {
                object.insert(
                    "previous_response_id".to_string(),
                    Value::String(upstream_previous_response_id.to_string()),
                );
            } else {
                object.remove("previous_response_id");
            }
        }
        let object = body
            .as_object_mut()
            .expect("response.create was validated as an object");
        object.remove("type");
        object.remove("stream_id");
        object.remove("generate");
        object.remove("background");
        object.insert("stream".to_string(), Value::Bool(true));

        let response = responses_inner(
            state,
            auth,
            RequestId::new(),
            ClientRequestId(None),
            RequestReceivedAt(chrono::Utc::now()),
            websocket_event_headers(&headers),
            body,
            None,
            "/v1/responses",
            "/responses",
            true,
        )
        .await
        .map_err(|error| api_error(error, lane.clone()))?;

        let response = require_successful_http_response(
            response,
            &lane,
            &cache,
            previous_response_id.as_deref(),
        )
        .await?;

        response_stream_accepted = true;
        forward_sse_body(
            response.into_body(),
            &outbound,
            &lane,
            &cache,
            full_context,
            stored,
            client_previous_response_id.as_deref(),
            replay_parent,
            upstream_previous_response_id,
        )
        .await
    }
    .await;

    if let Err(error) = result {
        if response_stream_accepted
            && let Some(previous_response_id) = previous_response_id.as_deref()
        {
            cache
                .lock()
                .await
                .evict_same_lane_parent(previous_response_id, &lane);
        }
        let _ = send_json(&outbound, error.event()).await;
    }
}

async fn require_successful_http_response(
    response: Response,
    lane: &LaneId,
    cache: &Arc<Mutex<ConnectionCache>>,
    previous_response_id: Option<&str>,
) -> std::result::Result<Response, ProtocolError> {
    if response.status().is_success() {
        return Ok(response);
    }

    if let Some(previous_response_id) = previous_response_id {
        cache
            .lock()
            .await
            .evict_same_lane_parent(previous_response_id, lane);
    }
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), OPENAI_RESPONSES_BODY_LIMIT_BYTES)
        .await
        .map_err(|_| {
            ProtocolError::server("Failed to read upstream error response.", lane.clone())
        })?;
    Err(protocol_error_from_http(status, &body, lane.clone()))
}

fn prepare_warmup_persistence_request_body(
    body: &mut Value,
    replay_parent: bool,
    local_parent_persisted: bool,
    upstream_previous_response_id: Option<&str>,
) {
    if replay_parent && !local_parent_persisted {
        let object = body
            .as_object_mut()
            .expect("response.create was validated as an object");
        if let Some(upstream_previous_response_id) = upstream_previous_response_id {
            object.insert(
                "previous_response_id".to_string(),
                Value::String(upstream_previous_response_id.to_string()),
            );
        } else {
            object.remove("previous_response_id");
        }
    }
}

fn websocket_event_headers(headers: &HeaderMap) -> HeaderMap {
    let mut headers = headers.clone();
    // Idempotency-Key scopes one HTTP request. Reusing the handshake value for
    // every response.create would collapse independent WebSocket generations.
    headers.remove("idempotency-key");
    // X-Client-Request-Id must identify one request. A handshake value cannot
    // safely be reused for every bridged upstream HTTP response.create call.
    headers.remove("x-client-request-id");
    headers
}

fn validate_transport_fields(body: &mut Value, lane: LaneId) -> Result<(), ProtocolError> {
    let object = body.as_object_mut().ok_or_else(|| {
        ProtocolError::invalid(
            "invalid_event",
            "WebSocket message must be a JSON object.",
            None,
            lane.clone(),
        )
    })?;
    // Current OpenAI SDKs expose the HTTP transport fields on their WebSocket
    // response.create helper and serialize them when supplied. They do not
    // change WebSocket behavior, so accept boolean values and strip them before
    // validation, warmup persistence, and the forced streaming upstream call.
    for field in ["stream", "background"] {
        if let Some(value) = object.remove(field)
            && !value.is_null()
            && !value.is_boolean()
        {
            return Err(ProtocolError::invalid(
                "invalid_request_error",
                format!("{field} must be a boolean or null"),
                Some(field),
                lane.clone(),
            ));
        }
    }
    if let Some(generate) = object.get("generate")
        && !generate.is_boolean()
    {
        return Err(ProtocolError::invalid(
            "invalid_request_error",
            "generate must be a boolean",
            Some("generate"),
            lane,
        ));
    }
    if let Some(store) = object.get("store")
        && !store.is_null()
        && !store.is_boolean()
    {
        return Err(ProtocolError::invalid(
            "invalid_request_error",
            "store must be a boolean or null",
            Some("store"),
            lane,
        ));
    }
    if let Some(previous_response_id) = object.get("previous_response_id")
        && !previous_response_id.is_null()
        && !previous_response_id.is_string()
    {
        return Err(ProtocolError::invalid(
            "invalid_request_error",
            "previous_response_id must be a string or null",
            Some("previous_response_id"),
            lane,
        ));
    }
    Ok(())
}

fn missing_local_previous_response_error(
    previous_response_id: &str,
    lane: LaneId,
) -> Option<ProtocolError> {
    previous_response_id.starts_with("resp_ws_").then(|| {
        ProtocolError::invalid(
            "previous_response_not_found",
            format!("Previous response with id '{previous_response_id}' not found."),
            Some("previous_response_id"),
            lane,
        )
    })
}

fn normalize_input(input: Option<&Value>) -> Vec<Value> {
    normalize_owned_input(input.cloned())
}

fn take_normalized_input(body: &mut Value) -> Vec<Value> {
    let input = body
        .as_object_mut()
        .and_then(|object| object.remove("input"));
    normalize_owned_input(input)
}

fn normalize_owned_input(input: Option<Value>) -> Vec<Value> {
    match input {
        Some(Value::Array(items)) => items,
        Some(Value::String(text)) => vec![json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": text}],
        })],
        Some(Value::Null) | None => Vec::new(),
        Some(value) => vec![value],
    }
}

fn warmup_request_state(body: &Value) -> serde_json::Map<String, Value> {
    let Some(object) = body.as_object() else {
        return serde_json::Map::new();
    };
    object
        .iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "type"
                    | "stream_id"
                    | "generate"
                    | "stream"
                    | "background"
                    | "input"
                    | "previous_response_id"
                    | "model"
                    | "store"
            )
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn apply_warmup_request_state(body: &mut Value, request_state: &serde_json::Map<String, Value>) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    for (name, value) in request_state {
        object.entry(name.clone()).or_insert_with(|| value.clone());
    }
}

async fn emit_warmup_events(
    outbound: &OutboundSender,
    lane: &LaneId,
    response: &Value,
) -> Result<(), ProtocolError> {
    for (sequence_number, event_type, status) in [
        (0, "response.created", "in_progress"),
        (1, "response.in_progress", "in_progress"),
        (2, "response.completed", "completed"),
    ] {
        let mut response = response.clone();
        response["status"] = Value::String(status.to_string());
        if status != "completed" {
            response["completed_at"] = Value::Null;
        }
        let mut event = json!({
            "type": event_type,
            "sequence_number": sequence_number,
            "response": response,
        });
        attach_stream_id(&mut event, lane);
        send_json(outbound, event).await?;
    }
    Ok(())
}

fn warmup_response_object(
    response_id: &str,
    model: Option<&str>,
    stored: bool,
    previous_response_id: Option<&str>,
) -> Value {
    let created_at = chrono::Utc::now().timestamp();
    json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": "completed",
        "completed_at": created_at,
        "error": null,
        "incomplete_details": null,
        "instructions": null,
        "max_output_tokens": null,
        "model": model,
        "output": [],
        "parallel_tool_calls": true,
        "previous_response_id": previous_response_id,
        "reasoning": {"effort": null, "summary": null},
        "store": stored,
        "temperature": 1.0,
        "text": {"format": {"type": "text"}},
        "tool_choice": "auto",
        "tools": [],
        "top_p": 1.0,
        "truncation": "disabled",
        "usage": {
            "input_tokens": 0,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens": 0,
            "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": 0
        },
        "metadata": {}
    })
}

#[allow(clippy::too_many_arguments)]
async fn forward_sse_body(
    body: Body,
    outbound: &OutboundSender,
    lane: &LaneId,
    cache: &Arc<Mutex<ConnectionCache>>,
    mut context_items: Vec<Value>,
    stored: bool,
    client_previous_response_id: Option<&str>,
    patch_previous_response_id: bool,
    upstream_previous_response_id: Option<String>,
) -> Result<(), ProtocolError> {
    let mut stream = body.into_data_stream();
    let mut decoder = SseJsonDecoder::default();
    let mut terminal_response = None;
    let mut terminal_event = None;
    let mut error_forwarded = false;
    let mut next_sequence_number = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            ProtocolError::server("Upstream response stream failed.", lane.clone())
                .with_sequence_number(next_sequence_number)
        })?;
        for mut event in decoder.push(&chunk).map_err(|error| {
            error
                .with_lane(lane.clone())
                .with_sequence_number(next_sequence_number)
        })? {
            normalize_websocket_error_event(&mut event);
            error_forwarded |= event.get("type").and_then(Value::as_str) == Some("error");
            if patch_previous_response_id
                && let Some(response) = event.get_mut("response").and_then(Value::as_object_mut)
            {
                response.insert(
                    "previous_response_id".to_string(),
                    client_previous_response_id
                        .map_or(Value::Null, |id| Value::String(id.to_string())),
                );
            }
            let is_terminal = is_terminal_event(&event);
            if is_terminal {
                terminal_response = event.get("response").cloned();
            }
            attach_stream_id(&mut event, lane);
            if is_terminal {
                // Do not acknowledge a stateless response until its continuation
                // state is safely cached. Deltas remain streamed immediately.
                terminal_event = Some(event);
            } else {
                next_sequence_number = sequence_number_after(&event, next_sequence_number);
                send_json(outbound, event).await?;
            }
        }
    }
    for mut event in decoder.finish().map_err(|error| {
        error
            .with_lane(lane.clone())
            .with_sequence_number(next_sequence_number)
    })? {
        normalize_websocket_error_event(&mut event);
        error_forwarded |= event.get("type").and_then(Value::as_str) == Some("error");
        if patch_previous_response_id
            && let Some(response) = event.get_mut("response").and_then(Value::as_object_mut)
        {
            response.insert(
                "previous_response_id".to_string(),
                client_previous_response_id.map_or(Value::Null, |id| Value::String(id.to_string())),
            );
        }
        let is_terminal = is_terminal_event(&event);
        if is_terminal {
            terminal_response = event.get("response").cloned();
        }
        attach_stream_id(&mut event, lane);
        if is_terminal {
            terminal_event = Some(event);
        } else {
            next_sequence_number = sequence_number_after(&event, next_sequence_number);
            send_json(outbound, event).await?;
        }
    }

    if error_forwarded {
        if let Some(previous_response_id) = client_previous_response_id {
            cache
                .lock()
                .await
                .evict_same_lane_parent(previous_response_id, lane);
        }
        return Ok(());
    }
    if let Some(response) = terminal_response {
        let model = response
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.trim().is_empty())
            .map(str::to_string);
        if let Some(output) = response.get("output").and_then(Value::as_array) {
            context_items.extend(output.iter().cloned());
        }
        if let Some(response_id) = response.get("id").and_then(Value::as_str) {
            let stored = response
                .get("store")
                .and_then(Value::as_bool)
                .unwrap_or(stored);
            let mut cache = cache.lock().await;
            let cached = cache.replace_lane(
                response_id.to_string(),
                CachedResponse::terminal(
                    model,
                    context_items,
                    stored,
                    upstream_previous_response_id,
                    lane.clone(),
                ),
            );
            if !cached {
                if stored {
                    // Stored upstream responses remain continuable by ID even
                    // when their optional local cache entry is too large.
                    cache.evict_lane(lane);
                } else {
                    drop(cache);
                    send_json(
                        outbound,
                        response_cache_capacity_protocol_error(lane.clone())
                            .with_sequence_number(next_sequence_number)
                            .event(),
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
    }
    if let Some(event) = terminal_event {
        send_json(outbound, event).await?;
    }
    Ok(())
}

fn is_terminal_event(event: &Value) -> bool {
    matches!(
        event.get("type").and_then(Value::as_str),
        Some("response.completed" | "response.failed" | "response.incomplete")
    )
}

fn sequence_number_after(event: &Value, fallback: u64) -> u64 {
    event
        .get("sequence_number")
        .and_then(Value::as_u64)
        .map(|sequence_number| sequence_number.saturating_add(1))
        .unwrap_or(fallback)
}

/// Rebuild every upstream error from allowlisted fields while preserving the
/// official flat Responses stream-event shape. Successful events pass through
/// byte-for-byte except for the optional `stream_id` added by the WebSocket
/// transport.
fn normalize_websocket_error_event(event: &mut Value) {
    if event.get("type").and_then(Value::as_str) != Some("error") {
        return;
    }
    let status = event
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .unwrap_or(500);
    let status_code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let (_, default_error_code) = openai_responses_error_fields(status_code);
    let nested_source = event.get("error").and_then(Value::as_object);
    let code = sanitize_openai_responses_error_code(
        nested_source
            .and_then(|error| error.get("code"))
            .or_else(|| event.get("code")),
        default_error_code,
    );
    let param = sanitize_openai_responses_error_param(
        nested_source
            .and_then(|error| error.get("param"))
            .or_else(|| event.get("param")),
    );
    let sequence_number = event
        .get("sequence_number")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    *event = json!({
        "type": "error",
        "code": code,
        "message": "Upstream request failed",
        "param": param,
        "sequence_number": sequence_number,
    });
}

fn attach_stream_id(event: &mut Value, lane: &LaneId) {
    let Some(stream_id) = lane else { return };
    if let Some(object) = event.as_object_mut() {
        object.insert("stream_id".to_string(), Value::String(stream_id.clone()));
    }
}

#[derive(Default)]
struct JsonLengthWriter {
    bytes: usize,
}

impl Write for JsonLengthWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

async fn send_json(outbound: &OutboundSender, event: Value) -> Result<(), ProtocolError> {
    let mut length = JsonLengthWriter::default();
    serde_json::to_writer(&mut length, &event).map_err(|_| {
        ProtocolError::server("Failed to serialize a WebSocket response event.", None)
    })?;
    let permit = outbound.reserve(length.bytes).await.map_err(|()| {
        ProtocolError::server("WebSocket client is not accepting response events.", None)
    })?;
    // Reserve the exact queue budget before allocating the wire string. This
    // keeps concurrent producers from transiently materializing unaccounted
    // escaped payloads while a full outbound queue is applying backpressure.
    let text = serde_json::to_string(&event).map_err(|_| {
        ProtocolError::server("Failed to serialize a WebSocket response event.", None)
    })?;
    match outbound
        .send_reserved(Message::Text(text.into()), permit)
        .await
    {
        Ok(()) => Ok(()),
        Err(()) => Err(ProtocolError::server(
            "WebSocket client is not accepting response events.",
            None,
        )),
    }
}

fn api_error(error: ApiError, lane: LaneId) -> ProtocolError {
    match error {
        ApiError::RateLimit(message) => ProtocolError {
            code: "rate_limit_exceeded".to_string(),
            message: message.into_boxed_str(),
            param: None,
            sequence_number: 0,
            lane,
        },
        ApiError::NotFound(message) if message.contains("Previous response") => {
            ProtocolError::invalid(
                "previous_response_not_found",
                message,
                Some("previous_response_id"),
                lane,
            )
        }
        ApiError::BadRequest(message) | ApiError::NotFound(message) => {
            ProtocolError::invalid("invalid_request_error", message, None, lane)
        }
        ApiError::Auth(message) => ProtocolError {
            code: "invalid_api_key".to_string(),
            message: message.into_boxed_str(),
            param: None,
            sequence_number: 0,
            lane,
        },
        ApiError::Forbidden(message) => ProtocolError {
            code: "permission_denied".to_string(),
            message: message.into_boxed_str(),
            param: None,
            sequence_number: 0,
            lane,
        },
        ApiError::ServiceUnavailable(message) | ApiError::Routing(message) => ProtocolError {
            code: "service_unavailable".to_string(),
            message: message.into_boxed_str(),
            param: None,
            sequence_number: 0,
            lane,
        },
        ApiError::Provider(_) | ApiError::Internal(_) | ApiError::Config(_) => ProtocolError {
            code: "server_error".to_string(),
            message: "Internal server error".into(),
            param: None,
            sequence_number: 0,
            lane,
        },
        other => ProtocolError {
            code: "invalid_request_error".to_string(),
            message: other.to_string().into_boxed_str(),
            param: None,
            sequence_number: 0,
            lane,
        },
    }
}

fn protocol_error_from_http(status: u16, body: &[u8], lane: LaneId) -> ProtocolError {
    let parsed = serde_json::from_slice::<Value>(body).ok();
    let error = parsed.as_ref().and_then(|value| value.get("error"));
    let status_code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let (default_type, default_code) = openai_responses_error_fields(status_code);
    let code = sanitize_openai_responses_error_code(
        error.and_then(|value| value.get("code")),
        default_code,
    );
    let param = sanitize_openai_responses_error_param(error.and_then(|value| value.get("param")));
    ProtocolError {
        code: code.as_str().unwrap_or(default_type).to_string(),
        message: "Upstream request failed".into(),
        param: param.as_str().map(str::to_string),
        sequence_number: 0,
        lane,
    }
}

struct SseJsonDecoder {
    buffer: Vec<u8>,
    data_lines: Vec<String>,
    data_bytes: usize,
    max_line_bytes: usize,
    max_event_bytes: usize,
}

impl Default for SseJsonDecoder {
    fn default() -> Self {
        Self::with_limits(MAX_SSE_LINE_BYTES, MAX_SSE_EVENT_BYTES)
    }
}

impl SseJsonDecoder {
    fn with_limits(max_line_bytes: usize, max_event_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            data_lines: Vec::new(),
            data_bytes: 0,
            max_line_bytes,
            max_event_bytes,
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>, ProtocolError> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buffer.drain(..=position).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.handle_line(&line, &mut events)?;
        }
        if self.buffer.len() > self.max_line_bytes {
            return Err(ProtocolError::server(
                "Responses stream line exceeded the configured size limit.",
                None,
            ));
        }
        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<Value>, ProtocolError> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.handle_line(&line, &mut events)?;
        }
        self.flush(&mut events)?;
        Ok(events)
    }

    fn handle_line(&mut self, line: &[u8], events: &mut Vec<Value>) -> Result<(), ProtocolError> {
        if line.len() > self.max_line_bytes {
            return Err(ProtocolError::server(
                "Responses stream line exceeded the configured size limit.",
                None,
            ));
        }
        let line = std::str::from_utf8(line).map_err(|_| {
            ProtocolError::server("Responses stream contained invalid UTF-8.", None)
        })?;
        if line.is_empty() {
            return self.flush(events);
        }
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.strip_prefix(' ').unwrap_or(data);
            let separator_bytes = usize::from(!self.data_lines.is_empty());
            let next_bytes = self
                .data_bytes
                .checked_add(separator_bytes)
                .and_then(|bytes| bytes.checked_add(data.len()))
                .ok_or_else(|| {
                    ProtocolError::server("Responses stream event size overflowed.", None)
                })?;
            if next_bytes > self.max_event_bytes {
                return Err(ProtocolError::server(
                    "Responses stream event exceeded the configured size limit.",
                    None,
                ));
            }
            self.data_bytes = next_bytes;
            self.data_lines.push(data.to_string());
        }
        Ok(())
    }

    fn flush(&mut self, events: &mut Vec<Value>) -> Result<(), ProtocolError> {
        if self.data_lines.is_empty() {
            return Ok(());
        }
        let data = self.data_lines.join("\n");
        self.data_lines.clear();
        self.data_bytes = 0;
        if data.trim() == "[DONE]" {
            return Ok(());
        }
        let event = serde_json::from_str(&data).map_err(|_| {
            ProtocolError::server("Responses stream contained an invalid JSON event.", None)
        })?;
        events.push(event);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use keycompute_auth::Permission;
    use tokio_tungstenite::{connect_async, tungstenite::Message as ClientMessage};

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OfficialResponseErrorEvent {
        code: String,
        message: String,
        param: Option<String>,
        sequence_number: u64,
        #[serde(rename = "type")]
        event_type: String,
        #[serde(default)]
        stream_id: Option<String>,
    }

    struct FailingMessageSink;

    impl Sink<Message> for FailingMessageSink {
        type Error = std::io::Error;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
            std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "injected writer failure",
            )))
        }

        fn start_send(
            self: std::pin::Pin<&mut Self>,
            _item: Message,
        ) -> std::result::Result<(), Self::Error> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "injected writer failure",
            ))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn validates_official_stream_id_grammar() {
        assert!(valid_stream_id("planner_1.alpha-beta"));
        assert!(!valid_stream_id(""));
        assert!(!valid_stream_id("contains space"));
        assert!(!valid_stream_id("含中文"));
        assert!(!valid_stream_id(&"a".repeat(257)));
    }

    #[test]
    fn response_create_uses_default_or_named_lane() {
        let default = parse_response_create(r#"{"type":"response.create","model":"gpt"}"#).unwrap();
        assert_eq!(default.lane, None);
        let named =
            parse_response_create(r#"{"type":"response.create","stream_id":"main","model":"gpt"}"#)
                .unwrap();
        assert_eq!(named.lane.as_deref(), Some("main"));
    }

    #[test]
    fn websocket_rejects_unsupported_control_events() {
        for payload in [
            r#"{"type":"response.steer","previous_response_id":"resp_123","input":"change direction"}"#,
            r#"{"type":"response.inject","response_id":"resp_123","input":[]}"#,
        ] {
            let event = parse_response_create(payload).unwrap_err().event();

            assert_eq!(event["type"], "error");
            assert_eq!(event["code"], "invalid_event");
            assert_eq!(event["param"], "type");
            assert_eq!(event["sequence_number"], 0);
            assert!(
                event["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("response.create"))
            );
        }
    }

    #[test]
    fn invalid_stream_id_error_matches_official_shape() {
        let error = parse_response_create(
            r#"{"type":"response.create","stream_id":"bad stream","model":"gpt"}"#,
        )
        .unwrap_err();
        let event = error.event();
        assert_eq!(event["type"], "error");
        assert_eq!(event["code"], "invalid_stream_id");
        assert_eq!(event["sequence_number"], 0);
        assert!(event.get("stream_id").is_none());

        let null_error =
            parse_response_create(r#"{"type":"response.create","stream_id":null,"model":"gpt"}"#)
                .unwrap_err();
        assert_eq!(null_error.event()["code"], "invalid_stream_id");
    }

    #[test]
    fn accepts_and_strips_official_sdk_transport_fields() {
        let mut official_sdk_frame = json!({
            "type": "response.create",
            "stream": true,
            "background": false,
            "stream_id": "main",
            "model": "gpt-test",
            "input": "hello",
        });
        validate_transport_fields(&mut official_sdk_frame, Some("main".to_string())).unwrap();
        assert!(official_sdk_frame.get("stream").is_none());
        assert!(official_sdk_frame.get("background").is_none());

        let mut nullable_fields = json!({
            "type": "response.create",
            "stream": null,
            "background": null,
            "store": null,
        });
        validate_transport_fields(&mut nullable_fields, None).unwrap();
        assert!(nullable_fields.get("stream").is_none());
        assert!(nullable_fields.get("background").is_none());
        assert!(nullable_fields["store"].is_null());

        let mut invalid_stream = json!({"type":"response.create","stream":"true"});
        let error = validate_transport_fields(&mut invalid_stream, None).unwrap_err();
        assert_eq!(error.param.as_deref(), Some("stream"));

        let mut invalid_store = json!({"type":"response.create","store":"false"});
        let invalid_store = validate_transport_fields(&mut invalid_store, None).unwrap_err();
        assert_eq!(invalid_store.param.as_deref(), Some("store"));

        let mut invalid_previous = json!({"type":"response.create","previous_response_id":42});
        let invalid_previous = validate_transport_fields(&mut invalid_previous, None).unwrap_err();
        assert_eq!(
            invalid_previous.param.as_deref(),
            Some("previous_response_id")
        );
    }

    #[test]
    fn generate_false_rejects_malformed_known_responses_fields() {
        for (field, value) in [
            ("input", json!(42)),
            ("tools", json!({"type": "function"})),
            ("max_output_tokens", json!(15)),
        ] {
            let mut body = json!({
                "type": "response.create",
                "model": "gpt-test",
                "generate": false,
                "future_field": {"preserved": true},
            });
            body.as_object_mut()
                .unwrap()
                .insert(field.to_string(), value);
            validate_transport_fields(&mut body, None).unwrap();

            let error = validate_responses_request(&body).unwrap_err();
            let ApiError::BadRequest(message) = error else {
                panic!("expected invalid Responses field error");
            };
            assert!(message.starts_with(field));
            assert_eq!(body["future_field"]["preserved"], true);
        }
    }

    #[test]
    fn generate_false_accepts_known_create_shapes_and_future_fields() {
        let mut body = json!({
            "type": "response.create",
            "model": "gpt-test",
            "generate": false,
            "store": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}]
            }],
            "instructions": "be concise",
            "tools": [{"type": "function", "name": "lookup", "parameters": {}}],
            "tool_choice": "auto",
            "metadata": {"request": "warmup"},
            "reasoning": {"effort": "high"},
            "text": {"format": {"type": "text"}},
            "max_output_tokens": 16,
            "future_field": {"preserved": true},
        });
        validate_transport_fields(&mut body, None).unwrap();
        validate_responses_request(&body).unwrap();

        assert_eq!(body["future_field"]["preserved"], true);
    }

    #[test]
    fn evicted_local_parent_returns_official_not_found_error() {
        let error =
            missing_local_previous_response_error("resp_ws_evicted", Some("main".to_string()))
                .unwrap();
        let event = error.event();
        assert_eq!(event["code"], "previous_response_not_found");
        assert_eq!(event["param"], "previous_response_id");
        assert_eq!(event["sequence_number"], 0);
        assert_eq!(event["stream_id"], "main");
        assert!(missing_local_previous_response_error("resp_upstream", None).is_none());
    }

    #[test]
    fn sse_decoder_handles_fragmented_multiline_frames() {
        let mut decoder = SseJsonDecoder::default();
        assert!(
            decoder
                .push(b"event: response.created\nda")
                .unwrap()
                .is_empty()
        );
        let events = decoder
            .push(b"ta: {\"type\":\"response.created\",\n")
            .unwrap();
        assert!(events.is_empty());
        let events = decoder.push(b"data: \"sequence_number\":0}\n\n").unwrap();
        assert_eq!(events[0]["type"], "response.created");
    }

    #[test]
    fn sse_decoder_bounds_partial_lines_and_multiline_events() {
        let mut line_decoder = SseJsonDecoder::with_limits(8, 32);
        let line_error = line_decoder.push(b"data: 123").unwrap_err();
        assert!(line_error.message.contains("line exceeded"));

        let mut event_decoder = SseJsonDecoder::with_limits(32, 5);
        event_decoder.push(b"data: 123\n").unwrap();
        let event_error = event_decoder.push(b"data: 45\n").unwrap_err();
        assert!(event_error.message.contains("event exceeded"));
    }

    #[test]
    fn json_preflight_accounts_for_container_allocation_overhead() {
        let string_payload = r#"{"input":"0,0,0,0"}"#;
        let array_payload = r#"{"input":[0,0,0,0]}"#;

        assert!(estimated_json_parse_working_set_bytes(string_payload.as_bytes()) < 256);
        assert!(
            estimated_json_parse_working_set_bytes(array_payload.as_bytes())
                > array_payload.len() + 256
        );
    }

    #[test]
    fn request_working_set_counts_wire_and_decoded_json_simultaneously() {
        let payload = r#"{"type":"response.create","input":"large payload"}"#;
        let body: Value = serde_json::from_str(payload).unwrap();

        let preparse = preparse_request_working_set_bytes(payload);
        let parsed = parsed_request_working_set_bytes(payload.len(), &body);

        assert!(preparse >= payload.len().saturating_mul(2));
        assert_eq!(parsed, payload.len() + estimated_json_bytes(&body));
        assert!(parsed > payload.len());
    }

    #[tokio::test]
    async fn outbound_queue_rejects_an_event_larger_than_its_byte_budget() {
        let (outbound, _receiver) = OutboundSender::with_byte_limit(2, 4);

        assert!(
            outbound
                .send(Message::Text("12345".into()), 5)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn writer_failure_stops_connection_supervisor() {
        let source = futures::stream::iter(vec![Ok::<_, std::io::Error>(Message::Ping(
            Vec::new().into(),
        ))])
        .chain(futures::stream::pending());

        tokio::time::timeout(
            Duration::from_secs(1),
            serve_connection_parts(
                FailingMessageSink,
                source,
                AppState::new(),
                HeaderMap::new(),
            ),
        )
        .await
        .expect("writer failure must terminate the connection supervisor");
    }

    #[tokio::test]
    async fn outbound_json_budget_uses_the_serialized_wire_size() {
        let event = json!({"value": "\u{0000}".repeat(100)});
        let raw_string_bytes = event["value"].as_str().unwrap().len();
        let wire_bytes = serde_json::to_string(&event).unwrap().len();
        assert!(wire_bytes > raw_string_bytes + 300);
        let (outbound, _receiver) = OutboundSender::with_byte_limit(2, raw_string_bytes + 300);

        let error = send_json(&outbound, event).await.unwrap_err();

        assert_eq!(error.code, "server_error");
        assert!(error.message.contains("not accepting"));
    }

    #[test]
    fn outbound_budget_includes_the_largest_stream_id_envelope() {
        let mut event = json!({
            "type": "response.output_text.delta",
            "delta": "x",
        });
        let base_bytes = serde_json::to_string(&event).unwrap().len();
        attach_stream_id(&mut event, &Some("s".repeat(MAX_STREAM_ID_BYTES)));
        let named_bytes = serde_json::to_string(&event).unwrap().len();
        let envelope_bytes = named_bytes - base_bytes;

        assert!(
            MAX_SSE_EVENT_BYTES + envelope_bytes <= MAX_RESIDENT_OUTBOUND_BYTES,
            "a valid maximum-sized SSE event must remain sendable after stream_id is attached"
        );
    }

    #[test]
    fn warmup_state_is_applied_once_and_explicit_fields_win() {
        let warmup = json!({
            "type": "response.create",
            "stream_id": "main",
            "generate": false,
            "model": "gpt-test",
            "store": false,
            "input": "cached message",
            "instructions": "cached instructions",
            "tools": [{"type":"function","name":"lookup","parameters":{}}],
            "reasoning": {"effort":"high"}
        });
        let state = warmup_request_state(&warmup);
        assert_eq!(state["instructions"], "cached instructions");
        assert!(state.get("input").is_none());
        assert!(state.get("model").is_none());
        assert!(state.get("store").is_none());

        let mut generated = json!({
            "type": "response.create",
            "model": "gpt-test",
            "previous_response_id": "resp_ws_parent",
            "input": "new message",
            "reasoning": {"effort":"low"}
        });
        apply_warmup_request_state(&mut generated, &state);
        assert_eq!(generated["instructions"], "cached instructions");
        assert_eq!(generated["tools"][0]["name"], "lookup");
        assert_eq!(generated["reasoning"]["effort"], "low");
        assert_eq!(generated["input"], "new message");
    }

    #[test]
    fn stored_warmup_routes_around_every_unpersisted_replayed_parent() {
        let body = json!({
            "type": "response.create",
            "model": "gpt-test",
            "store": true,
            "generate": false,
            "previous_response_id": "resp_ws_parent",
            "input": "next"
        });

        let mut without_upstream_parent = body.clone();
        prepare_warmup_persistence_request_body(&mut without_upstream_parent, true, false, None);
        assert!(
            without_upstream_parent
                .get("previous_response_id")
                .is_none()
        );

        let mut with_upstream_parent = body.clone();
        with_upstream_parent["previous_response_id"] = Value::String("resp_store_false".into());
        prepare_warmup_persistence_request_body(
            &mut with_upstream_parent,
            true,
            false,
            Some("resp_upstream_parent"),
        );
        assert_eq!(
            with_upstream_parent["previous_response_id"],
            "resp_upstream_parent"
        );

        let mut persisted_parent = body;
        prepare_warmup_persistence_request_body(&mut persisted_parent, true, true, None);
        assert_eq!(persisted_parent["previous_response_id"], "resp_ws_parent");

        let mut stored_upstream_parent = json!({
            "previous_response_id": "resp_upstream_stored"
        });
        prepare_warmup_persistence_request_body(
            &mut stored_upstream_parent,
            false,
            false,
            Some("resp_upstream_stored"),
        );
        assert_eq!(
            stored_upstream_parent["previous_response_id"],
            "resp_upstream_stored"
        );
    }

    #[test]
    fn websocket_events_do_not_reuse_handshake_request_identity_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer test".parse().unwrap());
        headers.insert("openai-beta", "responses=v1".parse().unwrap());
        headers.insert("idempotency-key", "handshake-key".parse().unwrap());
        headers.insert("x-client-request-id", "handshake-request".parse().unwrap());

        let event_headers = websocket_event_headers(&headers);
        assert!(event_headers.get("idempotency-key").is_none());
        assert!(event_headers.get("x-client-request-id").is_none());
        assert_eq!(event_headers["authorization"], "Bearer test");
        assert_eq!(event_headers["openai-beta"], "responses=v1");
        assert_eq!(headers["idempotency-key"], "handshake-key");
        assert_eq!(headers["x-client-request-id"], "handshake-request");
    }

    #[tokio::test]
    async fn warmup_emits_chainable_response_id_and_named_stream() {
        let (tx, mut rx) = OutboundSender::new(4);
        let response = warmup_response_object("resp_ws_test", Some("gpt-test"), false, None);
        emit_warmup_events(&tx, &Some("main".to_string()), &response)
            .await
            .unwrap();
        let mut events = Vec::new();
        for _ in 0..3 {
            let Message::Text(text) = rx.recv().await.unwrap().message else {
                panic!("expected JSON text event");
            };
            events.push(serde_json::from_str::<Value>(text.as_str()).unwrap());
        }
        assert_eq!(events[0]["type"], "response.created");
        assert_eq!(events[2]["type"], "response.completed");
        assert_eq!(events[2]["stream_id"], "main");
        assert_eq!(events[2]["response"]["id"], "resp_ws_test");
        assert_eq!(events[2]["response"]["status"], "completed");
        assert_eq!(events[2]["response"], response);
    }

    #[test]
    fn server_errors_use_the_official_flat_event_shape() {
        let event =
            ProtocolError::server("Internal server error", Some("main".to_string())).event();
        assert_eq!(event["type"], "error");
        assert_eq!(event["code"], "server_error");
        assert_eq!(event["message"], "Internal server error");
        assert!(event["param"].is_null());
        assert_eq!(event["sequence_number"], 0);
        assert_eq!(event["stream_id"], "main");
        assert!(event.get("status").is_none());
        assert!(event.get("error").is_none());

        let decoded: OfficialResponseErrorEvent = serde_json::from_value(event).unwrap();
        assert_eq!(decoded.event_type, "error");
        assert_eq!(decoded.code, "server_error");
        assert_eq!(decoded.message, "Internal server error");
        assert_eq!(decoded.param, None);
        assert_eq!(decoded.sequence_number, 0);
        assert_eq!(decoded.stream_id.as_deref(), Some("main"));
    }

    #[test]
    fn decoder_failures_are_server_errors() {
        let mut decoder = SseJsonDecoder::default();
        let error = decoder
            .push(b"data: not-json\n\n")
            .unwrap_err()
            .with_lane(Some("main".to_string()));
        let event = error.event();
        assert_eq!(event["code"], "server_error");
        assert_eq!(event["sequence_number"], 0);
        assert_eq!(event["stream_id"], "main");
    }

    #[test]
    fn streaming_error_is_normalized_to_official_websocket_shape() {
        let mut event = json!({
            "type": "error",
            "code": "server_error",
            "message": "Upstream request failed",
            "param": null,
            "sequence_number": 7,
            "debug": "token=secret",
        });
        normalize_websocket_error_event(&mut event);
        assert_eq!(event["code"], "server_error");
        assert_eq!(event["message"], "Upstream request failed");
        assert!(event["param"].is_null());
        assert_eq!(event["sequence_number"], 7);
        assert!(event.get("debug").is_none());
        assert!(event.get("status").is_none());
        assert!(event.get("error").is_none());
        assert_eq!(event.as_object().unwrap().len(), 5);
    }

    #[test]
    fn nested_websocket_error_is_rebuilt_without_untrusted_fields() {
        let mut event = json!({
            "type": "error",
            "status": 429,
            "sequence_number": 11,
            "details": {"request": "private prompt"},
            "error": {
                "type": "rate_limit_error",
                "code": "rate_limit_exceeded",
                "message": "provider token=secret",
                "param": "model",
                "debug": "internal-stack"
            }
        });

        normalize_websocket_error_event(&mut event);

        assert_eq!(event["code"], "rate_limit_exceeded");
        assert_eq!(event["message"], "Upstream request failed");
        assert_eq!(event["param"], "model");
        assert_eq!(event["sequence_number"], 11);
        assert_eq!(event.as_object().unwrap().len(), 5);
        let encoded = event.to_string();
        assert!(!encoded.contains("private prompt"));
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("internal-stack"));
    }

    #[test]
    fn websocket_error_rejects_untrusted_classification_strings() {
        let mut event = json!({
            "type": "error",
            "status": 429,
            "error": {
                "type": "credential_sk_live_secret",
                "code": "sk_live_secret",
                "message": "failed",
                "param": "authorization_token"
            }
        });

        normalize_websocket_error_event(&mut event);

        assert_eq!(event["code"], "rate_limit_exceeded");
        assert!(event["param"].is_null());
        assert!(!event.to_string().contains("secret"));
        assert!(!event.to_string().contains("authorization_token"));

        let error = protocol_error_from_http(
            429,
            br#"{"error":{"type":"credential_sk_live_secret","code":"sk_live_secret","message":"failed","param":"authorization_token"}}"#,
            None,
        );
        let event = error.event();
        assert_eq!(event["code"], "rate_limit_exceeded");
        assert!(event["param"].is_null());
        assert!(!event.to_string().contains("secret"));
    }

    #[test]
    fn upstream_http_error_keeps_codes_but_redacts_websocket_message() {
        let error = protocol_error_from_http(
            429,
            br#"{"error":{"type":"rate_limit_error","code":"rate_limit_exceeded","message":"slow down","param":"model"}}"#,
            Some("main".to_string()),
        );
        let event = error.event();
        assert_eq!(event["code"], "rate_limit_exceeded");
        assert_eq!(event["message"], "Upstream request failed");
        assert_eq!(event["param"], "model");
        assert_eq!(event["sequence_number"], 0);
        assert_eq!(event["stream_id"], "main");
    }

    #[tokio::test]
    async fn upstream_http_error_evicts_only_a_same_lane_parent() {
        let parent_lane = Some("main".to_string());
        let cache = Arc::new(Mutex::new(ConnectionCache::default()));
        cache.lock().await.insert(
            "resp_same_lane".to_string(),
            CachedResponse::new(
                None,
                Vec::new(),
                serde_json::Map::new(),
                false,
                None,
                parent_lane.clone(),
            ),
        );
        cache.lock().await.insert(
            "resp_cross_lane".to_string(),
            CachedResponse::new(
                None,
                Vec::new(),
                serde_json::Map::new(),
                false,
                None,
                parent_lane.clone(),
            ),
        );

        let same_lane_error = require_successful_http_response(
            (axum::http::StatusCode::BAD_REQUEST, "bad request").into_response(),
            &parent_lane,
            &cache,
            Some("resp_same_lane"),
        )
        .await
        .unwrap_err();
        assert_eq!(same_lane_error.code, "invalid_request_error");
        assert!(cache.lock().await.get("resp_same_lane").is_none());

        let cross_lane_error = require_successful_http_response(
            (axum::http::StatusCode::BAD_REQUEST, "bad request").into_response(),
            &Some("critic".to_string()),
            &cache,
            Some("resp_cross_lane"),
        )
        .await
        .unwrap_err();
        assert_eq!(cross_lane_error.code, "invalid_request_error");
        assert!(cache.lock().await.get("resp_cross_lane").is_some());
    }

    #[test]
    fn maintenance_error_is_a_lane_scoped_service_unavailable_event() {
        let event = api_error(
            ApiError::ServiceUnavailable("Planned maintenance".to_string()),
            Some("main".to_string()),
        )
        .event();

        assert_eq!(event["code"], "service_unavailable");
        assert_eq!(event["message"], "Planned maintenance");
        assert_eq!(event["sequence_number"], 0);
        assert_eq!(event["stream_id"], "main");
    }

    #[test]
    fn cache_is_bounded_and_same_lane_errors_evict_only_parent_lane() {
        let mut cache = ConnectionCache::default();
        for index in 0..=MAX_CACHED_RESPONSES {
            cache.insert(
                format!("resp_{index}"),
                CachedResponse::new(
                    None,
                    Vec::new(),
                    serde_json::Map::new(),
                    false,
                    None,
                    Some("main".to_string()),
                ),
            );
        }
        assert_eq!(cache.responses.len(), MAX_CACHED_RESPONSES);
        assert!(cache.get("resp_0").is_none());
        cache.evict_same_lane_parent(
            &format!("resp_{MAX_CACHED_RESPONSES}"),
            &Some("other".to_string()),
        );
        assert!(cache.get(&format!("resp_{MAX_CACHED_RESPONSES}")).is_some());
        cache.evict_same_lane_parent(
            &format!("resp_{MAX_CACHED_RESPONSES}"),
            &Some("main".to_string()),
        );
        assert!(cache.get(&format!("resp_{MAX_CACHED_RESPONSES}")).is_none());
    }

    #[test]
    fn cached_parent_snapshot_survives_a_same_lane_replacement() {
        let lane = Some("main".to_string());
        let mut cache = ConnectionCache::default();
        let parent = CachedResponse::new(
            Some("gpt-test".to_string()),
            vec![json!({"type":"message","content":"parent context"})],
            serde_json::Map::new(),
            false,
            None,
            lane.clone(),
        );
        let expected_budget = parent.estimated_bytes.saturating_mul(2);
        assert!(cache.insert("resp_parent".to_string(), parent));

        let (snapshot, budget) = snapshot_cached_parent(&cache, Some("resp_parent"), 2);
        let replacement = CachedResponse::new(
            Some("gpt-test".to_string()),
            vec![json!({"type":"message","content":"replacement"})],
            serde_json::Map::new(),
            false,
            None,
            lane,
        );
        assert!(cache.replace_lane("resp_replacement".to_string(), replacement));

        assert!(cache.get("resp_parent").is_none());
        let snapshot = snapshot.expect("the continuation must retain its parent snapshot");
        assert_eq!(snapshot.context_items[0]["content"], "parent context");
        assert_eq!(budget, Some(expected_budget));
    }

    #[test]
    fn cache_enforces_bytes_keeps_latest_lane_and_drops_stored_context() {
        let mut cache = ConnectionCache::default();
        let first = CachedResponse::new(
            None,
            vec![Value::String("a".repeat(64))],
            serde_json::Map::new(),
            false,
            None,
            Some("main".to_string()),
        );
        let second = CachedResponse::new(
            None,
            vec![Value::String("b".repeat(64))],
            serde_json::Map::new(),
            false,
            None,
            Some("critic".to_string()),
        );
        let byte_limit = cache_entry_bytes("resp_first", &first)
            .saturating_add(cache_entry_bytes("resp_second", &second))
            .saturating_sub(1);
        assert!(cache.insert_with_limits("resp_first".to_string(), first, 8, byte_limit));
        assert!(cache.insert_with_limits("resp_second".to_string(), second, 8, byte_limit));
        assert!(cache.get("resp_first").is_none());
        assert!(cache.get("resp_second").is_some());
        assert!(cache.estimated_bytes <= byte_limit);

        cache.insert(
            "resp_main_old".to_string(),
            CachedResponse::new(
                None,
                Vec::new(),
                serde_json::Map::new(),
                false,
                None,
                Some("main".to_string()),
            ),
        );
        cache.evict_lane(&Some("main".to_string()));
        assert!(cache.get("resp_main_old").is_none());
        assert!(cache.get("resp_second").is_some());

        let stored = CachedResponse::terminal(
            None,
            vec![Value::String("large persisted context".to_string())],
            true,
            Some("resp_parent".to_string()),
            Some("main".to_string()),
        );
        assert!(stored.context_items.is_empty());
        assert!(stored.upstream_previous_response_id.is_none());
    }

    #[test]
    fn oversized_lane_replacement_preserves_retryable_parent() {
        let lane = Some("main".to_string());
        let mut cache = ConnectionCache::default();
        let parent = CachedResponse::new(
            None,
            vec![Value::String("parent".to_string())],
            serde_json::Map::new(),
            false,
            None,
            lane.clone(),
        );
        assert!(cache.insert("resp_parent".to_string(), parent));
        let replacement = CachedResponse::new(
            None,
            vec![Value::String("replacement is larger".to_string())],
            serde_json::Map::new(),
            false,
            None,
            lane.clone(),
        );
        let byte_limit = cache_entry_bytes("resp_replacement", &replacement).saturating_sub(1);

        assert!(!cache.replace_lane_with_limits(
            "resp_replacement".to_string(),
            replacement,
            8,
            byte_limit,
        ));
        assert!(cache.get("resp_parent").is_some());
        assert!(cache.get("resp_replacement").is_none());

        let event = response_cache_capacity_protocol_error(lane).event();
        assert_eq!(event["code"], "response_cache_capacity_exceeded");
        assert_eq!(event["sequence_number"], 0);
    }

    #[test]
    fn request_budget_saturation_fails_without_blocking_the_socket_reader() {
        let count = Arc::new(Semaphore::new(1));
        let bytes = Arc::new(Semaphore::new(10));
        let first = try_reserve_request_budget(&count, &bytes, 10, 10).unwrap();
        assert!(matches!(
            try_reserve_request_budget(&count, &bytes, 10, 1),
            Err(RequestBudgetExtensionError::ConnectionCapacity)
        ));

        let lane = response_create_lane_hint(
            r#"{"type":"response.create","stream_id":"research","input":"queued"}"#,
        );
        let event =
            request_budget_protocol_error(RequestBudgetExtensionError::ConnectionCapacity, lane)
                .event();
        assert_eq!(event["stream_id"], "research");
        assert_eq!(event["code"], "websocket_request_capacity_exceeded");

        drop(first);
        assert!(try_reserve_request_budget(&count, &bytes, 10, 1).is_ok());

        let count = Arc::new(Semaphore::new(2));
        let bytes = Arc::new(Semaphore::new(10));
        let first = try_reserve_request_budget(&count, &bytes, 10, 10).unwrap();
        assert!(matches!(
            try_reserve_request_budget(&count, &bytes, 10, 1),
            Err(RequestBudgetExtensionError::ConnectionCapacity)
        ));
        drop(first);
    }

    #[test]
    fn request_budget_rejects_a_working_set_above_its_hard_limit() {
        let count = Arc::new(Semaphore::new(2));
        let bytes = Arc::new(Semaphore::new(10));

        assert!(matches!(
            try_reserve_request_budget(&count, &bytes, 10, 11),
            Err(RequestBudgetExtensionError::HardLimit)
        ));

        let mut budget = try_reserve_request_budget(&count, &bytes, 10, 6).unwrap();
        assert_eq!(
            try_extend_request_budget(&mut budget, 5),
            Err(RequestBudgetExtensionError::HardLimit)
        );
        assert_eq!(budget.reserved_bytes, 6);
    }

    #[test]
    fn request_budget_extension_never_waits_while_holding_initial_capacity() {
        let count = Arc::new(Semaphore::new(2));
        let bytes = Arc::new(Semaphore::new(10));
        let mut first = try_reserve_request_budget(&count, &bytes, 10, 4).unwrap();
        let second = try_reserve_request_budget(&count, &bytes, 10, 4).unwrap();

        assert_eq!(
            try_extend_request_budget(&mut first, 3),
            Err(RequestBudgetExtensionError::ConnectionCapacity)
        );
        assert_eq!(first.reserved_bytes, 4);

        drop(second);
        assert_eq!(try_extend_request_budget(&mut first, 3), Ok(()));
        assert_eq!(first.reserved_bytes, 7);
    }

    #[test]
    fn generated_context_is_budgeted_until_upstream_confirms_storage() {
        assert_eq!(continuation_context_copy_count(true, true), 2);
        assert_eq!(continuation_context_copy_count(true, false), 2);
        assert_eq!(continuation_context_copy_count(false, true), 2);
        assert_eq!(continuation_context_copy_count(false, false), 1);
    }

    #[test]
    fn store_false_replay_moves_current_input_before_cloning_context() {
        let mut body = json!({
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "large-current-input"}]
            }]
        });
        let original_text = body["input"][0]["content"][0]["text"].as_str().unwrap();
        let original_pointer = original_text.as_ptr();

        let input = take_normalized_input(&mut body);

        assert!(body.get("input").is_none());
        assert_eq!(
            input[0]["content"][0]["text"].as_str().unwrap().as_ptr(),
            original_pointer,
            "moving an array input must retain its nested string allocation"
        );
    }

    #[tokio::test]
    async fn forwarded_stream_error_evicts_same_lane_parent() {
        let lane = Some("main".to_string());
        let cache = Arc::new(Mutex::new(ConnectionCache::default()));
        cache.lock().await.insert(
            "resp_parent".to_string(),
            CachedResponse::new(
                None,
                Vec::new(),
                serde_json::Map::new(),
                false,
                None,
                lane.clone(),
            ),
        );
        let (outbound, mut events) = OutboundSender::new(2);

        forward_sse_body(
            Body::from(
                "data: {\"type\":\"error\",\"code\":\"server_error\",\"message\":\"boom\",\"param\":null}\n\n",
            ),
            &outbound,
            &lane,
            &cache,
            Vec::new(),
            false,
            Some("resp_parent"),
            false,
            None,
        )
        .await
        .unwrap();

        let Message::Text(event) = events.recv().await.unwrap().message else {
            panic!("expected WebSocket JSON event")
        };
        let event: Value = serde_json::from_str(event.as_str()).unwrap();
        assert_eq!(event["type"], "error");
        assert_eq!(event["code"], "server_error");
        assert_eq!(event["sequence_number"], 0);
        serde_json::from_value::<OfficialResponseErrorEvent>(event).unwrap();
        assert!(cache.lock().await.get("resp_parent").is_none());
    }

    #[tokio::test]
    async fn bridge_errors_continue_the_upstream_sequence() {
        let lane = Some("main".to_string());
        let cache = Arc::new(Mutex::new(ConnectionCache::default()));
        let (outbound, mut events) = OutboundSender::new(2);
        let body = Body::from_stream(tokio_stream::iter([
            Ok::<_, std::convert::Infallible>(bytes::Bytes::from_static(
                b"data: {\"type\":\"response.output_text.delta\",\"sequence_number\":4,\"delta\":\"hi\"}\n\n",
            )),
            Ok(bytes::Bytes::from_static(b"data: not-json\n\n")),
        ]));
        let error = forward_sse_body(
            body,
            &outbound,
            &lane,
            &cache,
            Vec::new(),
            false,
            None,
            false,
            None,
        )
        .await
        .unwrap_err();

        let Message::Text(first_event) = events.recv().await.unwrap().message else {
            panic!("expected the upstream event before the bridge error")
        };
        let first_event: Value = serde_json::from_str(first_event.as_str()).unwrap();
        assert_eq!(first_event["sequence_number"], 4);

        let error_event = error.event();
        assert_eq!(error_event["sequence_number"], 5);
        let decoded: OfficialResponseErrorEvent = serde_json::from_value(error_event).unwrap();
        assert_eq!(decoded.sequence_number, 5);
        assert_eq!(decoded.stream_id.as_deref(), Some("main"));
    }

    #[tokio::test]
    async fn upstream_store_false_retains_complete_generated_context() {
        let lane = Some("main".to_string());
        let cache = Arc::new(Mutex::new(ConnectionCache::default()));
        let context = vec![
            json!({"type": "message", "role": "user", "content": "parent input"}),
            json!({"type": "message", "role": "user", "content": "current input"}),
        ];
        let output = json!({
            "type": "message",
            "role": "assistant",
            "content": "current output"
        });
        let expected_context = context
            .iter()
            .cloned()
            .chain(std::iter::once(output.clone()))
            .collect::<Vec<_>>();
        let (outbound, mut events) = OutboundSender::new(2);
        let terminal = json!({
            "type": "response.completed",
            "response": {
                "id": "resp_zdr",
                "model": "gpt-test",
                "store": false,
                "output": [output]
            }
        });

        forward_sse_body(
            Body::from(format!("data: {terminal}\n\n")),
            &outbound,
            &lane,
            &cache,
            context,
            true,
            None,
            false,
            Some("resp_upstream_parent".to_string()),
        )
        .await
        .unwrap();

        let guard = cache.lock().await;
        let cached = guard.get("resp_zdr").expect("terminal response cached");
        assert!(!cached.stored);
        assert_eq!(cached.context_items, expected_context);
        assert_eq!(
            cached.upstream_previous_response_id.as_deref(),
            Some("resp_upstream_parent")
        );
        drop(guard);

        let Message::Text(event) = events.recv().await.unwrap().message else {
            panic!("expected terminal WebSocket JSON event")
        };
        let event: Value = serde_json::from_str(event.as_str()).unwrap();
        assert_eq!(event["response"]["store"], false);
    }

    #[tokio::test]
    async fn websocket_connection_rejects_controls_then_handles_response_create() {
        let state = AppState::new();
        let auth = AuthExtractor::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "user",
        )
        .with_permissions(vec![Permission::UseApi]);
        let jwt = keycompute_auth::JwtValidator::new("change-me-in-production", "keycompute")
            .generate_token(auth.user_id, auth.tenant_id, "user")
            .unwrap();
        let mut connection_headers = HeaderMap::new();
        connection_headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {jwt}").parse().unwrap(),
        );
        let app = Router::new().route(
            "/v1/responses",
            get(move |upgrade: WebSocketUpgrade| {
                let state = state.clone();
                let auth = auth.clone();
                let headers = connection_headers.clone();
                async move {
                    upgrade.on_upgrade(move |socket| serve_connection(socket, state, auth, headers))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let (mut client, _) = connect_async(format!("ws://{address}/v1/responses"))
            .await
            .unwrap();

        for unsupported in [
            json!({
                "type": "response.steer",
                "previous_response_id": "resp_123",
                "input": "change direction"
            }),
            json!({
                "type": "response.inject",
                "response_id": "resp_123",
                "input": []
            }),
        ] {
            client
                .send(ClientMessage::Text(unsupported.to_string().into()))
                .await
                .unwrap();
            let ClientMessage::Text(text) = client.next().await.unwrap().unwrap() else {
                panic!("expected unsupported-event error");
            };
            let event: Value = serde_json::from_str(text.as_str()).unwrap();
            assert_eq!(event["type"], "error");
            assert_eq!(event["code"], "invalid_event");
            assert_eq!(event["param"], "type");
            assert_eq!(event["sequence_number"], 0);
        }

        // Unsupported controls are request-scoped errors and must not poison
        // the connection for a subsequent supported event.
        client
            .send(ClientMessage::Text(
                json!({
                    "type": "response.create",
                    "stream": true,
                    "background": false,
                    "stream_id": "main",
                    "model": "gpt-test",
                    "store": false,
                    "generate": false,
                    "input": "warm this request"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        let mut events = Vec::new();
        for _ in 0..3 {
            let ClientMessage::Text(text) = client.next().await.unwrap().unwrap() else {
                panic!("expected JSON text event");
            };
            events.push(serde_json::from_str::<Value>(text.as_str()).unwrap());
        }
        assert_eq!(events[0]["type"], "response.created");
        assert_eq!(events[1]["type"], "response.in_progress");
        assert_eq!(events[2]["type"], "response.completed");
        assert_eq!(events[2]["stream_id"], "main");
        let warmup_response_id = events[2]["response"]["id"].as_str().unwrap().to_string();
        assert!(warmup_response_id.starts_with("resp_ws_"));

        // A different named lane can fork the current cached response.
        client
            .send(ClientMessage::Text(
                json!({
                    "type": "response.create",
                    "stream_id": "critic",
                    "model": "gpt-test",
                    "store": false,
                    "generate": false,
                    "previous_response_id": warmup_response_id,
                    "input": "fork"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut forked = Vec::new();
        for _ in 0..3 {
            let ClientMessage::Text(text) = client.next().await.unwrap().unwrap() else {
                panic!("expected JSON text event");
            };
            forked.push(serde_json::from_str::<Value>(text.as_str()).unwrap());
        }
        assert_eq!(forked[2]["type"], "response.completed");
        assert_eq!(forked[2]["stream_id"], "critic");

        // A request-scoped validation failure occurs before a child response
        // is accepted and must not consume the same-lane stateless parent.
        client
            .send(ClientMessage::Text(
                json!({
                    "type": "response.create",
                    "stream_id": "main",
                    "model": "gpt-test",
                    "store": "not-a-boolean",
                    "generate": false,
                    "previous_response_id": events[2]["response"]["id"],
                    "input": "invalid continuation"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let ClientMessage::Text(text) = client.next().await.unwrap().unwrap() else {
            panic!("expected validation error event");
        };
        let validation_error: Value = serde_json::from_str(text.as_str()).unwrap();
        assert_eq!(validation_error["type"], "error");
        assert_eq!(validation_error["stream_id"], "main");
        assert_eq!(validation_error["code"], "invalid_request_error");
        assert_eq!(validation_error["sequence_number"], 0);

        // Same-lane continuation advances the lane and evicts its old parent.
        client
            .send(ClientMessage::Text(
                json!({
                    "type": "response.create",
                    "stream_id": "main",
                    "model": "gpt-test",
                    "store": false,
                    "generate": false,
                    "previous_response_id": events[2]["response"]["id"],
                    "input": "continue"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut continued = Vec::new();
        for _ in 0..3 {
            let ClientMessage::Text(text) = client.next().await.unwrap().unwrap() else {
                panic!("expected JSON text event");
            };
            continued.push(serde_json::from_str::<Value>(text.as_str()).unwrap());
        }
        assert_eq!(continued[2]["type"], "response.completed");
        assert_eq!(continued[2]["stream_id"], "main");
        assert_eq!(
            continued[2]["response"]["previous_response_id"],
            events[2]["response"]["id"]
        );

        client.close(None).await.unwrap();
        server.abort();
    }
}
