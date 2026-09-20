//! Bounded crash reconciliation. Interrupted inference is never resubmitted.
use super::{
    execution,
    store::{self, ResponseRecord},
};
use crate::{
    error::{ApiError, Result},
    state::AppState,
};
use keycompute_types::{
    PricingSnapshot, RequestContext,
    node_native::{NodeNativeHttpResult, NodeNativeOperation},
    node_stream::NodeNativeStreamSummary,
};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;
fn unavailable() -> ApiError {
    ApiError::ServiceUnavailable("Managed response reconciliation is pending".into())
}
fn token(value: &Value, name: &str) -> u32 {
    value
        .get(name)
        .and_then(Value::as_u64)
        .filter(|n| *n <= i32::MAX as u64)
        .unwrap_or(0) as u32
}

pub(crate) fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(30));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) = run_once(&state).await {
                tracing::warn!(%error,"managed response maintenance will retry");
            }
        }
    });
}

pub(crate) async fn run_once(state: &AppState) -> Result<usize> {
    let Some(pool) = &state.pool else {
        return Ok(0);
    };
    let candidates = store::recovery_candidates(pool).await?;
    let mut recovered = 0;
    let iteration_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    for candidate in candidates {
        if tokio::time::Instant::now() >= iteration_deadline {
            break;
        }
        let Some(record) = store::claim_recovery(pool, &candidate).await? else {
            continue;
        };
        let result = tokio::time::timeout_at(iteration_deadline, recover(state, &record)).await;
        match result {
            Ok(Ok(())) => recovered += 1,
            Ok(Err(error)) => {
                tracing::warn!(response_id=%record.id,%error,"managed response reconciliation deferred without repeating inference")
            }
            Err(_) => {
                tracing::warn!(response_id=%record.id,"managed response reconciliation timed out without repeating inference")
            }
        }
    }
    store::cleanup(pool).await?;
    Ok(recovered)
}

async fn recover(state: &AppState, record: &ResponseRecord) -> Result<()> {
    let pool = state.pool.as_ref().ok_or_else(unavailable)?;
    // A stalled old process loses the resource ownership fence. Cancellation
    // stops any remaining local/node work; no new task is ever created here.
    execution::cancel_execution(state, record).await?;
    let saved = record.execution_json.clone().unwrap_or_else(|| json!({}));
    let mut raw = saved
        .get("native_result")
        .filter(|v| !v.is_null())
        .cloned()
        .and_then(|v| serde_json::from_value::<NodeNativeHttpResult>(v).ok());
    let mut summary = None;
    if record.access_mode == "node_dispatch" {
        let task = tokio::time::timeout(
            Duration::from_secs(3),
            pool.write_conn().query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id,result_json FROM node_tasks WHERE request_id=$1 AND user_id=$2 LIMIT 1",
                [record.request_id.into(), record.user_id.into()],
            )),
        )
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
        if let Some(task) = task {
            let task_id = task.try_get::<Uuid>("", "id").map_err(|_| unavailable())?;
            if let Some(value) = task
                .try_get::<Option<Value>>("", "result_json")
                .map_err(|_| unavailable())?
            {
                if value.get("body").is_some() {
                    raw = serde_json::from_value(value).ok();
                } else {
                    summary = serde_json::from_value::<NodeNativeStreamSummary>(value).ok();
                }
            }
            if summary.is_none()
                && let Some(gateway) = &state.node_gateway
            {
                summary = tokio::time::timeout(
                    Duration::from_secs(3),
                    gateway.store.native_stream_summary(task_id),
                )
                .await
                .map_err(|_| unavailable())?
                .map_err(|_| unavailable())?;
            }
            if raw.is_none()
                && let Some(body) = summary.as_ref().and_then(|s| s.final_body.clone())
            {
                raw = Some(NodeNativeHttpResult {
                    status: 200,
                    headers: vec![],
                    body,
                });
            }
        }
    }
    if let Some(raw) = &mut raw {
        raw.headers.retain(|(name, value)| {
            keycompute_types::node_native::native_response_header_allowed(name, value)
        });
    }
    let native_valid = raw.as_ref().is_some_and(|r| {
        r.validate_for(NodeNativeOperation::Responses, &record.model)
            .is_ok()
    });
    let mut input = token(&saved, "input_tokens");
    let mut output = token(&saved, "output_tokens");
    let mut input_exact = saved["input_exact"] == true;
    let mut output_exact = saved["output_exact"] == true;
    if let Some(result) = raw.as_ref().filter(|_| native_valid)
        && let Ok(Some((i, o))) = result.validate_for(NodeNativeOperation::Responses, &record.model)
    {
        input = i;
        output = o;
        input_exact = true;
        output_exact = true;
    } else if let Some(usage) = summary.as_ref().and_then(|s| s.usage.as_ref()) {
        input = usage.input_tokens;
        output = usage.output_tokens;
        input_exact = usage.input_exact;
        output_exact = usage.output_exact;
    }
    let observed = output > 0
        || input_exact
        || saved["upstream_accepted"] == true
        || raw
            .as_ref()
            .is_some_and(|r| native_valid && r.status == 200);
    let key = saved
        .get("key_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    let reservation=tokio::time::timeout(Duration::from_secs(3),pool.write_conn().query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,"SELECT owner_token FROM balance_reservations WHERE request_id=$1 AND user_id=$2 AND tenant_id=$3 AND status='active'",
        [record.request_id.into(),record.user_id.into(),record.tenant_id.into()]))).await.map_err(|_|unavailable())?.map_err(|_|unavailable())?;
    let owner = reservation
        .as_ref()
        .and_then(|r| r.try_get::<Uuid>("", "owner_token").ok())
        .or_else(|| {
            saved
                .get("balance_reservation_owner")
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok())
        });
    let mut secured = true;
    if observed {
        let price = saved
            .get("pricing")
            .cloned()
            .and_then(|p| serde_json::from_value::<PricingSnapshot>(p).ok())
            .ok_or_else(unavailable)?;
        let mut ctx = RequestContext::new(
            record.request_id,
            record.user_id,
            record.tenant_id,
            key.ok_or_else(unavailable)?,
            record.model.clone(),
            vec![],
            record.stream,
            price,
        );
        ctx.access_mode = record.scope().mode;
        ctx.native_openai_responses_request = Some(Arc::new({
            let mut request = record.request_json.clone();
            request["input"] = record.input_json.clone();
            request
        }));
        if let Some(owner) = owner {
            ctx.set_balance_reservation_owner_token(owner);
        }
        if let Some(tokens) = saved
            .get("tpm_reserved")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
        {
            ctx.set_tpm_reservation_tokens(tokens);
        }
        if input_exact {
            ctx.set_input_tokens(input);
        } else {
            if input == 0 {
                input = llm_gateway::GatewayExecutor::estimate_context_input_tokens(&ctx);
            }
            ctx.set_input_tokens_estimate(input);
        }
        if output_exact {
            ctx.set_output_tokens(output);
        } else {
            ctx.set_output_tokens_estimate(output);
        }
        let provider = if record.account_id.is_some() {
            "openai"
        } else {
            "node"
        };
        ctx.set_provider(provider);
        secured = crate::handlers::finalize_immediate_settlement_logged(
            &crate::handlers::ImmediateSettlementServices::from_state(state),
            &ctx,
            provider,
            record.account_id.unwrap_or_else(Uuid::nil),
            if native_valid && raw.as_ref().is_some_and(|r| r.status == 200) {
                "success"
            } else {
                "incomplete"
            },
            "openai",
        )
        .await;
    } else {
        if let (Some(balance), Some(owner)) = (state.billing.balance_service(), owner) {
            balance
                .release_request_reservation(record.request_id, owner)
                .await
                .map_err(ApiError::from)?;
        }
        if let Some(key) = key {
            state
                .rate_limiter
                .release_token_reservation(
                    &keycompute_ratelimit::RateLimitKey::new(record.tenant_id, record.user_id, key),
                    record.request_id,
                )
                .await
                .map_err(ApiError::from)?;
        }
    }
    if !secured {
        return Err(unavailable());
    }
    store::accounting_secured(pool, record).await?;
    if record.active() {
        if let Some(result) = raw.filter(|_| native_valid) {
            let status = if result.status != 200 {
                "failed"
            } else {
                match result.body["status"].as_str() {
                    Some("completed") => "completed",
                    Some("incomplete") => "incomplete",
                    _ => "failed",
                }
            };
            store::finish(pool, record, status, result.body).await?;
        } else {
            store::fail(pool,record,"execution_interrupted","The previous execution was interrupted and its final result is unavailable. Inference was not automatically repeated.").await?;
        }
    } else {
        store::release_conversation(pool.write_conn(), record).await?;
    }
    Ok(())
}
