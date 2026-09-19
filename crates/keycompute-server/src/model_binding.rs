//! Exact tenant/model account binding and authoritative dispatch snapshots.
//!
//! Each lookup is a single writer statement: binding, tenant lifecycle,
//! account configuration and model health come from the same MVCC snapshot.
//! The final lookup after capacity admission is the authorization point; no
//! database locks are held across the upstream request or response stream.
use crate::state::AppState;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use keycompute_db::{Account, DbRouter, models::model_binding::AccountModelHealth};
use keycompute_routing::{AccountStateStore, ProviderHealthStore};
use keycompute_runtime::{EncryptedApiKey, decrypt_api_key};
use keycompute_types::{
    AccountModelHealthObserver, AccountModelHealthSnapshot, AccountSelection, ExecutionPlan,
    ExecutionTarget, KeyComputeError, ModelBindingError as Failure, ModelBindingSelection,
    ModelBindingValidator, ModelHealthObservation, RequestContext, Result,
};
use llm_protocol_provider::{ProtocolType, normalize_base_url};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

const STATE_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const HEALTH_TTL_SECS: i64 = 300;

const SNAPSHOT_SQL: &str = r#"
SELECT a.*, mb.id AS binding_id, mb.revision AS binding_revision,
       mb.enabled AS binding_enabled, mb.model AS binding_model,
       (caller.status='active') AS caller_active,
       (owner.status='active') AS owner_active,
       h.status AS model_status, h.checked_at AS model_checked_at,
       h.expires_at AS model_expires_at,
       h.account_config_version AS model_config_version,
       h.generation AS model_generation, statement_timestamp() AS database_now
FROM model_bindings mb
JOIN accounts a ON a.id=mb.account_id
JOIN tenants caller ON caller.id=mb.tenant_id
JOIN tenants owner ON owner.id=a.tenant_id
LEFT JOIN account_model_health h
  ON h.account_id=a.id AND h.api_capability=mb.api_capability AND h.model=mb.model
WHERE mb.tenant_id=$1 AND mb.api_capability='chat_completions'
  AND ($2::TEXT IS NULL OR mb.model=$2)
ORDER BY mb.model,mb.id
"#;

#[derive(FromQueryResult)]
struct BindingState {
    binding_id: Uuid,
    binding_revision: i64,
    binding_enabled: bool,
    binding_model: String,
    caller_active: bool,
    owner_active: bool,
    model_status: Option<String>,
    model_checked_at: Option<DateTime<Utc>>,
    model_expires_at: Option<DateTime<Utc>>,
    model_config_version: Option<DateTime<Utc>>,
    model_generation: Option<i64>,
    database_now: DateTime<Utc>,
}
struct Snapshot {
    account: Account,
    binding: BindingState,
}

pub(crate) struct DbModelBindingValidator {
    pool: Arc<DbRouter>,
    account_states: Arc<AccountStateStore>,
    account_health: Arc<ProviderHealthStore>,
}
impl DbModelBindingValidator {
    pub(crate) fn new(state: &AppState) -> Result<Self> {
        Ok(Self {
            pool: state.pool.clone().ok_or(Failure::DependencyUnavailable)?,
            account_states: Arc::clone(&state.account_states),
            account_health: Arc::clone(&state.provider_health),
        })
    }

    async fn snapshots(&self, tenant: Uuid, model: Option<&str>) -> Result<Vec<Snapshot>> {
        let rows = tokio::time::timeout(
            STATE_TIMEOUT,
            self.pool
                .write_conn()
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    SNAPSHOT_SQL,
                    [tenant.into(), model.into()],
                )),
        )
        .await
        .map_err(|_| Failure::DependencyUnavailable)?
        .map_err(|_| Failure::DependencyUnavailable)?;
        rows.iter()
            .map(|row| {
                Ok(Snapshot {
                    account: Account::from_query_result(row, "")
                        .map_err(|_| Failure::DependencyUnavailable)?,
                    binding: BindingState::from_query_result(row, "")
                        .map_err(|_| Failure::DependencyUnavailable)?,
                })
            })
            .collect()
    }

    /// Shared eligibility for discovery, initial resolution and final dispatch.
    fn eligible(&self, tenant: Uuid, snapshot: &Snapshot) -> Result<AccountModelHealthSnapshot> {
        let a = &snapshot.account;
        let b = &snapshot.binding;
        if a.tenant_id != tenant && a.visibility != "global" {
            return Err(Failure::NotFound.into());
        }
        if !b.caller_active || !b.owner_active || !b.binding_enabled || !a.enabled {
            return Err(Failure::Unavailable.into());
        }
        if a.provider != "openai"
            || !a.api_capabilities.iter().any(|v| v == "chat_completions")
            || !a.models_supported.iter().any(|v| v == &b.binding_model)
        {
            return Err(Failure::ModelNotSupported.into());
        }
        self.account_health.hydrate_account_health(a);
        if !self.account_health.account_is_routable(a) || self.account_states.is_cooling_down(&a.id)
        {
            return Err(Failure::Unavailable.into());
        }
        if b.model_config_version != Some(a.updated_at)
            || !b.model_checked_at.is_some_and(|t| t <= b.database_now)
            || !b.model_expires_at.is_some_and(|t| t > b.database_now)
        {
            return Err(Failure::HealthUnknown.into());
        }
        match b.model_status.as_deref() {
            Some("healthy") => {}
            Some("unhealthy" | "degraded") => return Err(Failure::ModelUnhealthy.into()),
            _ => return Err(Failure::HealthUnknown.into()),
        }
        Ok(AccountModelHealthSnapshot {
            account_id: a.id,
            api_capability: "chat_completions".into(),
            model: b.binding_model.clone(),
            account_config_version: a.updated_at,
            generation: b.model_generation.ok_or(Failure::HealthUnknown)?,
        })
    }
}

fn connection(account: &Account) -> Result<(String, String)> {
    let endpoint = if account.endpoint.is_empty() {
        ProtocolType::Openai.default_endpoint().to_string()
    } else {
        normalize_base_url(&account.endpoint).map_err(|_| Failure::Unavailable)?
    };
    let key = decrypt_api_key(&EncryptedApiKey::from(
        account.upstream_api_key_encrypted.as_str(),
    ))
    .map_err(|_| Failure::Unavailable)?;
    if key.is_empty() {
        return Err(Failure::Unavailable.into());
    }
    Ok((endpoint, key))
}

pub(crate) fn install_model_health_observer(ctx: &mut RequestContext, state: &AppState) {
    if let Ok(service) = DbModelBindingValidator::new(state) {
        ctx.set_account_model_health_observer(Arc::new(service));
    }
}

pub(crate) async fn resolve_model_binding_plan(
    state: &AppState,
    tenant: Uuid,
    model: &str,
) -> Result<(ExecutionPlan, ModelBindingSelection, DateTime<Utc>)> {
    if model.is_empty()
        || model.trim() != model
        || model.chars().count() > 255
        || model.to_ascii_lowercase().starts_with("node:")
    {
        return Err(KeyComputeError::InvalidRequest(
            "Invalid model for the model-binding endpoint".into(),
        ));
    }
    let service = DbModelBindingValidator::new(state)?;
    let snapshot = service
        .snapshots(tenant, Some(model))
        .await?
        .into_iter()
        .next()
        .ok_or(Failure::NotFound)?;
    service.eligible(tenant, &snapshot)?;
    let (endpoint, key) = connection(&snapshot.account)?;
    let binding = ModelBindingSelection {
        binding_id: snapshot.binding.binding_id,
        binding_revision: snapshot.binding.binding_revision,
    };
    let version = snapshot.account.updated_at;
    let target =
        ExecutionTarget::new_upstream_account("openai", snapshot.account.id, endpoint, key)
            .with_selection(AccountSelection::ModelBinding {
                binding_id: binding.binding_id,
                binding_revision: binding.binding_revision,
            });
    Ok((ExecutionPlan::new(target), binding, version))
}

pub(crate) async fn list_routable_model_bindings(
    state: &AppState,
    tenant: Uuid,
    model: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let service = DbModelBindingValidator::new(state)?;
    let rows = service.snapshots(tenant, model).await?;
    Ok(rows
        .into_iter()
        .filter(|row| service.eligible(tenant, row).is_ok())
        .map(|row| (row.binding.binding_model, row.account.provider))
        .collect())
}

#[async_trait::async_trait]
impl ModelBindingValidator for DbModelBindingValidator {
    async fn validate_target(
        &self,
        tenant: Uuid,
        model: &str,
        selection: ModelBindingSelection,
        target: &ExecutionTarget,
        account_config_version: DateTime<Utc>,
    ) -> Result<AccountModelHealthSnapshot> {
        let ExecutionTarget::UpstreamAccount {
            provider,
            account_id,
            endpoint,
            upstream_api_key,
            selection: actual_selection,
        } = target
        else {
            return Err(Failure::InvalidPlan.into());
        };
        if *actual_selection
            != (AccountSelection::ModelBinding {
                binding_id: selection.binding_id,
                binding_revision: selection.binding_revision,
            })
        {
            return Err(Failure::InvalidPlan.into());
        }
        let snapshot = self
            .snapshots(tenant, Some(model))
            .await?
            .into_iter()
            .next()
            .ok_or(Failure::Changed)?;
        if snapshot.binding.binding_id != selection.binding_id
            || snapshot.binding.binding_revision != selection.binding_revision
            || snapshot.account.id != *account_id
            || snapshot.account.updated_at != account_config_version
        {
            return Err(Failure::Changed.into());
        }
        let health = self.eligible(tenant, &snapshot)?;
        let (expected_endpoint, expected_key) = connection(&snapshot.account)?;
        if provider != "openai"
            || endpoint != &expected_endpoint
            || upstream_api_key.expose() != expected_key
        {
            return Err(Failure::Changed.into());
        }
        Ok(health)
    }
}

#[derive(FromQueryResult)]
struct TrackedModel {
    account_config_version: DateTime<Utc>,
    generation: i64,
}
#[derive(FromQueryResult)]
struct DatabaseClock {
    now: DateTime<Utc>,
}

/// Fetch writer time rather than comparing health against application clocks.
pub(crate) async fn database_now(db: &impl ConnectionTrait) -> Result<DateTime<Utc>> {
    tokio::time::timeout(
        STATE_TIMEOUT,
        DatabaseClock::find_by_statement(Statement::from_string(
            DbBackend::Postgres,
            "SELECT statement_timestamp() AS now".to_string(),
        ))
        .one(db),
    )
    .await
    .map_err(|_| Failure::DependencyUnavailable)?
    .map_err(|_| Failure::DependencyUnavailable)?
    .map(|row| row.now)
    .ok_or_else(|| Failure::DependencyUnavailable.into())
}

#[async_trait::async_trait]
impl AccountModelHealthObserver for DbModelBindingValidator {
    async fn snapshot(
        &self,
        target: &ExecutionTarget,
        api_capability: &str,
        model: &str,
    ) -> Result<Option<AccountModelHealthSnapshot>> {
        let ExecutionTarget::UpstreamAccount {
            account_id,
            provider,
            endpoint,
            upstream_api_key,
            ..
        } = target
        else {
            return Ok(None);
        };
        // One indexed join; do not enroll untracked models or attach an old
        // model probe to a newly changed connection configuration.
        let row=tokio::time::timeout(STATE_TIMEOUT, self.pool.write_conn().query_one(
            Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT a.*, a.updated_at AS account_config_version,h.generation FROM account_model_health h JOIN accounts a ON a.id=h.account_id JOIN tenants t ON t.id=a.tenant_id WHERE h.account_id=$1 AND h.api_capability=$2 AND h.model=$3 AND a.updated_at=h.account_config_version AND t.status='active'",
            [(*account_id).into(),api_capability.into(),model.into()])))
            .await.map_err(|_|Failure::DependencyUnavailable)?
            .map_err(|_|Failure::DependencyUnavailable)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let account =
            Account::from_query_result(&row, "").map_err(|_| Failure::DependencyUnavailable)?;
        let tracked = TrackedModel::from_query_result(&row, "")
            .map_err(|_| Failure::DependencyUnavailable)?;
        // Only an observation of this exact account connection may update its
        // model-health generation. A changed snapshot must not revive it.
        let current_endpoint = if account.endpoint.is_empty() {
            ProtocolType::parse(provider)
                .map(|p| p.default_endpoint().to_string())
                .unwrap_or_default()
        } else {
            account.endpoint.clone()
        };
        let current_key = if keycompute_runtime::global_crypto().is_some() {
            decrypt_api_key(&EncryptedApiKey::from(
                account.upstream_api_key_encrypted.as_str(),
            ))
            .map_err(|_| Failure::Unavailable)?
        } else {
            account.upstream_api_key_encrypted.clone()
        };
        if account.provider != *provider
            || current_endpoint != *endpoint
            || current_key != upstream_api_key.expose()
        {
            return Err(Failure::Changed.into());
        }
        Ok(Some(AccountModelHealthSnapshot {
            account_id: *account_id,
            api_capability: api_capability.into(),
            model: model.into(),
            account_config_version: tracked.account_config_version,
            generation: tracked.generation,
        }))
    }
    async fn observe(
        &self,
        snapshot: &AccountModelHealthSnapshot,
        observation: ModelHealthObservation,
    ) -> Result<()> {
        let (status, reason) = match observation {
            ModelHealthObservation::Healthy => ("healthy", None),
            ModelHealthObservation::Unhealthy { reason_code } => ("unhealthy", Some(reason_code)),
        };
        let now = database_now(self.pool.write_conn()).await?;
        tokio::time::timeout(
            STATE_TIMEOUT,
            AccountModelHealth::record_runtime_if_current(
                self.pool.write_conn(),
                snapshot.account_id,
                &snapshot.api_capability,
                &snapshot.model,
                snapshot.account_config_version,
                snapshot.generation,
                status,
                reason,
                now,
                now + ChronoDuration::seconds(HEALTH_TTL_SECS),
            ),
        )
        .await
        .map_err(|_| Failure::DependencyUnavailable)?
        .map_err(|_| Failure::DependencyUnavailable)?;
        Ok(())
    }
}
