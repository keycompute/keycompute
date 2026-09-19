//! Authoritative account-only quota admission and read-only routing snapshots.
use crate::handlers::generation_tpm_reservation_tokens;
use keycompute_db::{Account, DbRouter};
use keycompute_ratelimit::{RateLimitConfig, account::AccountQuotaService};
use keycompute_types::{
    AccountAttemptLease, AccountCapacityPolicy, AccountCapacitySnapshot, ExecutionTarget,
    KeyComputeError, RequestContext, Result,
};
use sea_orm::{DbBackend, FromQueryResult, Statement};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

pub struct ServerAccountCapacity {
    pub db: Arc<DbRouter>,
    pub quotas: Arc<AccountQuotaService>,
}

impl std::fmt::Debug for ServerAccountCapacity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerAccountCapacity")
            .field("database", &"<authoritative writer>")
            .field("quotas", &self.quotas)
            .finish()
    }
}

#[async_trait::async_trait]
impl AccountCapacityPolicy for ServerAccountCapacity {
    async fn snapshot(&self, account_id: Uuid) -> Result<AccountCapacitySnapshot> {
        self.quotas.snapshot(account_id).await
    }
    async fn admit(
        &self,
        ctx: &RequestContext,
        target: &ExecutionTarget,
    ) -> Result<Box<dyn AccountAttemptLease>> {
        let ExecutionTarget::UpstreamAccount {
            account_id,
            provider,
            endpoint,
            upstream_api_key,
            ..
        } = target
        else {
            return Err(KeyComputeError::InvalidRequest(
                "account admission requires a provider target".into(),
            ));
        };
        // Route snapshots and affinity picks do not authorize future attempts.
        // Re-read enabled state, visibility, protocol, model and quota on writer.
        let (policy,binding_id,binding_revision) = match target.account_selection() {
            Some(keycompute_types::AccountSelection::PassthroughBinding {binding_id,binding_revision}) => (
                "a.enabled AND EXISTS(SELECT 1 FROM passthrough_bindings access_grant JOIN tenants anchor ON anchor.id=access_grant.tenant_id AND anchor.status='active' WHERE access_grant.id=$3 AND access_grant.revision=$4 AND access_grant.account_id=a.id AND (access_grant.tenant_id=$2 OR access_grant.is_global))".to_string(),binding_id,binding_revision),
            _ => (format!("{} AND $3::UUID IS NULL AND $4::BIGINT=0",keycompute_db::models::upstream_access::non_pt_predicate("a","$2")),Uuid::nil(),0),
        };
        let binding_id = (!binding_id.is_nil()).then_some(binding_id);
        let account = tokio::time::timeout(
            Duration::from_secs(3),
            Account::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
                format!("SELECT a.* FROM accounts a JOIN tenants owner ON owner.id=a.tenant_id WHERE a.id=$1 AND owner.status='active' AND EXISTS(SELECT 1 FROM tenants caller WHERE caller.id=$2 AND caller.status='active') AND {policy}"),
                [(*account_id).into(),ctx.tenant_id.into(),binding_id.into(),binding_revision.into()])).one(self.db.write_conn()),
        ).await.map_err(|_|KeyComputeError::ServiceUnavailable("account configuration lookup timed out".into()))?
         .map_err(|_|KeyComputeError::ServiceUnavailable("account configuration unavailable".into()))?
         .ok_or_else(||KeyComputeError::PermissionDenied("upstream account is not authorized for this entry point".into()))?;
        if account.health_status == "unhealthy"
            || account.provider != *provider
            || (!ctx.model.is_empty()
                && !account
                    .models_supported
                    .iter()
                    .any(|model| model == &ctx.model))
        {
            return Err(KeyComputeError::PermissionDenied(
                "upstream account is not eligible".into(),
            ));
        }
        let capability = if ctx.native_anthropic_request.is_some() {
            "messages"
        } else if ctx.native_openai_responses_request.is_some() {
            "responses"
        } else {
            "chat_completions"
        };
        if !account
            .api_capabilities
            .iter()
            .any(|value| value == capability)
        {
            return Err(KeyComputeError::PermissionDenied(
                "upstream account capability changed".into(),
            ));
        }
        let effective_endpoint = if account.endpoint.is_empty() {
            llm_protocol_provider::ProtocolType::parse(provider)
                .map(|p| p.default_endpoint().to_string())
                .unwrap_or_default()
        } else {
            account.endpoint.clone()
        };
        let current_key = if keycompute_runtime::global_crypto().is_some() {
            keycompute_runtime::decrypt_api_key(&keycompute_runtime::EncryptedApiKey::from(
                account.upstream_api_key_encrypted.clone(),
            ))
            .map_err(|_| {
                KeyComputeError::PermissionDenied("upstream account key is unavailable".into())
            })?
        } else {
            account.upstream_api_key_encrypted.clone()
        };
        if effective_endpoint != *endpoint || current_key != upstream_api_key.expose() {
            return Err(KeyComputeError::PermissionDenied(
                "upstream account connection changed after routing".into(),
            ));
        }
        let limits = RateLimitConfig::from_tenant(account.rpm_limit, account.tpm_limit);
        let prediction = generation_tpm_reservation_tokens(ctx, limits.tpm_limit);
        Ok(Box::new(
            self.quotas.admit(*account_id, prediction, limits).await?,
        ))
    }
}
