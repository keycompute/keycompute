//! Platform-managed Responses state; upstream executors remain stateless.
mod events;
pub(crate) mod execution;
pub(crate) mod maintenance;
pub(crate) use execution::cancel_execution;
mod resources;
pub(crate) mod store;
use crate::{
    error::{ApiError, Result},
    extractors::{AuthExtractor, ClientRequestId, RequestId, RequestReceivedAt},
    state::{AppState, GenerationHttpBodyPermit},
};
use axum::{
    Json,
    body::Body,
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use execution::ManagedExecution;
use keycompute_auth::Permission;
use keycompute_types::{ModelAccessMode, node_native::NodeNativeOperation};
use resources::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use store::Scope;

fn boolean(body: &Value, name: &str, default: bool) -> Result<bool> {
    match body.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(v)) => Ok(*v),
        _ => Err(ApiError::BadRequest(format!(
            "{name} must be a boolean or null"
        ))),
    }
}
fn reference(body: &Value, name: &str) -> Result<Option<String>> {
    let value = match body.get(name) {
        None | Some(Value::Null) => return Ok(None),
        Some(v) => v,
    };
    let id = if name == "conversation" && value.is_object() {
        if value.as_object().is_some_and(|o| o.len() != 1) {
            return Err(ApiError::BadRequest(
                "conversation reference accepts only id".into(),
            ));
        }
        value.get("id").and_then(Value::as_str)
    } else {
        value.as_str()
    };
    id.filter(|id| !id.is_empty() && id.len() <= 200 && !id.chars().any(char::is_control))
        .map(|id| Some(id.to_owned()))
        .ok_or_else(|| ApiError::BadRequest(format!("{name} must identify a valid resource")))
}
pub(crate) fn require_api(auth: &AuthExtractor) -> Result<()> {
    if !auth.has_permission(&Permission::UseApi) {
        return Err(ApiError::Forbidden("API-use permission is required".into()));
    }
    Ok(())
}
struct Invocation {
    state: AppState,
    auth: AuthExtractor,
    request_id: RequestId,
    client_request_id: ClientRequestId,
    received: RequestReceivedAt,
    headers: HeaderMap,
    permit: Option<GenerationHttpBodyPermit>,
    body: Value,
    mode: ModelAccessMode,
}
impl Invocation {
    async fn execute(self, managed: Option<Arc<ManagedExecution>>) -> Result<Response> {
        crate::handlers::scoped_native::generate_with_state(
            self.state,
            self.auth,
            self.request_id,
            self.client_request_id,
            self.received,
            self.headers,
            self.permit,
            self.body,
            self.mode,
            NodeNativeOperation::Responses,
            managed,
        )
        .await
    }
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn create(
    state: AppState,
    mut auth: AuthExtractor,
    request_id: RequestId,
    client_request_id: ClientRequestId,
    received: RequestReceivedAt,
    mut headers: HeaderMap,
    permit: Option<GenerationHttpBodyPermit>,
    body: Value,
    mode: ModelAccessMode,
) -> Result<Response> {
    require_api(&auth)?;
    crate::handlers::scoped_native::validate_managed_responses_body(&body)?;
    if !body.is_object() {
        return Err(ApiError::BadRequest("Request must be a JSON object".into()));
    }
    let previous = reference(&body, "previous_response_id")?;
    let conversation = reference(&body, "conversation")?;
    if previous.is_some() && conversation.is_some() {
        return Err(ApiError::BadRequest(
            "previous_response_id and conversation are mutually exclusive".into(),
        ));
    }
    let background = boolean(&body, "background", false)?;
    let streaming = boolean(&body, "stream", false)?;
    let retain = boolean(&body, "store", !background)?;
    if !background && !retain && previous.is_none() && conversation.is_none() {
        return Invocation {
            state,
            auth,
            request_id,
            client_request_id,
            received,
            headers,
            permit,
            body,
            mode,
        }
        .execute(None)
        .await;
    }
    store::metadata(body.get("metadata"))?;
    crate::admission::ensure_generation(&state, &mut auth).await?;
    let scope = Scope::new(&auth, mode)?;
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Platform state is unavailable".into()))?;
    let encoded = serde_json::to_vec(&body)
        .map_err(|_| ApiError::BadRequest("Invalid request JSON".into()))?;
    if encoded.len() > store::MAX_HISTORY_BYTES {
        return Err(ApiError::BadRequest("Managed request exceeds 2 MiB".into()));
    }
    let request_hash = format!("{:x}", Sha256::digest(&encoded));
    let idempotency_hash = if let Some(value) = headers.get("idempotency-key") {
        let key = value
            .to_str()
            .map_err(|_| ApiError::BadRequest("Invalid Idempotency-Key".into()))?;
        if key.is_empty() || key.len() > 256 || key.chars().any(char::is_control) {
            return Err(ApiError::BadRequest(
                "Idempotency-Key must be 1 to 256 printable characters".into(),
            ));
        }
        Some(format!("{:x}", Sha256::digest(key.as_bytes())))
    } else {
        None
    };
    let model = match body.get("model") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(model))
            if model.chars().count() <= 255 && !model.chars().any(char::is_control) =>
        {
            model.clone()
        }
        _ => {
            return Err(ApiError::BadRequest(
                "model must be a valid model identifier".into(),
            ));
        }
    };
    let deadline_secs = if mode == ModelAccessMode::NodeDispatch {
        state
            .node_gateway
            .as_ref()
            .map(|g| g.task_deadline().as_secs())
            .unwrap_or(1)
    } else {
        state.gateway_config.timeout_secs
    }
    .min(3600) as i64
        + 30;
    let (record, fresh) = store::create_response(
        pool,
        scope,
        store::NewResponse {
            request_id: request_id.0,
            model,
            input: store::normalize_input(body.get("input"))?,
            body: body.clone(),
            previous,
            conversation,
            background,
            store: retain,
            stream: streaming,
            deadline_secs,
            idempotency_hash,
            request_hash,
        },
    )
    .await?;
    if !fresh {
        if !record.retained() {
            return Err(store::missing());
        }
        if record.stream && record.background {
            return resource_stream(state, scope, record.id, None).await;
        }
        if record.active() && !record.background {
            return Err(store::conflict(
                "This idempotent response is still executing",
            ));
        }
        return Ok(Json(store::public_response(&record)).into_response());
    }
    let managed = ManagedExecution::new(state.clone(), record.clone());
    let mut upstream_body = body;
    upstream_body["model"] = record.model.clone().into();
    if record.previous_id.is_some() || record.conversation_id.is_some() {
        upstream_body["input"] = record.input_json.clone();
    }
    upstream_body
        .as_object_mut()
        .expect("object validated")
        .remove("previous_response_id");
    upstream_body
        .as_object_mut()
        .expect("object validated")
        .remove("conversation");
    upstream_body["store"] = false.into();
    upstream_body["background"] = false.into();
    if serde_json::to_vec(&upstream_body)
        .map_err(|_| ApiError::BadRequest("Invalid resolved request".into()))?
        .len()
        > store::MAX_HISTORY_BYTES
    {
        managed.abort().await;
        return Err(ApiError::BadRequest(
            "Resolved request exceeds 2 MiB; no history was truncated".into(),
        ));
    }
    for name in ["authorization", "x-api-key", "cookie", "idempotency-key"] {
        headers.remove(name);
    }
    let invocation = Invocation {
        state: state.clone(),
        auth,
        request_id,
        client_request_id,
        received,
        headers,
        permit,
        body: upstream_body,
        mode,
    };
    if background {
        if streaming {
            let queued = store::public_response(&record);
            if let Err(error) = store::append_event(pool, &record, false, |seq| {
                Ok(format!(
                    "event: response.created\ndata: {}\n\n",
                    json!({"type":"response.created","sequence_number":seq,"response":queued})
                ))
            })
            .await
            {
                managed.abort().await;
                return Err(error);
            }
        }
        tokio::spawn(async move {
            match invocation.execute(Some(managed.clone())).await {
                Ok(response) => {
                    use futures::StreamExt;
                    let mut body = response.into_body().into_data_stream();
                    let mut bytes = 0usize;
                    while let Some(frame) = body.next().await {
                        match frame {
                            Ok(data) => {
                                bytes = bytes.saturating_add(data.len());
                                if bytes > store::MAX_RESPONSE_BYTES * 2 {
                                    managed.cancel().await;
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
                Err(_) => managed.abort().await,
            }
        });
        if streaming {
            resource_stream(state, scope, record.id, None).await
        } else {
            Ok(Json(store::public_response(&record)).into_response())
        }
    } else {
        let result = invocation.execute(Some(managed.clone())).await;
        if result.is_err() {
            managed.abort().await;
        }
        result
    }
}
static RESOURCE_STREAM_SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
    std::sync::OnceLock::new();
async fn resource_stream(
    state: AppState,
    scope: Scope,
    id: String,
    starting_after: Option<i64>,
) -> Result<Response> {
    let slot = RESOURCE_STREAM_SLOTS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(32)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::RateLimit("Too many active response-replay streams".into()))?;
    let pool = state
        .pool
        .clone()
        .ok_or_else(|| ApiError::ServiceUnavailable("Platform state unavailable".into()))?;
    let mut after = starting_after.unwrap_or(-1);
    let initial = store::read_events(pool.write_conn(), scope, &id, after).await?;
    let stream = async_stream::stream! {
        let _slot=slot;let mut initial=Some(initial);
        let deadline=tokio::time::Instant::now()+Duration::from_secs(3600);
        loop {
            if tokio::time::Instant::now()>=deadline {yield Err::<bytes::Bytes,std::io::Error>(std::io::Error::other("Response replay connection expired; resume using its cursor"));break;}
            let next=if let Some(initial)=initial.take(){Ok(initial)}else{store::read_events(pool.write_conn(),scope,&id,after).await};
            let (record,events)=match next {Ok(v)=>v,Err(_)=>{yield Err(std::io::Error::other("Response state is no longer available"));break;}};
            if events.is_empty() {
                if !record.active() {
                    if !matches!(record.status.as_str(),"completed"|"incomplete") {yield Err(std::io::Error::other("Response execution ended without success"));}
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;continue;
            }
            for event in events {after=event.seq;yield Ok(bytes::Bytes::from(event.frame));}
        }
    };
    Ok((
        [
            ("content-type", "text/event-stream"),
            ("cache-control", "private, no-store"),
            ("x-accel-buffering", "no"),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

/// Resource routes share the family authentication/middleware, but accept a
/// smaller bounded administrative body than generation endpoints.
pub(crate) fn resource_routes(mode: ModelAccessMode) -> axum::Router<AppState> {
    use axum::routing::{get, post};
    let route = match mode {
        ModelAccessMode::Passthrough => axum::Router::new()
            .route(
                "/pt/v1/responses/{id}",
                get(passthrough_response_retrieve).delete(passthrough_response_delete),
            )
            .route(
                "/pt/v1/responses/{id}/input_items",
                get(passthrough_response_input_items),
            )
            .route(
                "/pt/v1/responses/{id}/cancel",
                post(passthrough_response_cancel),
            )
            .route(
                "/pt/v1/conversations",
                post(passthrough_conversation_create),
            )
            .route(
                "/pt/v1/conversations/{id}",
                get(passthrough_conversation_retrieve)
                    .post(passthrough_conversation_update)
                    .patch(passthrough_conversation_update)
                    .delete(passthrough_conversation_delete),
            )
            .route(
                "/pt/v1/conversations/{id}/items",
                get(passthrough_conversation_items_list)
                    .post(passthrough_conversation_items_create),
            )
            .route(
                "/pt/v1/conversations/{id}/items/{item_id}",
                get(passthrough_conversation_item_retrieve)
                    .delete(passthrough_conversation_item_delete),
            ),
        ModelAccessMode::NodeDispatch => axum::Router::new()
            .route(
                "/nt/v1/responses/{id}",
                get(node_response_retrieve).delete(node_response_delete),
            )
            .route(
                "/nt/v1/responses/{id}/input_items",
                get(node_response_input_items),
            )
            .route("/nt/v1/responses/{id}/cancel", post(node_response_cancel))
            .route("/nt/v1/conversations", post(node_conversation_create))
            .route(
                "/nt/v1/conversations/{id}",
                get(node_conversation_retrieve)
                    .post(node_conversation_update)
                    .patch(node_conversation_update)
                    .delete(node_conversation_delete),
            )
            .route(
                "/nt/v1/conversations/{id}/items",
                get(node_conversation_items_list).post(node_conversation_items_create),
            )
            .route(
                "/nt/v1/conversations/{id}/items/{item_id}",
                get(node_conversation_item_retrieve).delete(node_conversation_item_delete),
            ),
        ModelAccessMode::AccountPool => {
            unreachable!("ordinary resources use their own implementation")
        }
    };
    route.layer(axum::extract::DefaultBodyLimit::max(
        store::MAX_HISTORY_BYTES,
    ))
}
