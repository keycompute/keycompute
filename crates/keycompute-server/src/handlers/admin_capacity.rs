//! Process resource diagnostics, protected by the same admin permission as
//! other monitoring endpoints. Never export principal IDs or connection URLs.
use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    state::AppState,
};
use axum::{Json, extract::State};
use serde_json::{Value, json};

fn admission(value: keycompute_runtime::admission::AdmissionStatus) -> Value {
    json!({"active":value.active,"queued":value.queued,"keys":value.keys})
}
macro_rules! redis_status {
    ($value:expr) => {{
        let value = $value;
        json!({"connections":value.size,"available":value.available,"waiting":value.waiting,"limit":value.max_size})
    }};
}
pub fn process_snapshot(state: &AppState) -> Value {
    let memory = keycompute_types::memory::process_memory_budget().status();
    let writer = state.pool.as_ref().and_then(|db| match db.write_conn() {
        sea_orm::DatabaseConnection::SqlxPostgresPoolConnection(_) => {
            let pool = db.write_conn().get_postgres_connection_pool();
            Some(json!({"connections":pool.size(),"idle":pool.num_idle()}))
        }
        _ => None,
    });
    let blocked = state.node_gateway.as_ref().map(|node| {
        let (poll, result) = node.blocking_pool_status();
        json!({"claims":redis_status!(poll),"results":redis_status!(result)})
    });
    json!({
        "scope":"application_process", "shutdown":state.shutdown.snapshot(), "managed_payload_bytes":{"limit":memory.limit,"used":memory.used,"peak":memory.peak},
        "ingress":admission(state.generation_admission.ingress.status()),
        "generation":admission(state.generation_admission.requests.status()),
        "console":state.console_admission.metrics(),
        "display_cache":state.display_cache.metrics(),
        "accounts_local":admission(state.generation_admission.accounts.status()),
        "balance_reservations":admission(keycompute_billing::balance::BalanceService::request_reservation_status()),
        "balance_settlements":admission(keycompute_billing::balance::BalanceService::settlement_status()),
        "writer_pool":writer,
        "redis_commands":state.runtime_state.pool().map(|pool|redis_status!(pool.status())),
        "redis_cache":state.cache.pool().map(|pool|redis_status!(pool.status())),
        "redis_blocking":blocked,
        "account_lease_owners":keycompute_observability::account_leases::snapshot(),
        "stages":keycompute_observability::capacity::snapshot(),
    })
}
pub async fn capacity(State(state): State<AppState>, auth: AuthExtractor) -> Result<Json<Value>> {
    if !auth.has_permission(&keycompute_auth::Permission::PlatformDiagnostics) {
        return Err(ApiError::Forbidden(
            "System administration permission is required".into(),
        ));
    }
    Ok(Json(process_snapshot(&state)))
}
#[cfg(test)]
mod tests {
    use super::*;
    use keycompute_types::CredentialKind;
    #[tokio::test]
    async fn role_text_cannot_bypass_capacity_permission() {
        let state = AppState::new();
        let id = uuid::Uuid::new_v4();
        let mut auth = AuthExtractor::new(id, id, id, CredentialKind::Jwt);
        auth.permissions = vec![keycompute_auth::Permission::UseApi];
        assert!(matches!(
            capacity(State(state.clone()), auth).await,
            Err(ApiError::Forbidden(_))
        ));
        let response = process_snapshot(&state);
        assert_eq!(response["scope"], "application_process");
        assert!(response.get("stages").unwrap().is_array());
        assert!(!response.to_string().contains("redis://"));
    }
}
