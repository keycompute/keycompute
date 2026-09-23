//! 节点网关 HTTP Handler
//!
//! 处理 node-token 客户端的注册、心跳、任务领取和结果提交请求

use crate::{
    error::{ApiError, Result},
    extractors::{NodeSessionAuth, NodeSessionCompletionAuth},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, State},
};
use keycompute_types::node::{
    NodeHeartbeatRequest, NodeHeartbeatResponse, NodePollRequest, NodePollResponse,
    NodeRegisterRequest, NodeRegisterResponse, NodeTaskCompleteRequest, NodeTaskCompleteResponse,
    NodeTaskStreamEventRequest, NodeTaskStreamEventResponse,
};
use node_gateway::NodeGatewayService;
use std::sync::Arc;
use uuid::Uuid;

/// 节点注册 Handler
/// POST /node/v1/register
///
/// 不需要 session token 认证，使用 HMAC 签名的 registration_token 验证。
/// token 由用户申请 → Admin 审批后下发 → 注册时一次性消费。
pub async fn node_register(
    State(state): State<AppState>,
    Json(request): Json<NodeRegisterRequest>,
) -> Result<Json<NodeRegisterResponse>> {
    if request.protocol_version != "node.v1" {
        return Err(ApiError::BadRequest(
            "Unsupported node control protocol".into(),
        ));
    }
    node_gateway::NodeGatewayStore::validate_capabilities(&request.capabilities)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let node_gateway = get_node_gateway(&state)?;

    let response = node_gateway
        .register_node(&request)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(response))
}

/// POST /node/v1/capabilities. Existing credentials authenticate the exchange;
/// old sessions remain authorized only to finish their already leased tasks.
pub async fn node_capabilities(
    State(state): State<AppState>,
    auth: NodeSessionCompletionAuth,
    Json(body): Json<keycompute_types::node::NodeCapabilitiesRequest>,
) -> Result<Json<NodeRegisterResponse>> {
    if body.node_id != auth.node_id || body.session_id != auth.session_id {
        return Err(ApiError::NodeIdentityMismatch {
            expected_node_id: auth.node_id,
            expected_session_id: auth.session_id,
            actual_node_id: body.node_id,
            actual_session_id: body.session_id,
        });
    }
    if body.protocol_version != "node.v1" {
        return Err(ApiError::BadRequest(
            "Unsupported node control protocol".into(),
        ));
    }
    node_gateway::NodeGatewayStore::validate_capabilities(&body.capabilities)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let gateway = get_node_gateway(&state)?;
    let response = gateway
        .store
        .negotiate_capabilities(auth.node_id, auth.session_id, &body.capabilities)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

/// 节点心跳 Handler
/// POST /node/v1/heartbeat
///
/// 需要 session token 认证
pub async fn node_heartbeat(
    State(state): State<AppState>,
    auth: NodeSessionAuth,
    Json(body): Json<NodeHeartbeatRequest>,
) -> Result<Json<NodeHeartbeatResponse>> {
    // 验证请求体中的 node_id 和 session_id 与认证结果一致
    if body.node_id != auth.node_id || body.session_id != auth.session_id {
        return Err(ApiError::NodeIdentityMismatch {
            expected_node_id: auth.node_id,
            expected_session_id: auth.session_id,
            actual_node_id: body.node_id,
            actual_session_id: body.session_id,
        });
    }

    let node_gateway = get_node_gateway(&state)?;

    // 从 body 中提取 accepted_models
    let accepted_models = body.accepted_models.clone();

    let response = node_gateway
        .heartbeat(auth.node_id, auth.session_id, accepted_models)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(response))
}

/// 节点任务轮询 Handler
/// POST /node/v1/tasks/poll
///
/// 需要 session token 认证,长轮询领取任务
pub async fn node_poll(
    State(state): State<AppState>,
    auth: NodeSessionAuth,
    Json(body): Json<NodePollRequest>,
) -> Result<Json<NodePollResponse>> {
    // 验证请求体中的 node_id 和 session_id 与认证结果一致
    if body.node_id != auth.node_id || body.session_id != auth.session_id {
        return Err(ApiError::NodeIdentityMismatch {
            expected_node_id: auth.node_id,
            expected_session_id: auth.session_id,
            actual_node_id: body.node_id,
            actual_session_id: body.session_id,
        });
    }

    let node_gateway = get_node_gateway(&state)?;

    // 从数据库读取 session 的 accepted_models
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database pool not configured".to_string()))?;

    // Heartbeat persists accepted_models on the writer. Polling must observe
    // that fresh authorization state instead of a potentially lagging replica.
    let session = keycompute_db::models::node_session::NodeSession::find_in_scope(
        pool.write_conn(),
        keycompute_db::NodeSessionScope::checked(auth.tenant_id, auth.node_id, auth.owner_user_id)
            .map_err(ApiError::from)?,
        auth.session_id,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to query session: {}", e)))?
    .ok_or_else(|| ApiError::NotFound(format!("Session {} not found", auth.session_id)))?;

    let accepted_models: Vec<String> =
        serde_json::from_value(session.accepted_models_json).unwrap_or_default();

    let response = node_gateway
        .poll_task(auth.node_id, auth.session_id, accepted_models)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(response))
}

/// 节点任务完成 Handler
/// POST /node/v1/tasks/{task_id}/complete
///
/// 需要 session token 认证，支持幂等重试
pub async fn node_complete(
    State(state): State<AppState>,
    auth: NodeSessionCompletionAuth,
    Path(task_id): Path<Uuid>,
    Json(body): Json<NodeTaskCompleteRequest>,
) -> Result<Json<NodeTaskCompleteResponse>> {
    // 验证请求体中的 node_id 和 session_id 与认证结果一致
    if body.node_id != auth.node_id || body.session_id != auth.session_id {
        return Err(ApiError::NodeIdentityMismatch {
            expected_node_id: auth.node_id,
            expected_session_id: auth.session_id,
            actual_node_id: body.node_id,
            actual_session_id: body.session_id,
        });
    }

    // 验证路径中的 task_id 与请求体中的 task_id 一致
    if body.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task_id in path does not match task_id in body".to_string(),
        ));
    }

    let node_gateway = get_node_gateway(&state)?;

    let response = node_gateway
        .complete_task(
            body.task_id,
            body.lease_id,
            auth.node_id,
            auth.session_id,
            body.result,
        )
        .await
        .map_err(ApiError::from)?;

    Ok(Json(response))
}

/// POST /node/v1/tasks/{task_id}/events
///
/// Native stream delivery uses the completion-authentication policy, but its
/// lease/session/task checks are performed atomically by the gateway store.
pub async fn node_stream_event(
    State(state): State<AppState>,
    auth: NodeSessionCompletionAuth,
    Path(task_id): Path<Uuid>,
    Json(body): Json<NodeTaskStreamEventRequest>,
) -> Result<Json<NodeTaskStreamEventResponse>> {
    if body.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task_id in path does not match task_id in body".into(),
        ));
    }
    if body.node_id != auth.node_id || body.session_id != auth.session_id {
        return Err(ApiError::NodeIdentityMismatch {
            expected_node_id: auth.node_id,
            expected_session_id: auth.session_id,
            actual_node_id: body.node_id,
            actual_session_id: body.session_id,
        });
    }
    if body.protocol_version != "node.v1" {
        return Err(ApiError::BadRequest(
            "Unsupported node control protocol".into(),
        ));
    }
    let gateway = get_node_gateway(&state)?;
    let completion = match &body.event {
        keycompute_types::node::NodeNativeStreamEvent::Terminal { summary } => Some(
            keycompute_types::node::NodeTaskResult::NativeStreamSucceeded {
                summary: summary.clone(),
            },
        ),
        keycompute_types::node::NodeNativeStreamEvent::Failed { code, message, .. } => {
            Some(keycompute_types::node::NodeTaskResult::Failed {
                code: code.clone(),
                message: message.clone(),
                is_client_error: false,
            })
        }
        _ => None,
    };
    let ids = (body.task_id, body.lease_id, body.node_id, body.session_id);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        gateway.store.accept_native_stream_event(body),
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Native stream event storage timed out".into()))?
    .map_err(map_native_stream_error)?;
    if response.accepted
        && let Some(result) = completion
    {
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            gateway.complete_task(ids.0, ids.1, ids.2, ids.3, result),
        )
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Native stream completion timed out".into()))?
        .map_err(map_native_stream_error)?;
    }
    Ok(Json(response))
}

fn map_native_stream_error(error: keycompute_db::DbError) -> ApiError {
    let keycompute_db::DbError::Other(message) = error else {
        return ApiError::from(error);
    };
    match message.as_str() {
        "native_stream_lease_mismatch"
        | "native_stream_session_inactive"
        | "native_stream_tenant_inactive"
        | "native_stream_capability_denied" => ApiError::Auth(message),
        "native_stream_delivery_backpressure" => {
            ApiError::RateLimit("native stream delivery window is full".into())
        }
        "native_stream_sequence_conflict"
        | "native_stream_sequence_gap"
        | "native_stream_already_terminal"
        | "native_stream_terminal_mismatch"
        | "native_stream_usage_mismatch"
        | "native_stream_result_variant_mismatch"
        | "native_stream_result_expected" => ApiError::NodeTaskConflict(message),
        "native_stream_event_limit"
        | "native_stream_encoding"
        | "native_stream_payload_missing"
        | "native_stream_payload_invalid"
        | "native_stream_capability_invalid"
        | "native_stream_start_invalid"
        | "native_stream_must_start_at_seq_zero"
        | "native_stream_duplicate_start"
        | "native_stream_frame_limit"
        | "native_stream_total_limit"
        | "native_stream_frame_invalid"
        | "native_stream_error_limit"
        | "native_stream_state_invalid" => ApiError::BadRequest(message),
        "native_stream_deadline" => ApiError::NodeTaskConflict(message),
        "lease_mismatch"
        | "invalid_task_state"
        | "task_expired_during_complete"
        | "node_result_type_mismatch"
        | "concurrent_task_update_failed" => ApiError::NodeTaskConflict(message),
        "Session revoked" | "Session has been revoked" => ApiError::Auth(message),
        _ => ApiError::Internal(message),
    }
}

/// 获取 NodeGatewayService 引用
fn get_node_gateway(state: &AppState) -> Result<Arc<NodeGatewayService>> {
    state
        .node_gateway
        .clone()
        .ok_or_else(|| ApiError::Internal("Node gateway not configured".to_string()))
}

/// Metadata-only status for an already issued native task lease.
pub async fn node_lease_status(
    State(state): State<AppState>,
    auth: NodeSessionCompletionAuth,
    Path(task_id): Path<Uuid>,
    Json(body): Json<keycompute_types::node::NodeTaskLeaseStatusRequest>,
) -> Result<Json<keycompute_types::node::NodeTaskLeaseStatusResponse>> {
    if body.task_id != task_id {
        return Err(ApiError::BadRequest(
            "Task identity does not match the path".into(),
        ));
    }
    if body.node_id != auth.node_id || body.session_id != auth.session_id {
        return Err(ApiError::NodeIdentityMismatch {
            expected_node_id: auth.node_id,
            expected_session_id: auth.session_id,
            actual_node_id: body.node_id,
            actual_session_id: body.session_id,
        });
    }
    if body.protocol_version != "node.v1" {
        return Err(ApiError::BadRequest(
            "Unsupported node control protocol".into(),
        ));
    }
    let gateway = get_node_gateway(&state)?;
    gateway
        .store
        .native_lease_status(&body)
        .await
        .map(Json)
        .map_err(|error| match error {
            keycompute_db::DbError::NotFound { .. } => {
                ApiError::NotFound("Native task lease was not found".into())
            }
            _ => {
                ApiError::ServiceUnavailable("Native task status is temporarily unavailable".into())
            }
        })
}
