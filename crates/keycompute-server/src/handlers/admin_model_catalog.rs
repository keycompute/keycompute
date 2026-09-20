//! Bounded, tenant-scoped model-management read APIs.
//!
//! These queries deliberately aggregate model candidates in PostgreSQL before
//! pagination. The catalog is diagnostic metadata; it never exposes upstream
//! endpoints, credentials, or node-owner identifiers.

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    handlers::pagination::{normalize_list_pagination, total_pages},
    state::AppState,
};
use axum::{
    Json,
    extract::{Query, State},
};
use keycompute_auth::Permission;
use keycompute_types::{ModelAccessMode, ModelAvailability, ModelCatalogEntry, ModelCatalogPage};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::Deserialize;
use std::time::Duration;
use uuid::Uuid;

const MAX_PAGE_SIZE: i64 = 100;
const DB_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Deserialize)]
pub struct ModelCatalogQuery {
    pub mode: Option<ModelAccessMode>,
    pub tenant_id: Option<Uuid>,
    pub protocol: Option<String>,
    pub capability: Option<String>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub q: Option<String>,
}

#[derive(Debug, FromQueryResult)]
struct NodeCatalogRow {
    model: String,
    configured_targets: i64,
    eligible_targets: i64,
}

fn require_admin(auth: &AuthExtractor) -> Result<()> {
    if !auth.has_permission(&Permission::SystemAdmin) {
        return Err(ApiError::Forbidden("Admin permission required".into()));
    }
    Ok(())
}

fn target_tenant(auth: &AuthExtractor, requested: Option<Uuid>) -> Result<Uuid> {
    match requested {
        None => Ok(auth.tenant_id),
        Some(id) if id == auth.tenant_id => Ok(id),
        Some(_) if auth.is_admin() => Ok(requested.expect("matched Some")),
        Some(_) => Err(ApiError::Forbidden(
            "Cross-tenant catalog access is not permitted".into(),
        )),
    }
}

pub(crate) fn protocol_capability(
    _mode: ModelAccessMode,
    protocol: Option<&str>,
    capability: Option<&str>,
) -> Result<(String, String)> {
    let protocol = protocol.unwrap_or("openai").to_ascii_lowercase();
    let capability = capability
        .unwrap_or(if protocol == "anthropic" {
            "messages"
        } else {
            "chat_completions"
        })
        .to_ascii_lowercase();
    let valid = matches!(
        (protocol.as_str(), capability.as_str()),
        ("openai", "chat_completions" | "responses") | ("anthropic", "messages")
    );
    if !valid {
        return Err(ApiError::BadRequest(
            "Unsupported protocol/capability combination".into(),
        ));
    }
    Ok((protocol, capability))
}

fn escaped_q(q: Option<&str>) -> Option<String> {
    q.map(str::trim).filter(|s| !s.is_empty()).map(|s| {
        s.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    })
}

async fn query_rows<T: FromQueryResult>(
    db: &impl ConnectionTrait,
    stmt: Statement,
) -> Result<Vec<T>> {
    tokio::time::timeout(DB_TIMEOUT, T::find_by_statement(stmt).all(db))
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Model catalog metadata unavailable".into()))?
        .map_err(|_| ApiError::ServiceUnavailable("Model catalog metadata unavailable".into()))
}

pub async fn model_catalog(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Query(params): Query<ModelCatalogQuery>,
) -> Result<Json<ModelCatalogPage>> {
    require_admin(&auth)?;
    let mode = params.mode.unwrap_or_default();
    let tenant_id = target_tenant(&auth, params.tenant_id)?;
    let (protocol, capability) = protocol_capability(
        mode,
        params.protocol.as_deref(),
        params.capability.as_deref(),
    )?;
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Model catalog metadata unavailable".into()))?;
    let (page, page_size, offset) =
        normalize_list_pagination(params.page, params.page_size, None, None);
    let page_size = page_size.clamp(1, MAX_PAGE_SIZE);
    let q = escaped_q(params.q.as_deref());
    let (entries, total) = match mode {
        ModelAccessMode::AccountPool => {
            account_pool_catalog(
                &state,
                tenant_id,
                &protocol,
                &capability,
                q.as_deref(),
                page_size,
                offset,
            )
            .await?
        }
        ModelAccessMode::Passthrough => {
            passthrough_catalog(
                &state,
                tenant_id,
                &capability,
                q.as_deref(),
                page_size,
                offset,
            )
            .await?
        }
        ModelAccessMode::NodeDispatch => {
            node_catalog(
                &state,
                tenant_id,
                &capability,
                q.as_deref(),
                page_size,
                offset,
            )
            .await?
        }
    };
    Ok(Json(ModelCatalogPage {
        mode,
        tenant_id: tenant_id.to_string(),
        entries,
        page: page as u64,
        page_size: page_size as u64,
        total: total.max(0) as u64,
        total_pages: total_pages(total.max(0), page_size) as u64,
        observed_at: crate::passthrough_binding::database_now(pool.write_conn())
            .await?
            .to_rfc3339(),
    }))
}

async fn account_pool_catalog(
    state: &AppState,
    tenant: Uuid,
    protocol: &str,
    capability: &str,
    q: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<(Vec<ModelCatalogEntry>, i64)> {
    let db = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Model catalog unavailable".into()))?
        .write_conn();
    let cooling: Vec<Uuid> = state
        .account_states
        .cooling_accounts()
        .into_iter()
        .map(|v| v.0)
        .collect();
    let overlay = state.provider_health.catalog_health_overlay();
    let sql = r#"
      WITH local AS (SELECT * FROM jsonb_to_recordset($7::JSONB) AS h(id UUID,status TEXT,updated_at TIMESTAMPTZ,generation BIGINT,configuration_updated_at TIMESTAMPTZ)),
      candidates AS (
        SELECT DISTINCT a.id,m.model,a.enabled,
          (owner.status='active' AND EXISTS(SELECT 1 FROM tenants t WHERE t.id=$1 AND t.status='active')) AS active,
          (CASE WHEN h.id IS NOT NULL THEN h.status ELSE a.health_status END <> 'unhealthy') AS healthy,
          NOT(a.id=ANY($8::UUID[])) AS not_cooling
        FROM accounts a JOIN tenants owner ON owner.id=a.tenant_id
        CROSS JOIN LATERAL unnest(a.models_supported) AS m(model)
        LEFT JOIN local h ON h.id=a.id AND (h.configuration_updated_at IS NULL OR h.configuration_updated_at=a.upstream_config_version)
          AND (h.updated_at>a.health_updated_at OR (h.updated_at=a.health_updated_at AND h.generation>=a.health_generation))
        WHERE (a.tenant_id=$1 OR a.visibility='global') AND a.pool_enabled
          AND a.provider=$2 AND a.api_capabilities @> ARRAY[$3]::TEXT[]
          AND m.model<>''
          AND ($4::TEXT IS NULL OR m.model ILIKE '%'||$4||'%')
      ), grouped AS (
        SELECT model,COUNT(*)::BIGINT configured_targets,
          COUNT(*) FILTER(WHERE enabled AND active AND healthy AND not_cooling)::BIGINT eligible_targets,
          COUNT(*) FILTER(WHERE enabled AND active)::BIGINT enabled_targets
        FROM candidates GROUP BY model
      ), paged AS (SELECT * FROM grouped ORDER BY model LIMIT $5 OFFSET $6)
      SELECT totals.total,paged.* FROM (SELECT count(*)::BIGINT total FROM grouped) totals LEFT JOIN paged ON TRUE ORDER BY model
    "#;
    let sql = sql.replace(
        "(a.tenant_id=$1 OR a.visibility='global') AND a.pool_enabled",
        &keycompute_db::models::upstream_access::non_pt_predicate("a", "$1"),
    );
    let result = tokio::time::timeout(
        DB_TIMEOUT,
        db.query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [
                tenant.into(),
                protocol.into(),
                capability.into(),
                q.into(),
                limit.into(),
                offset.into(),
                overlay.into(),
                cooling.into(),
            ],
        )),
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Model catalog unavailable".into()))?
    .map_err(|error| {
        tracing::warn!(%error,"pool catalog query failed");
        ApiError::ServiceUnavailable("Model catalog unavailable".into())
    })?;
    let total = result
        .first()
        .map(|r| r.try_get::<i64>("", "total"))
        .transpose()
        .map_err(|_| ApiError::Internal("Invalid catalog count".into()))?
        .unwrap_or(0);
    let mut entries = Vec::new();
    for row in result {
        let model: Option<String> = row
            .try_get("", "model")
            .map_err(|_| ApiError::Internal("Invalid model catalog row".into()))?;
        let Some(model) = model else {
            continue;
        };
        let configured: i64 = row
            .try_get("", "configured_targets")
            .map_err(|_| ApiError::Internal("Invalid catalog count".into()))?;
        let eligible: i64 = row
            .try_get("", "eligible_targets")
            .map_err(|_| ApiError::Internal("Invalid catalog count".into()))?;
        let enabled: i64 = row
            .try_get("", "enabled_targets")
            .map_err(|_| ApiError::Internal("Invalid catalog count".into()))?;
        let status = if eligible > 0 {
            ModelAvailability::Ready
        } else if enabled == 0 {
            ModelAvailability::Disabled
        } else {
            ModelAvailability::Unavailable
        };
        entries.push(ModelCatalogEntry {
            model: model.clone(),
            request_model: model,
            protocol: protocol.into(),
            capability: capability.into(),
            request_path: match capability {
                "responses" => "/v1/responses",
                "messages" => "/v1/messages",
                _ => "/v1/chat/completions",
            }
            .into(),
            status,
            reason_code: if eligible > 0 {
                "pool_eligible"
            } else if enabled == 0 {
                "no_active_account"
            } else {
                "no_eligible_account"
            }
            .into(),
            configured_targets: configured as u64,
            eligible_targets: eligible as u64,
            binding_id: None,
            binding_revision: None,
            account_id: None,
            account_name: None,
            pool_enabled: Some(true),
            health_expires_at: None,
        });
    }
    Ok((entries, total))
}

async fn passthrough_catalog(
    state: &AppState,
    tenant: Uuid,
    capability: &str,
    q: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<(Vec<ModelCatalogEntry>, i64)> {
    let result = crate::passthrough_binding::DbPassthroughBindingValidator::for_capability(
        state,
        keycompute_types::AccountApiCapability::parse(capability).expect("validated capability"),
    )?
    .catalog_page(tenant, q, offset / limit + 1, limit)
    .await?;
    Ok((result.entries, result.total as i64))
}

fn node_supply_sql(operation: keycompute_types::node_native::NodeNativeOperation) -> String {
    format!(
        r#"
      SELECT DISTINCT n.id,m.model,
        EXISTS(SELECT 1 FROM node_sessions ns WHERE ns.node_id=n.id
          AND {ready} AND ns.accepted_models_json @> jsonb_build_array(m.model) AND {profile}) AS ready
      FROM nodes n JOIN users u ON u.id=n.owner_user_id JOIN tenants t ON t.id=u.tenant_id
      CROSS JOIN LATERAL (
        SELECT v->>'model' AS model FROM jsonb_array_elements(
          CASE WHEN jsonb_typeof(n.capabilities_json->'models')='array' THEN n.capabilities_json->'models' ELSE '[]'::JSONB END) v
        UNION SELECT jsonb_array_elements_text(ns.accepted_models_json) FROM node_sessions ns
          WHERE ns.node_id=n.id AND ns.expires_at>NOW() AND ns.revoked_at IS NULL
      ) m
      WHERE m.model IS NOT NULL AND m.model<>'' AND n.capabilities_json->>'runtime'='ollama'
    "#,
        ready = node_gateway::node_index::READY_NODE_CONDITION,
        profile = node_gateway::node_index::ready_profile_condition(
            "m.model",
            &format!("'{}'", operation.as_str())
        )
    )
}
async fn node_catalog(
    state: &AppState,
    tenant: Uuid,
    capability: &str,
    q: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<(Vec<ModelCatalogEntry>, i64)> {
    let db = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Node catalog unavailable".into()))?
        .write_conn();
    let operation = operation_for_capability(capability);
    let sql = format!(
        r#"WITH supply AS ({}), grouped AS (
        SELECT model,COUNT(DISTINCT id)::BIGINT configured_targets,
          COUNT(DISTINCT id) FILTER(WHERE ready AND EXISTS(SELECT 1 FROM tenants t WHERE t.id=$1 AND t.status='active'))::BIGINT eligible_targets
        FROM supply WHERE ($2::TEXT IS NULL OR model ILIKE '%'||$2||'%') GROUP BY model
      ), paged AS (SELECT * FROM grouped ORDER BY model LIMIT $3 OFFSET $4)
      SELECT totals.total,paged.* FROM (SELECT count(*)::BIGINT total FROM grouped) totals LEFT JOIN paged ON TRUE ORDER BY model"#,
        node_supply_sql(operation)
    );
    let result = tokio::time::timeout(
        DB_TIMEOUT,
        db.query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [tenant.into(), q.into(), limit.into(), offset.into()],
        )),
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Node catalog unavailable".into()))?
    .map_err(|error| {
        tracing::warn!(%error,"node catalog failed");
        ApiError::ServiceUnavailable("Node catalog unavailable".into())
    })?;
    let total = result
        .first()
        .map(|r| r.try_get::<i64>("", "total"))
        .transpose()
        .map_err(|_| ApiError::Internal("Invalid catalog count".into()))?
        .unwrap_or(0);
    let mut entries = Vec::new();
    for row in result {
        if row
            .try_get::<Option<String>>("", "model")
            .map_err(|_| ApiError::Internal("Invalid node model".into()))?
            .is_none()
        {
            continue;
        }
        let r = NodeCatalogRow::from_query_result(&row, "")
            .map_err(|_| ApiError::Internal("Invalid node catalog".into()))?;
        let ready = r.eligible_targets > 0 && state.node_gateway.is_some();
        entries.push(ModelCatalogEntry {
            model: r.model.clone(),
            request_model: r.model.clone(),
            protocol: operation.protocol().into(),
            capability: capability.into(),
            request_path: format!("/nt{}", operation.local_path()),
            status: if ready {
                ModelAvailability::Ready
            } else {
                ModelAvailability::Unavailable
            },
            reason_code: if ready { "node_ready" } else { "no_ready_node" }.into(),
            configured_targets: r.configured_targets as u64,
            eligible_targets: if ready { r.eligible_targets as u64 } else { 0 },
            binding_id: None,
            binding_revision: None,
            account_id: None,
            account_name: None,
            pool_enabled: None,
            health_expires_at: None,
        });
    }
    Ok((entries, total))
}

fn operation_for_capability(
    capability: &str,
) -> keycompute_types::node_native::NodeNativeOperation {
    use keycompute_types::node_native::NodeNativeOperation as Op;
    match capability {
        "messages" => Op::Messages,
        "responses" => Op::Responses,
        _ => Op::Chat,
    }
}
pub(crate) async fn ready_node_models_for(
    state: &AppState,
    tenant: Uuid,
    capability: &str,
) -> Result<Vec<String>> {
    if state.node_gateway.is_none() {
        return Ok(Vec::new());
    }
    let db = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Node catalog unavailable".into()))?
        .write_conn();
    let sql = format!(
        "WITH supply AS ({}) SELECT DISTINCT model FROM supply WHERE ready AND EXISTS(SELECT 1 FROM tenants t WHERE t.id=$1 AND t.status='active') ORDER BY model",
        node_supply_sql(operation_for_capability(capability))
    );
    #[derive(FromQueryResult)]
    struct NodeModel {
        model: String,
    }
    Ok(query_rows::<NodeModel>(
        db,
        Statement::from_sql_and_values(DbBackend::Postgres, sql, [tenant.into()]),
    )
    .await?
    .into_iter()
    .map(|r| r.model)
    .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn access_mode_and_protocol_are_independent() {
        assert_eq!(
            protocol_capability(ModelAccessMode::AccountPool, Some("anthropic"), None)
                .unwrap()
                .1,
            "messages"
        );
        assert_eq!(
            protocol_capability(ModelAccessMode::Passthrough, None, Some("responses"))
                .unwrap()
                .1,
            "responses"
        );
        assert_eq!(
            protocol_capability(ModelAccessMode::NodeDispatch, Some("anthropic"), None)
                .unwrap()
                .1,
            "messages"
        );
        for mode in [
            ModelAccessMode::AccountPool,
            ModelAccessMode::Passthrough,
            ModelAccessMode::NodeDispatch,
        ] {
            assert!(protocol_capability(mode, Some("anthropic"), Some("responses")).is_err());
            assert!(protocol_capability(mode, Some("openai"), Some("messages")).is_err());
        }
        assert_eq!(escaped_q(Some("a_%")), Some("a\\_\\%".into()));
    }
}
