//! Durable bounded node event delivery. Database locks never cover network I/O.
use crate::NodeGatewayStore;
use keycompute_db::{
    DbError,
    models::{node_session::NodeSession, node_task::NodeTask},
};
use keycompute_types::{node::*, node_capability::*, node_native::*, node_stream::*};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;
fn error(code: &str) -> DbError {
    DbError::Other(code.into())
}
pub fn stream_protocol(op: NodeNativeOperation) -> NativeStreamProtocol {
    match op {
        NodeNativeOperation::Chat => NativeStreamProtocol::Chat,
        NodeNativeOperation::Messages => NativeStreamProtocol::Messages,
        NodeNativeOperation::Responses => NativeStreamProtocol::Responses,
    }
}
#[derive(FromQueryResult)]
struct StreamRow {
    next_seq: i64,
    unread_frames: i32,
    unread_bytes: i64,
    total_bytes: i64,
    state: String,
    inspector_json: Value,
    head_status: i32,
    head_headers: Value,
    summary_json: Option<Value>,
}

#[derive(FromQueryResult)]
struct StreamTenantScope {
    owner_tenant_id: Uuid,
    caller_tenant_id: Uuid,
}

#[derive(FromQueryResult)]
struct StreamTenantStatus {
    id: Uuid,
    status: String,
}

fn stream_response_header_allowed(name: &str, value: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let value_ok = value.len() <= 256 && value.bytes().all(|byte| (32..127).contains(&byte));
    value_ok
        && match name.as_str() {
            "content-type" => value
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream")),
            "retry-after"
            | "x-request-id"
            | "request-id"
            | "x-ratelimit-limit-requests"
            | "x-ratelimit-remaining-requests"
            | "x-ratelimit-reset-requests"
            | "x-ratelimit-limit-tokens"
            | "x-ratelimit-remaining-tokens"
            | "x-ratelimit-reset-tokens" => true,
            _ => false,
        }
}

async fn lock_active_stream_tenants(
    tx: &sea_orm::DatabaseTransaction,
    task_id: Uuid,
) -> Result<StreamTenantScope, DbError> {
    let scope = tx
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT node.tenant_id AS owner_tenant_id, task.tenant_id AS caller_tenant_id \
             FROM node_tasks task \
             JOIN nodes node ON node.id=task.assigned_node_id \
             WHERE task.id=$1",
            [task_id.into()],
        ))
        .await?
        .ok_or_else(|| DbError::not_found("NodeTask", task_id.to_string()))?;
    let scope = StreamTenantScope::from_query_result(&scope, "")?;
    let mut tenant_ids = vec![scope.owner_tenant_id, scope.caller_tenant_id];
    tenant_ids.sort_unstable();
    tenant_ids.dedup();
    for tenant_id in tenant_ids {
        let row = tx
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id,status FROM tenants WHERE id=$1 FOR SHARE",
                [tenant_id.into()],
            ))
            .await?
            .ok_or_else(|| DbError::Other("native_stream_tenant_inactive".into()))?;
        let tenant = StreamTenantStatus::from_query_result(&row, "")?;
        if tenant.id != tenant_id || tenant.status != "active" {
            return Err(error("native_stream_tenant_inactive"));
        }
    }
    Ok(scope)
}
#[derive(Debug)]
pub struct StoredNativeEvent {
    pub lease_id: Uuid,
    pub seq: u64,
    pub event: NodeNativeStreamEvent,
}
impl NodeGatewayStore {
    pub async fn accept_native_stream_event(
        &self,
        r: NodeTaskStreamEventRequest,
    ) -> Result<NodeTaskStreamEventResponse, DbError> {
        // 4096 raw frames plus Start and Terminal use sequence numbers 0..=4097;
        // retries reuse their original sequence and do not consume another slot.
        if r.seq > 4097 {
            return Err(error("native_stream_event_limit"));
        }
        let value = serde_json::to_value(&r.event).map_err(|_| error("native_stream_encoding"))?;
        let encoded = serde_json::to_vec(&value).map_err(|_| error("native_stream_encoding"))?;
        if encoded.len() > 1024 * 1024 {
            return Err(error("native_stream_event_limit"));
        }
        let hash = format!("{:x}", Sha256::digest(&encoded));
        let tx = self.pool().begin().await?;
        tx.execute_unprepared(
            "SET LOCAL statement_timeout='1500ms'; SET LOCAL lock_timeout='500ms'",
        )
        .await?;
        // Lock order is tenant(s), task, session.  Tenant rows are sorted by
        // UUID so a task whose caller and node owner are in different tenants
        // cannot deadlock another stream transaction with the inverse pair.
        // No lock is held while the event is sent to a network client.
        let locked_tenants = lock_active_stream_tenants(&tx, r.task_id).await?;
        let task = NodeTask::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_tasks WHERE id=$1 FOR UPDATE",
            [r.task_id.into()],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| error("native_stream_lease_mismatch"))?;
        if task.assigned_node_id != Some(r.node_id)
            || task.assigned_session_id != Some(r.session_id)
            || task.lease_id != Some(r.lease_id)
        {
            return Err(error("native_stream_lease_mismatch"));
        }
        let current_scope=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT node.tenant_id AS owner_tenant_id,ot.status AS owner_status,task.tenant_id AS caller_tenant_id,ct.status AS caller_status \
             FROM node_tasks task JOIN nodes node ON node.id=task.assigned_node_id \
             JOIN tenants ot ON ot.id=node.tenant_id \
             JOIN tenants ct ON ct.id=task.tenant_id WHERE task.id=$1",
            [r.task_id.into()])).await?.ok_or_else(||error("native_stream_tenant_inactive"))?;
        let owner_tenant: Uuid = current_scope.try_get("", "owner_tenant_id")?;
        let caller_tenant: Uuid = current_scope.try_get("", "caller_tenant_id")?;
        let owner_status: String = current_scope.try_get("", "owner_status")?;
        let caller_status: String = current_scope.try_get("", "caller_status")?;
        if owner_tenant != locked_tenants.owner_tenant_id
            || caller_tenant != locked_tenants.caller_tenant_id
            || owner_status != "active"
            || caller_status != "active"
        {
            return Err(error("native_stream_tenant_inactive"));
        }
        let session=NodeSession::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT ns.* FROM node_sessions ns JOIN nodes n ON n.id=ns.node_id WHERE ns.id=$1 AND ns.node_id=$2 AND ns.revoked_at IS NULL AND ns.expires_at>NOW() AND n.status='online' FOR UPDATE",
            [r.session_id.into(),r.node_id.into()])).one(&tx).await?.ok_or_else(||error("native_stream_session_inactive"))?;
        let native: NodeNativeRequest = serde_json::from_value(
            task.payload_json
                .get("native")
                .cloned()
                .ok_or_else(|| error("native_stream_payload_missing"))?,
        )
        .map_err(|_| error("native_stream_payload_invalid"))?;
        let requirements: NativeRequirements = serde_json::from_value(
            task.native_requirements_json
                .clone()
                .ok_or_else(|| error("native_stream_capability_invalid"))?,
        )
        .map_err(|_| error("native_stream_capability_invalid"))?;
        let request_requirements = NativeRequirements::from_request(&native)
            .map_err(|_| error("native_stream_capability_invalid"))?;
        let persisted = serde_json::to_value(&requirements)
            .map_err(|_| error("native_stream_capability_invalid"))?;
        let derived = serde_json::to_value(&request_requirements)
            .map_err(|_| error("native_stream_capability_invalid"))?;
        if native.body.get("stream").and_then(Value::as_bool) != Some(true)
            || !requirements.features.contains(&NativeFeature::Sse)
            || persisted != derived
            || requirements.model != task.model
        {
            return Err(error("native_stream_capability_denied"));
        }
        let profile =
            serde_json::from_value::<Vec<NativeModelProfile>>(session.native_profiles_json.clone())
                .ok()
                .and_then(|profiles| profiles.into_iter().find(|p| p.permits(&requirements)))
                .ok_or_else(|| error("native_stream_capability_denied"))?;
        let current = StreamRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_native_streams WHERE task_id=$1 AND lease_id=$2 FOR UPDATE",
            [r.task_id.into(), r.lease_id.into()],
        ))
        .one(&tx)
        .await?;
        if let Some(row) = &current {
            if r.seq < row.next_seq as u64 {
                let found=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                    "SELECT 1 FROM node_native_stream_events WHERE task_id=$1 AND lease_id=$2 AND seq=$3 AND event_hash=$4",
                    [r.task_id.into(),r.lease_id.into(),r.seq.into(),hash.clone().into()])).await?.is_some();
                if !found {
                    return Err(error("native_stream_sequence_conflict"));
                }
                return Ok(NodeTaskStreamEventResponse {
                    accepted: true,
                    next_seq: r.seq + 1,
                    retry_after_ms: None,
                    terminal: row.state != "open" || task.is_terminal(),
                });
            }
            if r.seq > row.next_seq as u64 {
                return Err(error("native_stream_sequence_gap"));
            }
        }
        if task.is_terminal() {
            return Err(error("native_stream_already_terminal"));
        }
        let live = tx
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 FROM node_tasks WHERE id=$1 AND status='leased' AND deadline_at>NOW()",
                [r.task_id.into()],
            ))
            .await?
            .is_some();
        if !live {
            return Err(error("native_stream_deadline"));
        }
        let mut row = if let Some(row) = current {
            row
        } else {
            let NodeNativeStreamEvent::Start {
                status,
                headers,
                body,
            } = &r.event
            else {
                return Err(error("native_stream_must_start_at_seq_zero"));
            };
            if r.seq != 0
                || *status != 200
                || body.is_some()
                || headers.len() > 64
                || !headers.iter().any(|(n, v)| {
                    n.eq_ignore_ascii_case("content-type")
                        && v.split(';').next().is_some_and(|mime| {
                            mime.trim().eq_ignore_ascii_case("text/event-stream")
                        })
                })
                || headers
                    .iter()
                    .any(|(n, v)| !stream_response_header_allowed(n, v))
            {
                return Err(error("native_stream_start_invalid"));
            }
            StreamRow {
                next_seq: 0,
                unread_frames: 0,
                unread_bytes: 0,
                total_bytes: 0,
                state: "open".into(),
                inspector_json: serde_json::to_value(NativeStreamInspector::new(stream_protocol(
                    native.operation,
                )))
                .unwrap(),
                head_status: 200,
                head_headers: serde_json::to_value(headers).unwrap(),
                summary_json: None,
            }
        };
        if r.seq != row.next_seq as u64 {
            return Err(error("native_stream_sequence_gap"));
        }
        if row.state != "open" {
            return Ok(NodeTaskStreamEventResponse {
                accepted: false,
                next_seq: r.seq,
                retry_after_ms: None,
                terminal: true,
            });
        }
        let final_event = matches!(
            r.event,
            NodeNativeStreamEvent::Terminal { .. } | NodeNativeStreamEvent::Failed { .. }
        );
        if !final_event
            && (row.unread_frames >= 64
                || (row.unread_frames > 0 && row.unread_bytes + encoded.len() as i64 > 512 * 1024))
        {
            return Ok(NodeTaskStreamEventResponse {
                accepted: false,
                next_seq: r.seq,
                retry_after_ms: Some(50),
                terminal: false,
            });
        }
        let mut inspector: NativeStreamInspector = serde_json::from_value(row.inspector_json)
            .map_err(|_| error("native_stream_state_invalid"))?;
        let head_headers: Vec<(String, String)> = serde_json::from_value(row.head_headers.clone())
            .map_err(|_| error("native_stream_state_invalid"))?;
        match &r.event {
            NodeNativeStreamEvent::Start { .. } if r.seq != 0 => {
                return Err(error("native_stream_duplicate_start"));
            }
            NodeNativeStreamEvent::Start { .. } => {}
            NodeNativeStreamEvent::Data { frame } => {
                if inspector.is_terminal() {
                    return Err(error("native_stream_already_terminal"));
                }
                if frame.len() > MAX_NATIVE_SSE_FRAME_BYTES {
                    return Err(error("native_stream_frame_limit"));
                }
                row.total_bytes += frame.len() as i64;
                if row.total_bytes > MAX_NATIVE_SSE_TOTAL_BYTES as i64
                    || row.total_bytes > i64::from(profile.max_response_bytes)
                {
                    return Err(error("native_stream_total_limit"));
                }
                let mut decoder = BoundedSseDecoder::default();
                let mut frames = decoder
                    .push(frame.as_bytes())
                    .map_err(|_| error("native_stream_frame_invalid"))?;
                if let Some(last) = decoder
                    .finish()
                    .map_err(|_| error("native_stream_frame_invalid"))?
                {
                    frames.push(last);
                }
                if frames.len() != 1 || frames[0].raw != *frame {
                    return Err(error("native_stream_frame_invalid"));
                }
                inspector.observe_for_model(&frames[0], Some(&task.model));
            }
            NodeNativeStreamEvent::Terminal { summary } => {
                let verified = inspector.summary(200, head_headers.clone());
                if verified.usage.as_ref().is_some_and(|usage| {
                    usage.output_exact
                        && profile
                            .max_output_tokens
                            .is_some_and(|max| usage.output_tokens > max)
                }) {
                    return Err(error("native_stream_total_limit"));
                }
                if !inspector.is_terminal() || inspector.is_failed() || *summary != verified {
                    return Err(error("native_stream_terminal_mismatch"));
                }
                row.summary_json = Some(serde_json::to_value(verified).unwrap());
                row.state = "terminal".into();
            }
            NodeNativeStreamEvent::Failed {
                code,
                message,
                usage,
            } => {
                if inspector.is_terminal() && !inspector.is_failed() {
                    return Err(error("native_stream_already_terminal"));
                }
                if code.len() > 128 || message.len() > 1024 {
                    return Err(error("native_stream_error_limit"));
                }
                let mut verified = inspector.summary(200, head_headers);
                if *usage != verified.usage {
                    return Err(error("native_stream_usage_mismatch"));
                }
                verified.terminal_outcome = Some(NativeStreamTerminalOutcome::Failed);
                row.summary_json = Some(serde_json::to_value(verified).unwrap());
                row.state = "failed".into();
            }
        }
        let inspector_json =
            serde_json::to_value(inspector).map_err(|_| error("native_stream_encoding"))?;
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO node_native_streams(task_id,lease_id,node_id,session_id,inspector_json,head_status,head_headers) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(task_id,lease_id) DO NOTHING",
            [r.task_id.into(),r.lease_id.into(),r.node_id.into(),r.session_id.into(),inspector_json.clone().into(),row.head_status.into(),row.head_headers.into()])).await?;
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO node_native_stream_events(task_id,lease_id,seq,event_json,event_hash,event_bytes) VALUES($1,$2,$3,$4,$5,$6)",
            [r.task_id.into(),r.lease_id.into(),r.seq.into(),value.into(),hash.into(),(encoded.len() as i32).into()])).await?;
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE node_native_streams SET next_seq=next_seq+1,unread_frames=unread_frames+1,unread_bytes=unread_bytes+$3,total_bytes=$4,inspector_json=$5,summary_json=$6,state=$7,updated_at=NOW() WHERE task_id=$1 AND lease_id=$2",
            [r.task_id.into(),r.lease_id.into(),(encoded.len() as i64).into(),row.total_bytes.into(),inspector_json.into(),row.summary_json.into(),row.state.clone().into()])).await?;
        tx.commit().await?;
        Ok(NodeTaskStreamEventResponse {
            accepted: true,
            next_seq: r.seq + 1,
            retry_after_ms: None,
            terminal: row.state != "open",
        })
    }
    pub async fn read_native_stream_events(
        &self,
        task_id: Uuid,
        after: Option<u64>,
    ) -> Result<Vec<StoredNativeEvent>, DbError> {
        let rows=self.pool().write_conn().query_all(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT lease_id,seq,event_json FROM node_native_stream_events WHERE task_id=$1 AND seq>$2 AND event_json IS NOT NULL ORDER BY seq LIMIT 16",
            [task_id.into(),after.map(|n|n as i64).unwrap_or(-1).into()])).await?;
        rows.into_iter()
            .map(|r| {
                Ok(StoredNativeEvent {
                    lease_id: r.try_get("", "lease_id")?,
                    seq: r.try_get::<i64>("", "seq")? as u64,
                    event: serde_json::from_value(r.try_get("", "event_json")?)
                        .map_err(|_| error("native_stream_event_corrupt"))?,
                })
            })
            .collect()
    }
    pub async fn acknowledge_native_stream_event(
        &self,
        task_id: Uuid,
        lease_id: Uuid,
        seq: u64,
    ) -> Result<(), DbError> {
        let tx = self.pool().begin().await?;
        tx.execute_unprepared(
            "SET LOCAL statement_timeout='1500ms'; SET LOCAL lock_timeout='500ms'",
        )
        .await?;
        let stream = tx
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT lease_id FROM node_native_streams WHERE task_id=$1 FOR UPDATE",
                [task_id.into()],
            ))
            .await?
            .ok_or_else(|| error("native_stream_not_found"))?;
        let stored_lease: Uuid = stream.try_get("", "lease_id")?;
        if stored_lease != lease_id {
            return Err(error("native_stream_lease_mismatch"));
        }
        let event=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT event_bytes,consumed_at FROM node_native_stream_events WHERE task_id=$1 AND lease_id=$2 AND seq=$3 FOR UPDATE",
            [task_id.into(),lease_id.into(),seq.into()])).await?
            .ok_or_else(||error("native_stream_sequence_conflict"))?;
        let consumed_at: Option<chrono::DateTime<chrono::Utc>> =
            event.try_get("", "consumed_at")?;
        if consumed_at.is_none() {
            let bytes: i32 = event.try_get("", "event_bytes")?;
            tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "UPDATE node_native_stream_events SET consumed_at=NOW(),event_json=NULL WHERE task_id=$1 AND lease_id=$2 AND seq=$3",
                [task_id.into(),lease_id.into(),seq.into()])).await?;
            tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "UPDATE node_native_streams SET unread_frames=unread_frames-1,unread_bytes=unread_bytes-$3,updated_at=NOW() WHERE task_id=$1 AND lease_id=$2",
                [task_id.into(),lease_id.into(),(bytes as i64).into()])).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn native_stream_summary(
        &self,
        task_id: Uuid,
    ) -> Result<Option<NodeNativeStreamSummary>, DbError> {
        let row=self.pool().write_conn().query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT inspector_json,head_status,head_headers,summary_json FROM node_native_streams WHERE task_id=$1 ORDER BY created_at DESC LIMIT 1",[task_id.into()])).await?;
        row.map(|row| {
            if let Some(value) = row.try_get::<Option<Value>>("", "summary_json")? {
                return serde_json::from_value(value)
                    .map_err(|_| error("native_stream_summary_corrupt"));
            }
            let inspector: NativeStreamInspector =
                serde_json::from_value(row.try_get("", "inspector_json")?)
                    .map_err(|_| error("native_stream_state_corrupt"))?;
            Ok(inspector.summary(
                row.try_get::<i32>("", "head_status")? as u16,
                serde_json::from_value(row.try_get("", "head_headers")?)
                    .map_err(|_| error("native_stream_headers_corrupt"))?,
            ))
        })
        .transpose()
    }
    pub async fn cancel_native_stream(&self, task_id: Uuid, reason: &str) -> Result<(), DbError> {
        let tx = self.pool().begin().await?;
        tx.execute_unprepared(
            "SET LOCAL statement_timeout='1500ms'; SET LOCAL lock_timeout='500ms'",
        )
        .await?;
        let task = NodeTask::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_tasks WHERE id=$1 FOR UPDATE",
            [task_id.into()],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| DbError::not_found("NodeTask", task_id.to_string()))?;
        let stream=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT * FROM node_native_streams WHERE task_id=$1 ORDER BY created_at DESC LIMIT 1 FOR UPDATE",[task_id.into()])).await?;
        // A durable successful terminal event is still waiting for /complete.
        // Leave that state untouched so a retry can finish the same attempt.
        if stream.as_ref().is_some_and(|row| {
            row.try_get::<String>("", "state").ok().as_deref() == Some("terminal")
        }) {
            tx.commit().await?;
            return Ok(());
        }
        let reason = reason.chars().take(128).collect::<String>();
        if matches!(task.status.as_str(), "queued" | "leased") {
            tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "UPDATE node_tasks SET status='failed',error_json=$2,finished_at=NOW(),updated_at=NOW() WHERE id=$1 AND status IN ('queued','leased')",
                [task_id.into(),serde_json::json!({"code":reason,"message":"native stream interrupted"}).into()])).await?;
        }
        if let Some(row) = stream {
            let state: String = row.try_get("", "state")?;
            if state == "open" {
                let inspector: NativeStreamInspector =
                    serde_json::from_value(row.try_get("", "inspector_json")?)
                        .map_err(|_| error("native_stream_state_invalid"))?;
                let headers: Vec<(String, String)> =
                    serde_json::from_value(row.try_get("", "head_headers")?)
                        .map_err(|_| error("native_stream_state_invalid"))?;
                let mut summary =
                    inspector.summary(row.try_get::<i32>("", "head_status")? as u16, headers);
                summary.terminal_outcome = Some(NativeStreamTerminalOutcome::Failed);
                tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                    "UPDATE node_native_streams SET state='canceled',summary_json=$2,updated_at=NOW() WHERE task_id=$1 AND state='open'",
                    [task_id.into(),serde_json::to_value(summary).map_err(|_|error("native_stream_encoding"))?.into()])).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn cleanup_native_streams(&self) -> Result<u64, DbError> {
        let tx = self.pool().begin().await?;
        tx.execute_unprepared(
            "SET LOCAL statement_timeout='1500ms'; SET LOCAL lock_timeout='500ms'",
        )
        .await?;
        let result=tx.execute(Statement::from_string(DbBackend::Postgres,
            "DELETE FROM node_native_streams WHERE (task_id,lease_id) IN (SELECT s.task_id,s.lease_id FROM node_native_streams s JOIN node_tasks t ON t.id=s.task_id WHERE t.status IN ('succeeded','failed','expired') AND s.updated_at<NOW()-INTERVAL '1 hour' ORDER BY s.updated_at LIMIT 128)".to_owned())).await?;
        tx.commit().await?;
        Ok(result.rows_affected())
    }
}
