//! Current-tenant provider accounts and passthrough bindings.
//! Tenant/global selectors are intentionally absent. Reads are fresh; controls never auto-replay.
use crate::{ApiClient, ClientError, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TenantAccount {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub tenant_active: bool,
    pub name: String,
    pub provider: String,
    pub api_key_preview: String,
    pub api_base: Option<String>,
    pub models: Vec<String>,
    pub api_capabilities: Vec<String>,
    pub rpm_limit: i32,
    pub tpm_limit: i32,
    pub current_rpm: i32,
    pub is_active: bool,
    pub is_healthy: bool,
    pub health_status: String,
    pub health_penalty: i32,
    pub health_reason: Option<String>,
    pub routing_eligible: bool,
    pub pool_enabled: bool,
    pub passthrough_binding_count: u64,
    pub last_probe_at: Option<String>,
    pub last_probe_status: Option<String>,
    pub last_probe_error_code: Option<String>,
    pub priority: i32,
    pub visibility: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct TenantAccountPage {
    pub accounts: Vec<TenantAccount>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}
#[derive(Debug, Clone, Serialize)]
pub struct CreateTenantAccount {
    pub name: String,
    pub provider: String,
    pub api_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    pub models: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_capabilities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tpm_limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_enabled: Option<bool>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct UpdateTenantAccount {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_capabilities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tpm_limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_enabled: Option<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TenantBinding {
    pub id: Uuid,
    pub account_id: Uuid,
    pub account_name: String,
    pub tenant_id: Uuid,
    pub tenant_name: String,
    pub provider: String,
    pub is_global: bool,
    pub pool_enabled: bool,
    pub revision: i64,
    pub models_supported: Vec<String>,
    pub health_status: String,
    pub health_reason_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct TenantBindingPage {
    pub bindings: Vec<TenantBinding>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BindingAccountOption {
    pub id: Uuid,
    pub name: String,
    pub provider: String,
    pub pool_enabled: bool,
    pub models: Vec<String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct BindingOptions {
    pub accounts: Vec<BindingAccountOption>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}
#[derive(Debug, Clone, Serialize)]
pub struct CreateTenantBinding {
    pub account_id: Uuid,
    pub pool_enabled: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct UpdateTenantBinding {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_enabled: Option<bool>,
    pub expected_revision: i64,
}
#[derive(Debug, Clone, Deserialize)]
pub struct DeletedBinding {
    pub deleted: bool,
    pub binding_id: Uuid,
}
#[derive(Debug, Clone, Deserialize)]
pub struct DeletedAccount {
    pub success: bool,
    pub account_id: Uuid,
}

fn invalid() -> ClientError {
    ClientError::InvalidResponse(
        "Provider response did not match the requested tenant/resource".into(),
    )
}
fn real(id: Uuid) -> Result<()> {
    if id.is_nil() {
        Err(ClientError::Config(
            "An explicit nonzero resource ID is required".into(),
        ))
    } else {
        Ok(())
    }
}
fn page(page: u32, size: u32, search: &str) -> Result<()> {
    if page == 0
        || !(1..=100).contains(&size)
        || search.len() > 255
        || search.chars().any(char::is_control)
    {
        Err(ClientError::Config(
            "Invalid provider pagination or search".into(),
        ))
    } else {
        Ok(())
    }
}
fn bounded(value: &str, max: usize, label: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        Err(ClientError::Config(format!("Invalid {label}")))
    } else {
        Ok(())
    }
}
fn validate_models(models: &[String]) -> Result<()> {
    if models.is_empty()
        || models.len() > 512
        || models
            .iter()
            .any(|m| m.trim().is_empty() || m.len() > 255 || m.chars().any(char::is_control))
    {
        Err(ClientError::Config(
            "At least one bounded model is required".into(),
        ))
    } else {
        Ok(())
    }
}
fn validate_capabilities(provider: &str, values: Option<&[String]>) -> Result<()> {
    let Some(values) = values else { return Ok(()) };
    if values.is_empty()
        || values.iter().any(|v| match provider {
            "openai" => !matches!(v.as_str(), "chat_completions" | "responses"),
            "anthropic" => v != "messages",
            _ => true,
        })
    {
        return Err(ClientError::Config(
            "Invalid API capabilities for provider protocol".into(),
        ));
    }
    Ok(())
}
fn validate_base(value: Option<&str>) -> Result<()> {
    if let Some(v) = value
        && (v.len() > 2048
            || v.chars().any(char::is_control)
            || v.chars().any(char::is_whitespace)
            || v.contains('?')
            || v.contains('#')
            || (!v.is_empty() && !v.starts_with("http://") && !v.starts_with("https://")))
    {
        return Err(ClientError::Config("Invalid provider base URL".into()));
    }
    Ok(())
}
fn validate_account_input(
    name: &str,
    provider: &str,
    models: &[String],
    rpm: Option<i32>,
    tpm: Option<i32>,
    priority: Option<i32>,
) -> Result<()> {
    bounded(name, 255, "account name")?;
    if !matches!(provider, "openai" | "anthropic") {
        return Err(ClientError::Config(
            "Provider protocol must be openai or anthropic".into(),
        ));
    }
    validate_models(models)?;
    if rpm.is_some_and(|v| v < 1)
        || tpm.is_some_and(|v| v < 1)
        || priority.is_some_and(|v| !(0..=10).contains(&v))
    {
        return Err(ClientError::Config(
            "Invalid account limits or priority".into(),
        ));
    }
    Ok(())
}
#[derive(Debug, Clone)]
pub struct TenantProviderApi {
    client: ApiClient,
    tenant: Uuid,
    accounts: String,
    bindings: String,
}
impl TenantProviderApi {
    pub fn new(client: &ApiClient, tenant: Uuid) -> Result<Self> {
        real(tenant)?;
        Ok(Self {
            client: client.clone(),
            tenant,
            accounts: format!("/api/v1/tenants/{tenant}/accounts"),
            bindings: format!("/api/v1/tenants/{tenant}/passthrough-bindings"),
        })
    }
    fn account(&self, row: TenantAccount, id: Option<Uuid>) -> Result<TenantAccount> {
        if row.id.is_nil()
            || row.tenant_id != self.tenant
            || row.visibility != "tenant"
            || id.is_some_and(|id| id != row.id)
            || !matches!(row.provider.as_str(), "openai" | "anthropic")
            || validate_capabilities(&row.provider, Some(&row.api_capabilities)).is_err()
        {
            Err(invalid())
        } else {
            Ok(row)
        }
    }
    fn binding(&self, row: TenantBinding, id: Option<Uuid>) -> Result<TenantBinding> {
        if row.id.is_nil()
            || row.account_id.is_nil()
            || row.tenant_id != self.tenant
            || row.is_global
            || row.revision <= 0
            || id.is_some_and(|id| id != row.id)
        {
            Err(invalid())
        } else {
            Ok(row)
        }
    }
    pub async fn accounts(
        &self,
        page_no: u32,
        size: u32,
        search: &str,
        provider: &str,
        status: &str,
        token: &str,
    ) -> Result<TenantAccountPage> {
        page(page_no, size, search)?;
        if !matches!(provider, "" | "openai" | "anthropic")
            || !matches!(status, "" | "all" | "active" | "inactive")
        {
            return Err(ClientError::Config("Invalid provider filter".into()));
        }
        let path = format!(
            "{}?page={page_no}&page_size={size}&search={}&provider={}&status={}",
            self.accounts,
            super::common::encode_query_value(search),
            super::common::encode_query_value(provider),
            super::common::encode_query_value(status)
        );
        let out: TenantAccountPage = self.client.get_json_fresh(&path, Some(token)).await?;
        if out.page != i64::from(page_no)
            || out.page_size != i64::from(size)
            || out.total < 0
            || out.total_pages < 0
            || out.accounts.len() > size as usize
        {
            return Err(invalid());
        }
        for row in &out.accounts {
            self.account(row.clone(), None)?;
        }
        Ok(out)
    }
    pub async fn account_detail(&self, id: Uuid, token: &str) -> Result<TenantAccount> {
        real(id)?;
        let row = self
            .client
            .get_json_fresh(&format!("{}/{id}", self.accounts), Some(token))
            .await?;
        self.account(row, Some(id))
    }
    pub async fn create_account(
        &self,
        body: &CreateTenantAccount,
        token: &str,
    ) -> Result<TenantAccount> {
        validate_account_input(
            &body.name,
            &body.provider,
            &body.models,
            body.rpm_limit,
            body.tpm_limit,
            body.priority,
        )?;
        bounded(&body.api_key, 16384, "API key")?;
        validate_base(body.api_base.as_deref())?;
        validate_capabilities(&body.provider, body.api_capabilities.as_deref())?;
        let row = self
            .client
            .post_json(&self.accounts, body, Some(token))
            .await?;
        self.account(row, None)
    }
    pub async fn update_account(
        &self,
        id: Uuid,
        body: &UpdateTenantAccount,
        token: &str,
    ) -> Result<TenantAccount> {
        real(id)?;
        if body.priority.is_some_and(|v| !(0..=10).contains(&v))
            || body.rpm_limit.is_some_and(|v| v < 1)
            || body.tpm_limit.is_some_and(|v| v < 1)
        {
            return Err(ClientError::Config(
                "Invalid account limits or priority".into(),
            ));
        }
        if let Some(v) = &body.models {
            validate_models(v)?;
        }
        if let Some(v) = &body.name {
            bounded(v, 255, "account name")?;
        }
        if let Some(v) = &body.api_key {
            bounded(v, 16384, "API key")?;
        }
        validate_base(body.api_base.as_deref())?;
        if body.api_capabilities.as_ref().is_some_and(|v| {
            v.is_empty()
                || v.iter()
                    .any(|x| !matches!(x.as_str(), "chat_completions" | "responses" | "messages"))
        }) {
            return Err(ClientError::Config("Invalid API capabilities".into()));
        }
        let row = self
            .client
            .put_json(&format!("{}/{id}", self.accounts), body, Some(token))
            .await?;
        self.account(row, Some(id))
    }
    pub async fn delete_account(&self, id: Uuid, token: &str) -> Result<DeletedAccount> {
        real(id)?;
        let out: DeletedAccount = self
            .client
            .delete_json(&format!("{}/{id}", self.accounts), Some(token))
            .await?;
        if !out.success || out.account_id != id {
            Err(invalid())
        } else {
            Ok(out)
        }
    }
    pub async fn test_account(&self, id: Uuid, token: &str) -> Result<serde_json::Value> {
        real(id)?;
        self.client
            .post_json(
                &format!("{}/{id}/test", self.accounts),
                &serde_json::json!({}),
                Some(token),
            )
            .await
    }
    pub async fn refresh_account(&self, id: Uuid, token: &str) -> Result<serde_json::Value> {
        real(id)?;
        self.client
            .post_json(
                &format!("{}/{id}/refresh", self.accounts),
                &serde_json::json!({}),
                Some(token),
            )
            .await
    }
    pub async fn bindings(
        &self,
        page_no: u32,
        size: u32,
        search: &str,
        token: &str,
    ) -> Result<TenantBindingPage> {
        page(page_no, size, search)?;
        let path = format!(
            "{}?page={page_no}&page_size={size}&search={}",
            self.bindings,
            super::common::encode_query_value(search)
        );
        let out: TenantBindingPage = self.client.get_json_fresh(&path, Some(token)).await?;
        if out.page != i64::from(page_no)
            || out.page_size != i64::from(size)
            || out.total < 0
            || out.total_pages < 0
            || out.bindings.len() > size as usize
        {
            return Err(invalid());
        }
        for row in &out.bindings {
            self.binding(row.clone(), None)?;
        }
        Ok(out)
    }
    pub async fn binding_options(
        &self,
        page_no: u32,
        size: u32,
        search: &str,
        token: &str,
    ) -> Result<BindingOptions> {
        page(page_no, size, search)?;
        let path = format!(
            "{}/options?page={page_no}&page_size={size}&search={}",
            self.bindings,
            super::common::encode_query_value(search)
        );
        let out: BindingOptions = self.client.get_json_fresh(&path, Some(token)).await?;
        if out.page != i64::from(page_no)
            || out.page_size != i64::from(size)
            || out.total < 0
            || out.total_pages < 0
            || out.accounts.len() > size as usize
            || out.accounts.iter().any(|a| a.id.is_nil())
        {
            Err(invalid())
        } else {
            Ok(out)
        }
    }
    pub async fn create_binding(
        &self,
        body: &CreateTenantBinding,
        token: &str,
    ) -> Result<TenantBinding> {
        real(body.account_id)?;
        let row = self
            .client
            .post_json(&self.bindings, body, Some(token))
            .await?;
        self.binding(row, None)
    }
    pub async fn update_binding(
        &self,
        id: Uuid,
        body: &UpdateTenantBinding,
        token: &str,
    ) -> Result<TenantBinding> {
        real(id)?;
        if body.expected_revision <= 0 {
            return Err(ClientError::Config(
                "A positive observed binding revision is required".into(),
            ));
        }
        if let Some(account) = body.account_id {
            real(account)?;
        }
        let row = self
            .client
            .put_json(&format!("{}/{id}", self.bindings), body, Some(token))
            .await?;
        self.binding(row, Some(id))
    }
    pub async fn delete_binding(
        &self,
        id: Uuid,
        revision: i64,
        token: &str,
    ) -> Result<DeletedBinding> {
        real(id)?;
        if revision <= 0 {
            return Err(ClientError::Config(
                "A positive observed binding revision is required".into(),
            ));
        }
        let out: DeletedBinding = self
            .client
            .delete_json(
                &format!("{}/{id}?expected_revision={revision}", self.bindings),
                Some(token),
            )
            .await?;
        if !out.deleted || out.binding_id != id {
            Err(invalid())
        } else {
            Ok(out)
        }
    }
    pub async fn probe_binding(&self, id: Uuid, token: &str) -> Result<serde_json::Value> {
        real(id)?;
        self.client
            .post_json(
                &format!("{}/{id}/probe", self.bindings),
                &serde_json::json!({}),
                Some(token),
            )
            .await
    }
}
