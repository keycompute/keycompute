//! Account-grant resolution and last-moment authoritative access checks.
//! Bindings grant all declared models; health observations never activate or
//! expand a grant. No database lock is held over upstream HTTP or SSE.
use crate::state::AppState;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use keycompute_db::{Account, DbRouter, models::passthrough_binding::AccountModelHealth};
use keycompute_routing::{AccountStateStore, ProviderHealthStore};
use keycompute_runtime::{EncryptedApiKey, decrypt_api_key};
use keycompute_types::{
    AccountModelHealthObserver, AccountModelHealthSnapshot, AccountSelection, ExecutionPlan,
    ExecutionTarget, KeyComputeError, ModelHealthObservation, PassthroughBindingError as Failure,
    PassthroughBindingSelection, PassthroughBindingValidator, RequestContext, Result,
};
use llm_protocol_provider::{ProtocolType, normalize_base_url};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use uuid::Uuid;

const STATE_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const HEALTH_TTL_SECS: i64 = 300;
const SNAPSHOT_SQL: &str = r#"
SELECT DISTINCT ON (a.id,declared.model) a.*, pb.id AS binding_id,pb.revision AS binding_revision,
       pb.tenant_id AS binding_tenant_id,anchor.name AS tenant_name,pb.is_global,pb.pool_enabled AS binding_pool_enabled,
       (anchor.status='active') AS anchor_active,(owner.status='active') AS owner_active,
       EXISTS(SELECT 1 FROM tenants consumer WHERE consumer.id=$1 AND consumer.status='active') AS caller_active,
       declared.model AS binding_model,h.status AS model_status,h.account_config_version AS model_config_version,
       h.generation AS model_generation,h.expires_at AS model_expires_at,
       statement_timestamp() AS database_now
FROM passthrough_bindings pb JOIN accounts a ON a.id=pb.account_id
JOIN tenants anchor ON anchor.id=pb.tenant_id JOIN tenants owner ON owner.id=a.tenant_id
CROSS JOIN LATERAL unnest(a.models_supported) AS declared(model)
LEFT JOIN account_model_health h ON h.account_id=a.id AND h.api_capability='chat_completions' AND h.model=declared.model
WHERE (pb.tenant_id=$1 OR pb.is_global) AND ($2::TEXT IS NULL OR declared.model=$2)
  AND declared.model<>''
ORDER BY a.id,declared.model,(anchor.status='active') DESC,(pb.tenant_id=$1) DESC,pb.id
"#;
#[derive(Clone, FromQueryResult)]
struct BindingState {
    binding_id: Uuid,
    binding_revision: i64,
    binding_model: String,
    anchor_active: bool,
    owner_active: bool,
    caller_active: bool,
    model_status: Option<String>,
    model_config_version: Option<DateTime<Utc>>,
    model_generation: Option<i64>,
    model_expires_at: Option<DateTime<Utc>>,
    binding_pool_enabled: bool,
}
struct Snapshot {
    account: Account,
    binding: BindingState,
}
pub(crate) struct DbPassthroughBindingValidator {
    pool: Arc<DbRouter>,
    account_states: Arc<AccountStateStore>,
    account_health: Arc<ProviderHealthStore>,
}
impl DbPassthroughBindingValidator {
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
    fn eligible(&self, snapshot: &Snapshot) -> Result<()> {
        let a = &snapshot.account;
        let b = &snapshot.binding;
        if !b.caller_active || !b.owner_active || !b.anchor_active || !a.enabled {
            return Err(Failure::Unavailable.into());
        }
        if a.provider != "openai"
            || !a.api_capabilities.iter().any(|v| v == "chat_completions")
            || !a.models_supported.contains(&b.binding_model)
        {
            return Err(Failure::ModelNotSupported.into());
        }
        // Declared readiness also requires usable local connection metadata;
        // no remote health probe is performed by discovery or listing.
        connection(a)?;
        self.account_health.hydrate_account_health(a);
        if !self.account_health.account_is_routable(a) || self.account_states.is_cooling_down(&a.id)
        {
            return Err(Failure::Unavailable.into());
        }
        // A recorded failure remains a failure until an explicit probe or a
        // successful live observation replaces it. TTL is not an activation
        // gate and does not silently heal a known failed model.
        if b.model_config_version == Some(a.upstream_config_version)
            && matches!(b.model_status.as_deref(), Some("unhealthy" | "degraded"))
        {
            return Err(Failure::ModelUnhealthy.into());
        }
        Ok(())
    }
    async fn capture_health(&self, row: &Snapshot) -> Result<AccountModelHealthSnapshot> {
        let health =
            if row.binding.model_config_version == Some(row.account.upstream_config_version) {
                row.binding
                    .model_generation
                    .map(|generation| AccountModelHealthSnapshot {
                        account_id: row.account.id,
                        api_capability: "chat_completions".into(),
                        model: row.binding.binding_model.clone(),
                        account_config_version: row.account.upstream_config_version,
                        generation,
                    })
            } else {
                None
            };
        if let Some(snapshot) = health {
            return Ok(snapshot);
        }
        let record = tokio::time::timeout(
            STATE_TIMEOUT,
            AccountModelHealth::ensure_snapshot(
                self.pool.write_conn(),
                row.account.id,
                "chat_completions",
                &row.binding.binding_model,
                row.account.upstream_config_version,
            ),
        )
        .await
        .map_err(|_| Failure::DependencyUnavailable)?
        .map_err(|_| Failure::DependencyUnavailable)?
        .ok_or(Failure::Changed)?;
        if matches!(record.status.as_str(), "unhealthy" | "degraded") {
            return Err(Failure::ModelUnhealthy.into());
        }
        Ok(AccountModelHealthSnapshot {
            account_id: record.account_id,
            api_capability: record.api_capability,
            model: record.model,
            account_config_version: record.account_config_version,
            generation: record.generation,
        })
    }
}
pub(crate) fn connection_metadata(endpoint: &str, encrypted_key: &str) -> Result<(String, String)> {
    let endpoint = if endpoint.is_empty() {
        ProtocolType::Openai.default_endpoint().to_string()
    } else {
        normalize_base_url(endpoint).map_err(|_| Failure::Unavailable)?
    };
    let key =
        decrypt_api_key(&EncryptedApiKey::from(encrypted_key)).map_err(|_| Failure::Unavailable)?;
    if key.is_empty() {
        return Err(Failure::Unavailable.into());
    }
    Ok((endpoint, key))
}
fn connection(a: &Account) -> Result<(String, String)> {
    connection_metadata(&a.endpoint, &a.upstream_api_key_encrypted)
}

fn unique_account(rows: Vec<Snapshot>) -> Result<Snapshot> {
    if rows.is_empty() {
        return Err(Failure::NotFound.into());
    }
    let first = rows[0].account.id;
    if rows.iter().any(|v| v.account.id != first) {
        return Err(Failure::Ambiguous.into());
    }
    rows.into_iter()
        .next()
        .ok_or_else(|| Failure::NotFound.into())
}
pub(crate) fn install_model_health_observer(ctx: &mut RequestContext, state: &AppState) {
    if let Ok(v) = DbPassthroughBindingValidator::new(state) {
        ctx.set_account_model_health_observer(Arc::new(v));
    }
}
pub(crate) async fn resolve_passthrough_binding_plan(
    state: &AppState,
    tenant: Uuid,
    model: &str,
) -> Result<(ExecutionPlan, PassthroughBindingSelection, DateTime<Utc>)> {
    if model.is_empty()
        || model.trim() != model
        || model.len() > 255
        || model.chars().any(char::is_control)
    {
        return Err(KeyComputeError::InvalidRequest(
            "Invalid model for the passthrough endpoint".into(),
        ));
    }
    let service = DbPassthroughBindingValidator::new(state)?;
    // Do not filter unhealthy or disabled candidates before detecting an
    // ambiguous namespace: that would create an implicit fallback policy.
    let row = unique_account(service.snapshots(tenant, Some(model)).await?)?;
    service.eligible(&row)?;
    let (endpoint, key) = connection(&row.account)?;
    let binding = PassthroughBindingSelection {
        binding_id: row.binding.binding_id,
        binding_revision: row.binding.binding_revision,
    };
    let target = ExecutionTarget::new_upstream_account("openai", row.account.id, endpoint, key)
        .with_selection(AccountSelection::PassthroughBinding {
            binding_id: binding.binding_id,
            binding_revision: binding.binding_revision,
        });
    Ok((
        ExecutionPlan::new(target),
        binding,
        row.account.upstream_config_version,
    ))
}
pub(crate) async fn list_routable_passthrough_bindings(
    state: &AppState,
    tenant: Uuid,
    model: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let service = DbPassthroughBindingValidator::new(state)?;
    let mut groups: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
    for row in service.snapshots(tenant, model).await? {
        groups
            .entry(row.binding.binding_model.clone())
            .or_default()
            .push(row);
    }
    Ok(groups
        .into_iter()
        .filter_map(|(model, rows)| {
            unique_account(rows)
                .ok()
                .filter(|row| service.eligible(row).is_ok())
                .map(|row| (model, row.account.provider))
        })
        .collect())
}
#[async_trait::async_trait]
impl PassthroughBindingValidator for DbPassthroughBindingValidator {
    async fn validate_target(
        &self,
        tenant: Uuid,
        model: &str,
        selection: PassthroughBindingSelection,
        target: &ExecutionTarget,
        version: DateTime<Utc>,
    ) -> Result<AccountModelHealthSnapshot> {
        let ExecutionTarget::UpstreamAccount {
            provider,
            account_id,
            endpoint,
            upstream_api_key,
            selection: actual,
        } = target
        else {
            return Err(Failure::InvalidPlan.into());
        };
        if *actual
            != (AccountSelection::PassthroughBinding {
                binding_id: selection.binding_id,
                binding_revision: selection.binding_revision,
            })
        {
            return Err(Failure::InvalidPlan.into());
        }
        let row =
            unique_account(self.snapshots(tenant, Some(model)).await?).map_err(
                |error| match error {
                    KeyComputeError::PassthroughBinding(Failure::NotFound) => {
                        Failure::Changed.into()
                    }
                    other => other,
                },
            )?;
        if row.binding.binding_id != selection.binding_id
            || row.binding.binding_revision != selection.binding_revision
            || row.account.id != *account_id
            || row.account.upstream_config_version != version
        {
            return Err(Failure::Changed.into());
        }
        self.eligible(&row)?;
        let (expected_endpoint, key) = connection(&row.account)?;
        if provider != "openai"
            || endpoint != &expected_endpoint
            || upstream_api_key.expose() != key
        {
            return Err(Failure::Changed.into());
        }
        self.capture_health(&row).await
    }
}
#[derive(FromQueryResult)]
struct DatabaseClock {
    now: DateTime<Utc>,
}
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
    .map(|r| r.now)
    .ok_or_else(|| Failure::DependencyUnavailable.into())
}
#[derive(FromQueryResult)]
struct Tracked {
    tracked: bool,
    generation: Option<i64>,
    model_version: Option<DateTime<Utc>>,
}
#[async_trait::async_trait]
impl AccountModelHealthObserver for DbPassthroughBindingValidator {
    async fn snapshot(
        &self,
        target: &ExecutionTarget,
        cap: &str,
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
        let row=tokio::time::timeout(STATE_TIMEOUT,self.pool.write_conn().query_one(Statement::from_sql_and_values(DbBackend::Postgres,
          "SELECT a.*,h.generation,h.account_config_version AS model_version,(h.account_id IS NOT NULL OR EXISTS(SELECT 1 FROM passthrough_bindings pb WHERE pb.account_id=a.id)) AS tracked FROM accounts a LEFT JOIN account_model_health h ON h.account_id=a.id AND h.api_capability=$2 AND h.model=$3 WHERE a.id=$1",
          [(*account_id).into(),cap.into(),model.into()]))).await.map_err(|_|Failure::DependencyUnavailable)?.map_err(|_|Failure::DependencyUnavailable)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let tracked =
            Tracked::from_query_result(&row, "").map_err(|_| Failure::DependencyUnavailable)?;
        if !tracked.tracked {
            return Ok(None);
        }
        let a = Account::from_query_result(&row, "").map_err(|_| Failure::DependencyUnavailable)?;
        let effective = if a.endpoint.is_empty() {
            ProtocolType::parse(&a.provider)
                .map(|p| p.default_endpoint().to_string())
                .unwrap_or_default()
        } else {
            a.endpoint.clone()
        };
        let key = if keycompute_runtime::global_crypto().is_some() {
            decrypt_api_key(&EncryptedApiKey::from(
                a.upstream_api_key_encrypted.as_str(),
            ))
            .map_err(|_| Failure::Unavailable)?
        } else {
            a.upstream_api_key_encrypted.clone()
        };
        if a.provider != *provider || effective != *endpoint || key != upstream_api_key.expose() {
            return Err(Failure::Changed.into());
        }
        let generation = if tracked.model_version == Some(a.upstream_config_version) {
            tracked.generation
        } else {
            None
        };
        let generation = match generation {
            Some(v) => v,
            None => {
                tokio::time::timeout(
                    STATE_TIMEOUT,
                    AccountModelHealth::ensure_snapshot(
                        self.pool.write_conn(),
                        a.id,
                        cap,
                        model,
                        a.upstream_config_version,
                    ),
                )
                .await
                .map_err(|_| Failure::DependencyUnavailable)?
                .map_err(|_| Failure::DependencyUnavailable)?
                .ok_or(Failure::Changed)?
                .generation
            }
        };
        Ok(Some(AccountModelHealthSnapshot {
            account_id: a.id,
            api_capability: cap.into(),
            model: model.into(),
            account_config_version: a.upstream_config_version,
            generation,
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

/// Read-only administrator model catalog. Configuration remains account-row
/// based; this helper does not expose per-model mutation or activation.
impl DbPassthroughBindingValidator {
    pub(crate) async fn catalog_page(
        &self,
        tenant: Uuid,
        q: Option<&str>,
        page: i64,
        size: i64,
    ) -> Result<keycompute_types::ModelCatalogPage> {
        use keycompute_types::{
            ModelAccessMode, ModelAvailability, ModelCatalogEntry, ModelCatalogPage,
        };
        let observed = database_now(self.pool.write_conn()).await?;
        let mut groups: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
        for row in self.snapshots(tenant, None).await? {
            if q.is_none_or(|needle| {
                row.binding
                    .binding_model
                    .to_lowercase()
                    .contains(&needle.to_lowercase())
            }) {
                groups
                    .entry(row.binding.binding_model.clone())
                    .or_default()
                    .push(row);
            }
        }
        let total = groups.len() as u64;
        let entries = groups
            .into_iter()
            .skip(((page - 1) * size) as usize)
            .take(size as usize)
            .map(|(model, rows)| {
                let count = rows.len() as u64;
                let row = &rows[0];
                let result = if count > 1 {
                    Err(Failure::Ambiguous.into())
                } else {
                    self.eligible(row)
                };
                let (status, reason) = match result {
                    Ok(()) => (ModelAvailability::Ready, "passthrough_ready"),
                    Err(KeyComputeError::PassthroughBinding(Failure::ModelUnhealthy)) => {
                        (ModelAvailability::Unhealthy, "model_unhealthy")
                    }
                    Err(KeyComputeError::PassthroughBinding(Failure::Ambiguous)) => (
                        ModelAvailability::Unavailable,
                        "passthrough_binding_ambiguous",
                    ),
                    _ => (ModelAvailability::Unavailable, "account_unavailable"),
                };
                ModelCatalogEntry {
                    model: model.clone(),
                    request_model: model,
                    protocol: "openai".into(),
                    capability: "chat_completions".into(),
                    request_path: "/pt/v1/chat/completions".into(),
                    status,
                    reason_code: reason.into(),
                    configured_targets: count,
                    eligible_targets: u64::from(status == ModelAvailability::Ready),
                    binding_id: Some(row.binding.binding_id.to_string()),
                    binding_revision: Some(row.binding.binding_revision),
                    account_id: Some(row.account.id.to_string()),
                    account_name: Some(row.account.name.clone()),
                    pool_enabled: Some(row.binding.binding_pool_enabled),
                    health_expires_at: row.binding.model_expires_at.map(|v| v.to_rfc3339()),
                }
            })
            .collect();
        Ok(ModelCatalogPage {
            mode: ModelAccessMode::Passthrough,
            tenant_id: tenant.to_string(),
            entries,
            page: page as u64,
            page_size: size as u64,
            total,
            total_pages: total.div_ceil(size as u64),
            observed_at: observed.to_rfc3339(),
        })
    }
}
