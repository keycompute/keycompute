//! Task administration does not transfer execution, lease or settlement ownership.
use super::*;
/// Lock only lifecycle metadata; task payload/result bodies are not needed
/// to authorize a cancellation or retain an archived accounting record.
#[derive(Debug, FromQueryResult)]
struct TaskControlRecord {
    id: Uuid,
    request_id: Uuid,
    tenant_id: Uuid,
    user_id: Uuid,
    status: String,
    native_requirements_json: Option<serde_json::Value>,
    lease_id: Option<Uuid>,
    cancellation_requested_at: Option<DateTime<Utc>>,
    archived_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskAction {
    Cancel,
    Archive,
}
#[derive(Debug, Clone, Copy)]
pub struct TaskMutation<'a> {
    pub id: Uuid,
    pub expected_updated_at: DateTime<Utc>,
    pub action: TaskAction,
    pub reason: &'a str,
}
#[derive(Debug, Clone, Serialize)]
pub struct TaskChange {
    pub task: TaskInfo,
    pub changed: bool,
    pub cancellation_requested: bool,
    pub archived: bool,
}
fn completed(status: &str) -> bool {
    matches!(status, "succeeded" | "failed" | "expired")
}
async fn locked_task(
    tx: &DatabaseTransaction,
    scope: NodeControlScope,
    id: Uuid,
) -> Result<TaskControlRecord, DbError> {
    let owner = match scope.authority {
        Authority::Owned(s, _) => Some(s.user_id()),
        _ => None,
    };
    TaskControlRecord::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT id,request_id,tenant_id,user_id,status,native_requirements_json,lease_id,cancellation_requested_at,archived_at,updated_at FROM node_tasks WHERE tenant_id=$1 AND id=$2 AND ($3::uuid IS NULL OR user_id=$3) FOR UPDATE",
        [scope.tenant.into(), id.into(), owner.into()])).one(tx).await?
        .ok_or_else(||DbError::not_found("Node task", id))
}
async fn metadata(tx: &DatabaseTransaction, row: &TaskControlRecord) -> Result<TaskInfo, DbError> {
    TaskInfo::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "SELECT {} FROM node_tasks r WHERE r.tenant_id=$1 AND r.user_id=$2 AND r.id=$3",
            NodeResource::Task.columns()
        ),
        [row.tenant_id.into(), row.user_id.into(), row.id.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| DbError::not_found("Node task", row.id))
}
/// A leased, cancellation-aware native worker observes the durable marker at
/// its lease-status endpoint. Keep the task leased until its authentic final
/// result arrives; a cancellation request must not prematurely release billing.
pub async fn change_task(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: NodeControlScope,
    audit: &AuditContext,
    mutation: TaskMutation<'_>,
) -> Result<TaskChange, DbError> {
    let reason = validate_reason(mutation.reason)?;
    if mutation.id.is_nil() {
        return Err(invalid("task ID required"));
    }
    let action = match mutation.action {
        TaskAction::Cancel => NodeAction::CancelTask,
        TaskAction::Archive => NodeAction::ArchiveTask,
    };
    let tx = begin(db).await?;
    let result = async {
        let (_, actor) = lock_for_action(&tx,scope,action,audit).await?;
        // Unlike node administration, this transaction never locks a node or
        // session row after this task. It shares completion's task-first order.
        let old = locked_task(&tx,scope,mutation.id).await?;
        if old.updated_at != mutation.expected_updated_at { return Err(conflict("Node task",old.id)); }
        let mut changed = false;
        match mutation.action {
            TaskAction::Archive => {
                if !completed(&old.status) { return Err(conflict("only terminal tasks can be archived",old.id)); }
                if old.archived_at.is_none() {
                    let updated = tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                        "UPDATE node_tasks SET archived_at=clock_timestamp(),archived_by=$5 WHERE tenant_id=$1 AND user_id=$2 AND id=$3 AND updated_at=$4 AND status IN ('succeeded','failed','expired') AND archived_at IS NULL",
                        [old.tenant_id.into(),old.user_id.into(),old.id.into(),old.updated_at.into(),actor.actor_user_id.into()])).await?;
                    if updated.rows_affected()!=1 { return Err(conflict("Node task",old.id)); }
                    changed = true;
                }
            }
            TaskAction::Cancel if !completed(&old.status) && old.cancellation_requested_at.is_none() => {
                if old.status == "leased" {
                    let cancellation_aware = old.native_requirements_json.as_ref()
                        .and_then(|v|v.get("features")).and_then(serde_json::Value::as_array)
                        .is_some_and(|features|features.iter().any(|v|v.as_str()==Some("cancellation")));
                    if !cancellation_aware { return Err(conflict("leased worker has no cancellation contract",old.id)); }
                    // A durable terminal result has already won. Do not turn a
                    // completed upstream response into an uncharged cancellation.
                    let terminal = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                        "SELECT state FROM node_native_streams WHERE task_id=$1 AND lease_id=$2 FOR UPDATE",
                        [old.id.into(),old.lease_id.into()])).await?;
                    if terminal.as_ref().is_some_and(|r|r.try_get::<String>("","state").ok().as_deref()==Some("terminal")) {
                        return Err(conflict("terminal native result is awaiting completion",old.id));
                    }
                } else if old.status != "queued" { return Err(conflict("task is not cancellable",old.id)); }
                let updated = tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                    "UPDATE node_tasks SET cancellation_requested_at=clock_timestamp(),cancellation_requested_by=$5,status=CASE WHEN status='queued' THEN 'failed' ELSE status END,finished_at=CASE WHEN status='queued' THEN clock_timestamp() ELSE finished_at END,error_json=CASE WHEN status='queued' THEN '{\"code\":\"task_cancelled_before_dispatch\",\"message\":\"Task cancelled before a worker lease\"}'::jsonb ELSE error_json END WHERE tenant_id=$1 AND user_id=$2 AND id=$3 AND updated_at=$4 AND status IN ('queued','leased') AND cancellation_requested_at IS NULL",
                    [old.tenant_id.into(),old.user_id.into(),old.id.into(),old.updated_at.into(),actor.actor_user_id.into()])).await?;
                if updated.rows_affected()!=1 { return Err(conflict("Node task",old.id)); }
                changed = true;
            }
            TaskAction::Cancel => {}
        }
        let current = metadata(&tx,&old).await?;
        TenantAuditEvent::append(&tx,AuditScopeType::Tenant,Some(scope.tenant),&actor,
            if mutation.action==TaskAction::Cancel {"node.task.cancel"} else {"node.task.archive"},
            "node_task",Some(&old.id.to_string()),AuditResult::Success,
            json!({"reason":reason,"user_id":old.user_id,"request_id":old.request_id,"changed":changed,
              "before":{"status":old.status,"archived":old.archived_at.is_some(),"cancellation_requested":old.cancellation_requested_at.is_some()},
              "after":{"status":current.status,"archived":current.archived_at.is_some(),"cancellation_requested":current.cancellation_requested_at.is_some()}})).await?;
        Ok(TaskChange { cancellation_requested:current.cancellation_requested_at.is_some(), archived:current.archived_at.is_some(),task:current,changed })
    }.await;
    finish(tx, result).await
}
