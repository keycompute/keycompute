//! Execution observations are durable independently of subscriber delivery.
use super::{
    events,
    store::{self, ResponseRecord},
};
use crate::{
    error::{ApiError, Result},
    state::AppState,
};
use keycompute_types::{ClientResponseOutcome, ModelAccessMode, RequestContext};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;
static EXECUTIONS: OnceLock<Mutex<HashMap<String, Weak<ManagedExecution>>>> = OnceLock::new();
#[derive(Default)]
struct Captured {
    status: Option<u16>,
    headers: Vec<(String, String)>,
    body: Option<Value>,
    terminal: Option<String>,
}
pub struct ManagedExecution {
    pub state: AppState,
    pub record: ResponseRecord,
    captured: Mutex<Captured>,
    context: Mutex<Option<Arc<RequestContext>>>,
    finish_lock: tokio::sync::Mutex<()>,
    finished: AtomicBool,
}
impl ManagedExecution {
    pub fn new(state: AppState, record: ResponseRecord) -> Arc<Self> {
        let value = Arc::new(Self {
            state,
            record,
            captured: Mutex::new(Captured::default()),
            context: Mutex::new(None),
            finish_lock: tokio::sync::Mutex::new(()),
            finished: AtomicBool::new(false),
        });
        EXECUTIONS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(value.record.id.clone(), Arc::downgrade(&value));
        value
    }
    fn pool(&self) -> Result<&keycompute_db::DbRouter> {
        self.state
            .pool
            .as_deref()
            .ok_or_else(|| ApiError::ServiceUnavailable("Platform state is unavailable".into()))
    }
    pub async fn prepare(
        self: &Arc<Self>,
        ctx: Arc<RequestContext>,
        account_id: Uuid,
    ) -> Result<()> {
        if ctx.request_id != self.record.request_id
            || ctx.model != self.record.model
            || ctx.tenant_id != self.record.tenant_id
            || ctx.user_id != self.record.user_id
            || ctx.access_mode != self.record.scope().mode
        {
            return Err(ApiError::Internal(
                "Managed execution identity mismatch".into(),
            ));
        }
        if ctx.access_mode == ModelAccessMode::NodeDispatch && !ctx.stream {
            let gateway = self.state.node_gateway.as_ref().ok_or_else(|| {
                ApiError::ServiceUnavailable("Node task service is unavailable".into())
            })?;
            let native = keycompute_types::node_native::NodeNativeRequest {
                operation: keycompute_types::node_native::NodeNativeOperation::Responses,
                body: ctx
                    .native_openai_responses_request
                    .as_ref()
                    .ok_or_else(|| ApiError::Internal("Managed Responses body missing".into()))?
                    .as_ref()
                    .clone(),
                headers: vec![],
            };
            let ready = tokio::time::timeout(
                Duration::from_secs(3),
                gateway
                    .store
                    .cancellable_native_ready(ctx.tenant_id, &native),
            )
            .await
            .map_err(|_| ApiError::ServiceUnavailable("Node capability lookup timed out".into()))?
            .map_err(|_| {
                ApiError::ServiceUnavailable("Node capability lookup unavailable".into())
            })?;
            if !ready {
                return Err(ApiError::ServiceUnavailable(
                    "No cancellation-capable node supports this managed response".into(),
                ));
            }
        }
        let account = (ctx.access_mode == ModelAccessMode::Passthrough).then_some(account_id);
        let execution = json!({"pricing":ctx.pricing_snapshot,"account_id":account,"key_id":ctx.produce_ai_key_id,"request_id":ctx.request_id,"billing_provider":if account.is_some(){"openai"}else{"node"},"accounting_pending":true});
        store::activate(self.pool()?, &self.record, account, execution).await?;
        *self.context.lock().unwrap_or_else(|e| e.into_inner()) = Some(ctx);
        Self::watch(Arc::downgrade(self));
        Ok(())
    }
    fn watch(weak: Weak<Self>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let Some(owner) = weak.upgrade() else { break };
                if owner.finished.load(Ordering::Acquire) {
                    break;
                }
                let valid = match owner.pool() {
                    Ok(pool) => store::running(pool, &owner.record).await.unwrap_or(false),
                    Err(_) => false,
                };
                if !valid {
                    owner.cancel().await;
                    break;
                }
            }
        });
    }
    pub async fn cancel(&self) {
        let ctx = self
            .context
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(ctx) = ctx {
            ctx.cancel_upstream(ClientResponseOutcome::ResponseFailed);
        }
        cancel_node(&self.state, &self.record).await;
    }
    pub async fn capture_http(
        &self,
        ctx: &RequestContext,
        status: u16,
        headers: &[(String, String)],
        body: &Value,
    ) -> Result<()> {
        {
            let mut c = self.captured.lock().unwrap_or_else(|e| e.into_inner());
            c.status = Some(status);
            c.headers = headers
                .iter()
                .filter(|(name, value)| {
                    keycompute_types::node_native::native_response_header_allowed(name, value)
                })
                .cloned()
                .collect();
            c.body = Some(body.clone());
        }
        self.checkpoint(ctx).await
    }
    pub async fn checkpoint(&self, ctx: &RequestContext) -> Result<()> {
        let (status, headers, body) = {
            let c = self.captured.lock().unwrap_or_else(|e| e.into_inner());
            (c.status, c.headers.clone(), c.body.clone())
        };
        let (input, output) = ctx.usage_snapshot();
        let value = json!({"native_result":body.map(|body|json!({"status":status.unwrap_or(200),"headers":headers,"body":body})),
            "input_tokens":input,"output_tokens":output,"input_exact":ctx.is_input_finalized(),
            "output_exact":ctx.is_output_finalized(),"tpm_reserved":ctx.tpm_reservation_tokens(),
            "balance_reservation_owner":ctx.balance_reservation_owner_token(),"upstream_accepted":ctx.is_upstream_response_accepted()});
        // A cancelled/deleted resource may reject payload persistence, but actual
        // observed usage still belongs to the detached settlement owner.
        for attempt in 0..3 {
            match store::record_execution(self.pool()?, &self.record, value.clone()).await {
                Ok(_) => return Ok(()),
                Err(error) if attempt == 2 => return Err(error),
                Err(_) => tokio::time::sleep(Duration::from_millis(50 * (attempt + 1))).await,
            }
        }
        unreachable!()
    }
    pub async fn frame(&self, ctx: &RequestContext, raw: &str) -> Result<Option<String>> {
        let translated = events::translate(raw, &self.record.id, 0)?;
        if translated.terminal_status.is_some() {
            let body = translated
                .final_response
                .ok_or_else(|| ApiError::Provider("Native terminal response is missing".into()))?;
            self.capture_http(ctx, 200, &[], &body).await?;
            self.captured
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .terminal = Some(raw.into());
            return Ok(None);
        }
        let frame = store::append_event(self.pool()?, &self.record, false, |seq| {
            events::translate_with(raw, &self.record.id, seq, |body| {
                let status = body
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("in_progress")
                    .to_string();
                *body = store::final_response(&self.record, body.take(), &status);
            })
            .map(|event| event.frame)
        })
        .await?;
        // Checkpoint observed usage independently of whether a replay client
        // consumes this frame; a later process can reconcile an interrupted run.
        self.checkpoint(ctx).await?;
        Ok(Some(frame))
    }
    pub async fn finish(
        &self,
        outcome: ClientResponseOutcome,
        secured: bool,
    ) -> Result<(Value, Option<String>)> {
        let _lock = self.finish_lock.lock().await;
        if self.finished.load(Ordering::Acquire) {
            let row = store::owned(self.pool()?, &self.record).await?;
            if row.active() {
                return Err(ApiError::ServiceUnavailable(
                    "Managed response is still reconciling".into(),
                ));
            }
            return Ok((store::public_response(&row), None));
        }
        if secured {
            store::accounting_secured(self.pool()?, &self.record).await?;
        }
        let (status, body, terminal) = {
            let c = self.captured.lock().unwrap_or_else(|e| e.into_inner());
            (c.status, c.body.clone(), c.terminal.clone())
        };
        let mut result_status = if !secured
            || outcome != ClientResponseOutcome::Succeeded
            || status.is_some_and(|s| s != 200)
        {
            "failed"
        } else {
            match body
                .as_ref()
                .and_then(|b| b.get("status"))
                .and_then(Value::as_str)
            {
                Some("incomplete") => "incomplete",
                Some("failed") => "failed",
                Some("cancelled") => "cancelled",
                _ => "completed",
            }
        };
        if body.is_none() {
            result_status = "failed";
        }
        let mut body = body.unwrap_or_else(|| store::public_response(&self.record));
        if !secured {
            body["error"] = json!({"code":"settlement_unavailable","message":"Execution ended but durable accounting could not be secured"});
        } else if result_status == "failed" && body.get("error").is_none_or(Value::is_null) {
            body["error"] = json!({"code":"execution_interrupted","message":"Execution did not produce a complete response"});
        }
        let mut completed = None;
        for attempt in 0..3 {
            match store::finish_and_event(
                self.pool()?,
                &self.record,
                result_status,
                body.clone(),
                terminal.as_deref(),
            )
            .await
            {
                Ok(result) => {
                    completed = Some(result);
                    break;
                }
                Err(error) if attempt == 2 => return Err(error),
                Err(_) => tokio::time::sleep(Duration::from_millis(50 * (attempt + 1))).await,
            }
        }
        let (row, frame) = completed.expect("bounded retry returns");
        if row.active() {
            return Err(ApiError::ServiceUnavailable(
                "Managed response persistence is pending".into(),
            ));
        }
        self.finished.store(true, Ordering::Release);
        *self.context.lock().unwrap_or_else(|e| e.into_inner()) = None;
        store::check_account(self.pool()?.write_conn(), row.scope(), row.account_id).await?;
        if row.deleted_at.is_some() {
            return Err(store::missing());
        }
        Ok((store::public_response(&row), frame))
    }
    pub async fn abort(&self) {
        self.cancel().await;
        let _ = self
            .finish(ClientResponseOutcome::ResponseFailed, true)
            .await;
    }
}
impl Drop for ManagedExecution {
    fn drop(&mut self) {
        if let Some(registry) = EXECUTIONS.get() {
            registry
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.record.id);
        }
    }
}
pub(crate) async fn cancel_execution(state: &AppState, record: &ResponseRecord) -> Result<()> {
    let active = EXECUTIONS.get().and_then(|map| {
        map.lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&record.id)
            .and_then(Weak::upgrade)
    });
    if let Some(active) = active {
        active.cancel().await;
    } else {
        cancel_node(state, record).await;
    }
    Ok(())
}
async fn cancel_node(state: &AppState, record: &ResponseRecord) {
    if record.scope().mode != ModelAccessMode::NodeDispatch {
        return;
    }
    if let (Some(pool), Some(gateway)) = (&state.pool, &state.node_gateway) {
        let query = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM node_tasks WHERE request_id=$1 AND user_id=$2 AND status IN ('queued','leased') LIMIT 1",
            [record.request_id.into(), record.user_id.into()],
        );
        if let Ok(Ok(Some(row))) =
            tokio::time::timeout(Duration::from_secs(2), pool.write_conn().query_one(query)).await
            && let Ok(id) = row.try_get::<Uuid>("", "id")
        {
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                gateway.cancel_native_stream(id, "managed_response_cancelled"),
            )
            .await;
        }
    }
}
pub(super) fn rewrite_frame(
    raw: &str,
    id: &str,
    seq: i64,
    final_body: Option<&Value>,
    _background: bool,
) -> Result<String> {
    // Terminal body and status come from the committed resource, never from a
    // late upstream completion that lost a cancellation race.
    if let Some(body) = final_body {
        let status = body
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("failed");
        let native = events::translate(raw, id, seq)?;
        if native.terminal_status == Some(status) {
            return events::translate_with(raw, id, seq, |response| *response = body.clone())
                .map(|event| event.frame);
        }
        let kind = match status {
            "completed" => "response.completed",
            "incomplete" => "response.incomplete",
            _ => "response.failed",
        };
        return Ok(format!(
            "id: {seq}\nevent: {kind}\ndata: {}\n\n",
            json!({"type":kind,"sequence_number":seq,"response":body})
        ));
    }
    events::translate(raw, id, seq).map(|value| value.frame)
}
