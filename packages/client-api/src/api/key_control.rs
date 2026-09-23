//! Tenant metadata management and a separate, owner-only one-time issuance client.
//! No platform bypass, secret read API, implicit target or automatic command replay.
use crate::{ApiClient, ClientError, Result};
use reqwest::Method;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct KeyMetadata {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub owner_user_id: Uuid,
    pub name: String,
    pub key_preview: String,
    pub revoked: bool,
    pub revoked_at: Option<String>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct KeyPage {
    pub keys: Vec<KeyMetadata>,
    pub total: i64,
    pub page: u32,
    pub page_size: u32,
    pub total_pages: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentStatus {
    Pending,
    Claimed,
    Cancelled,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IssuanceIntent {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub owner_user_id: Uuid,
    pub requested_by_user_id: Uuid,
    pub replaces_key_id: Option<Uuid>,
    pub requested_name: String,
    pub requested_expires_at: Option<String>,
    pub status: IntentStatus,
    pub expires_at: String,
    pub claimed_at: Option<String>,
    pub created_key_id: Option<Uuid>,
    pub created_at: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct IssuancePage {
    pub intents: Vec<IssuanceIntent>,
    pub total: i64,
    pub page: u32,
    pub page_size: u32,
    pub total_pages: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssuanceOutcome {
    Created,
    AlreadyPending,
    Cancelled,
    Declined,
}
#[derive(Debug, Clone, Deserialize)]
pub struct IssuanceResponse {
    pub intent: IssuanceIntent,
    pub outcome: IssuanceOutcome,
    pub message: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct NewIssuance {
    pub owner_user_id: Uuid,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct RotateKey {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct KeyPatch {
    pub expected_updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Omission preserves expiration; Some(None) intentionally clears it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Option<String>>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct KeyMutation {
    pub success: bool,
    pub key: Option<KeyMetadata>,
    pub key_id: Uuid,
    pub revoked_at: Option<String>,
    pub deleted: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyQuery {
    pub owner_user_id: Option<Uuid>,
    pub include_revoked: bool,
    pub page: u32,
    pub page_size: u32,
}
impl Default for KeyQuery {
    fn default() -> Self {
        Self {
            owner_user_id: None,
            include_revoked: false,
            page: 1,
            page_size: 20,
        }
    }
}
/// Intentionally nonserializable. UI callers may display/copy it only on explicit owner action.
#[derive(Clone, Deserialize)]
pub struct IssuedSecret(String);
impl IssuedSecret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for IssuedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IssuedSecret([REDACTED])")
    }
}
/// One-time claim results cannot be serialized into persistent frontend state.
/// ```compile_fail
/// fn persist(value: &client_api::api::key_control::ClaimedKey) {
///     let _ = serde_json::to_string(value);
/// }
/// ```
#[derive(Clone, Deserialize)]
pub struct ClaimedKey {
    pub outcome: String,
    pub intent_id: Uuid,
    pub key_id: Uuid,
    pub name: String,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub key: IssuedSecret,
    pub secret_returned_once: bool,
}
impl std::fmt::Debug for ClaimedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimedKey")
            .field("intent_id", &self.intent_id)
            .field("key_id", &self.key_id)
            .field("key", &"[REDACTED]")
            .finish()
    }
}
fn real(id: Uuid) -> Result<()> {
    if id.is_nil() {
        Err(ClientError::Config(
            "A real resource identifier is required".into(),
        ))
    } else {
        Ok(())
    }
}
fn invalid() -> ClientError {
    ClientError::InvalidResponse(
        "Key metadata did not match the requested tenant, owner or resource".into(),
    )
}
fn timestamp(raw: &str) -> Result<()> {
    if raw.is_empty() || raw.len() > 128 || raw.chars().any(char::is_control) {
        Err(ClientError::Config(
            "A bounded RFC3339 timestamp is required".into(),
        ))
    } else {
        Ok(())
    }
}
fn name(raw: &str) -> Result<()> {
    if raw.trim().is_empty() || raw.chars().count() > 255 || raw.chars().any(char::is_control) {
        Err(ClientError::Config(
            "A key name between one and 255 characters is required".into(),
        ))
    } else {
        Ok(())
    }
}
fn page_query(page: u32, size: u32) -> Result<String> {
    if !(1..=1_000_000).contains(&page) || !(1..=100).contains(&size) {
        return Err(ClientError::Config("Invalid key page".into()));
    }
    Ok(format!("page={page}&page_size={size}"))
}
fn valid_page(
    page: u32,
    size: u32,
    total: i64,
    total_pages: i64,
    returned_page: u32,
    returned_size: u32,
    len: usize,
) -> Result<()> {
    if returned_page != page
        || returned_size != size
        || total < 0
        || total_pages < 0
        || len > size as usize
        || total_pages != total / i64::from(size) + i64::from(total % i64::from(size) != 0)
    {
        return Err(invalid());
    }
    Ok(())
}
fn check_intent(
    row: &IssuanceIntent,
    tenant: Uuid,
    owner: Option<Uuid>,
    id: Option<Uuid>,
) -> Result<()> {
    if row.tenant_id != tenant
        || row.id.is_nil()
        || row.owner_user_id.is_nil()
        || row.requested_by_user_id.is_nil()
        || owner.is_some_and(|v| v != row.owner_user_id)
        || id.is_some_and(|v| v != row.id)
        || row.replaces_key_id.is_some_and(|v| v.is_nil())
        || row.created_key_id.is_some_and(|v| v.is_nil())
    {
        return Err(invalid());
    }
    Ok(())
}
#[derive(Debug, Clone)]
pub struct TenantKeyApi {
    client: ApiClient,
    tenant: Uuid,
    base: String,
}
impl TenantKeyApi {
    pub fn new(client: &ApiClient, tenant: Uuid) -> Result<Self> {
        real(tenant)?;
        Ok(Self {
            client: client.clone(),
            tenant,
            base: format!("/api/v1/tenants/{tenant}"),
        })
    }
    fn check(&self, row: &KeyMetadata, id: Option<Uuid>, owner: Option<Uuid>) -> Result<()> {
        if row.tenant_id != self.tenant
            || row.id.is_nil()
            || row.owner_user_id.is_nil()
            || id.is_some_and(|v| v != row.id)
            || owner.is_some_and(|v| v != row.owner_user_id)
            || timestamp(&row.updated_at).is_err()
        {
            return Err(invalid());
        }
        Ok(())
    }
    async fn command<T: DeserializeOwned, B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: &B,
        token: &str,
    ) -> Result<T> {
        let req = self
            .client
            .request_with_auth(method, path, Some(token))
            .await?;
        self.client.send_and_parse(req.json(body)).await
    }
    pub async fn list(&self, q: &KeyQuery, token: &str) -> Result<KeyPage> {
        let page = page_query(q.page, q.page_size)?;
        if let Some(owner) = q.owner_user_id {
            real(owner)?;
        }
        let owner = q
            .owner_user_id
            .map(|v| format!("&owner_user_id={v}"))
            .unwrap_or_default();
        let r: KeyPage = self
            .client
            .get_json_fresh(
                &format!(
                    "{}/keys?{page}&include_revoked={}{owner}",
                    self.base, q.include_revoked
                ),
                Some(token),
            )
            .await?;
        valid_page(
            q.page,
            q.page_size,
            r.total,
            r.total_pages,
            r.page,
            r.page_size,
            r.keys.len(),
        )?;
        for row in &r.keys {
            self.check(row, None, q.owner_user_id)?;
            if !q.include_revoked && row.revoked {
                return Err(invalid());
            }
        }
        Ok(r)
    }
    pub async fn detail(&self, id: Uuid, token: &str) -> Result<KeyMetadata> {
        real(id)?;
        let r: KeyMetadata = self
            .client
            .get_json_fresh(&format!("{}/keys/{id}", self.base), Some(token))
            .await?;
        self.check(&r, Some(id), None)?;
        Ok(r)
    }
    pub async fn patch(&self, id: Uuid, body: &KeyPatch, token: &str) -> Result<KeyMetadata> {
        real(id)?;
        timestamp(&body.expected_updated_at)?;
        if let Some(n) = &body.name {
            name(n)?;
        }
        if let Some(Some(t)) = &body.expires_at {
            timestamp(t)?;
        }
        if body.name.is_none() && body.expires_at.is_none() {
            return Err(ClientError::Config("Choose a metadata change".into()));
        }
        let r: KeyMetadata = self
            .command(
                Method::PATCH,
                &format!("{}/keys/{id}", self.base),
                body,
                token,
            )
            .await?;
        self.check(&r, Some(id), None)?;
        Ok(r)
    }
    async fn remove(&self, id: Uuid, revoke: bool, token: &str) -> Result<KeyMutation> {
        real(id)?;
        let path = format!(
            "{}/keys/{id}{}",
            self.base,
            if revoke { "/revoke" } else { "" }
        );
        let r: KeyMutation = self
            .command(
                if revoke { Method::POST } else { Method::DELETE },
                &path,
                &serde_json::json!({}),
                token,
            )
            .await?;
        if !r.success || r.key_id != id || revoke && r.deleted || r.deleted && r.key.is_some() {
            return Err(invalid());
        }
        if !r.deleted {
            let key = r.key.as_ref().ok_or_else(invalid)?;
            self.check(key, Some(id), None)?;
            if !key.revoked {
                return Err(invalid());
            }
        }
        Ok(r)
    }
    pub async fn revoke(&self, id: Uuid, token: &str) -> Result<KeyMutation> {
        self.remove(id, true, token).await
    }
    pub async fn delete(&self, id: Uuid, token: &str) -> Result<KeyMutation> {
        self.remove(id, false, token).await
    }
    pub async fn request(&self, body: &NewIssuance, token: &str) -> Result<IssuanceResponse> {
        real(body.owner_user_id)?;
        name(&body.name)?;
        if let Some(t) = &body.expires_at {
            timestamp(t)?;
        }
        let r: IssuanceResponse = self
            .command(
                Method::POST,
                &format!("{}/keys/issuance", self.base),
                body,
                token,
            )
            .await?;
        check_intent(&r.intent, self.tenant, Some(body.owner_user_id), None)?;
        if !matches!(
            r.outcome,
            IssuanceOutcome::Created | IssuanceOutcome::AlreadyPending
        ) || r.intent.status != IntentStatus::Pending
            || r.intent.replaces_key_id.is_some()
        {
            return Err(invalid());
        }
        Ok(r)
    }
    pub async fn rotate(
        &self,
        id: Uuid,
        body: &RotateKey,
        token: &str,
    ) -> Result<IssuanceResponse> {
        real(id)?;
        name(&body.name)?;
        if let Some(t) = &body.expires_at {
            timestamp(t)?;
        }
        let r: IssuanceResponse = self
            .command(
                Method::POST,
                &format!("{}/keys/{id}/rotate", self.base),
                body,
                token,
            )
            .await?;
        check_intent(&r.intent, self.tenant, None, None)?;
        if !matches!(
            r.outcome,
            IssuanceOutcome::Created | IssuanceOutcome::AlreadyPending
        ) || r.intent.status != IntentStatus::Pending
            || r.intent.replaces_key_id != Some(id)
        {
            return Err(invalid());
        }
        Ok(r)
    }
    pub async fn issuances(
        &self,
        page: u32,
        size: u32,
        owner: Option<Uuid>,
        token: &str,
    ) -> Result<IssuancePage> {
        let query = page_query(page, size)?;
        if let Some(id) = owner {
            real(id)?;
        }
        let suffix = owner
            .map(|v| format!("&owner_user_id={v}"))
            .unwrap_or_default();
        let r: IssuancePage = self
            .client
            .get_json_fresh(
                &format!("{}/key-issuance?{query}{suffix}", self.base),
                Some(token),
            )
            .await?;
        valid_page(
            page,
            size,
            r.total,
            r.total_pages,
            r.page,
            r.page_size,
            r.intents.len(),
        )?;
        for row in &r.intents {
            check_intent(row, self.tenant, owner, None)?;
            if row.status != IntentStatus::Pending {
                return Err(invalid());
            }
        }
        Ok(r)
    }
    pub async fn cancel(&self, id: Uuid, token: &str) -> Result<IssuanceResponse> {
        real(id)?;
        let r: IssuanceResponse = self
            .command(
                Method::POST,
                &format!("{}/key-issuance/{id}/cancel", self.base),
                &serde_json::json!({}),
                token,
            )
            .await?;
        check_intent(&r.intent, self.tenant, None, Some(id))?;
        if r.outcome != IssuanceOutcome::Cancelled || r.intent.status != IntentStatus::Cancelled {
            return Err(invalid());
        }
        Ok(r)
    }
}
/// Personal routes never carry owner/tenant override parameters, including for admins.
#[derive(Debug, Clone)]
pub struct OwnerKeyIssuanceApi {
    client: ApiClient,
    tenant: Uuid,
    owner: Uuid,
}
impl OwnerKeyIssuanceApi {
    pub fn new(client: &ApiClient, tenant: Uuid, owner: Uuid) -> Result<Self> {
        real(tenant)?;
        real(owner)?;
        Ok(Self {
            client: client.clone(),
            tenant,
            owner,
        })
    }
    pub async fn list(&self, page: u32, size: u32, token: &str) -> Result<IssuancePage> {
        let query = page_query(page, size)?;
        let r: IssuancePage = self
            .client
            .get_json_fresh(&format!("/api/v1/me/key-issuance?{query}"), Some(token))
            .await?;
        valid_page(
            page,
            size,
            r.total,
            r.total_pages,
            r.page,
            r.page_size,
            r.intents.len(),
        )?;
        for row in &r.intents {
            check_intent(row, self.tenant, Some(self.owner), None)?;
            if row.status != IntentStatus::Pending {
                return Err(invalid());
            }
        }
        Ok(r)
    }
    pub async fn claim(&self, intent: &IssuanceIntent, token: &str) -> Result<ClaimedKey> {
        check_intent(intent, self.tenant, Some(self.owner), None)?;
        if intent.status != IntentStatus::Pending {
            return Err(ClientError::Config(
                "Only a current pending issuance may be claimed".into(),
            ));
        }
        let r: ClaimedKey = self
            .client
            .post_json(
                &format!("/api/v1/me/key-issuance/{}/claim", intent.id),
                &serde_json::json!({}),
                Some(token),
            )
            .await
            .map_err(safe_claim_error)?;
        if r.intent_id != intent.id
            || r.key_id.is_nil()
            || r.outcome != "claimed"
            || !r.secret_returned_once
            || r.name != intent.requested_name
            || r.expires_at != intent.requested_expires_at
            || intent.replaces_key_id == Some(r.key_id)
            || timestamp(&r.created_at).is_err()
            || r.key.expose().is_empty()
            || r.key.expose().len() > 4096
            || r.key
                .expose()
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(safe_claim_error(invalid()));
        }
        Ok(r)
    }
    pub async fn decline(&self, intent: &IssuanceIntent, token: &str) -> Result<IssuanceResponse> {
        check_intent(intent, self.tenant, Some(self.owner), None)?;
        let r: IssuanceResponse = self
            .client
            .post_json(
                &format!("/api/v1/me/key-issuance/{}/decline", intent.id),
                &serde_json::json!({}),
                Some(token),
            )
            .await?;
        check_intent(&r.intent, self.tenant, Some(self.owner), Some(intent.id))?;
        if r.outcome != IssuanceOutcome::Declined || r.intent.status != IntentStatus::Cancelled {
            return Err(invalid());
        }
        Ok(r)
    }
}
fn safe_claim_error(error: ClientError) -> ClientError {
    let message="The one-time key claim could not be confirmed. Refresh issuance and key records before requesting another key.".to_owned();
    match error {
        ClientError::Unauthorized(_) => ClientError::Unauthorized(message),
        ClientError::Forbidden(_) => ClientError::Forbidden(message),
        ClientError::NotFound(_) => ClientError::NotFound(message),
        ClientError::RateLimited(mut info) => {
            info.message = message;
            info.scope = None;
            ClientError::RateLimited(info)
        }
        ClientError::ServiceUnavailable(_) => ClientError::ServiceUnavailable(message),
        ClientError::ServerError(_) => ClientError::ServerError(message),
        ClientError::Network(_) => ClientError::Network(message),
        _ => ClientError::Other(message),
    }
}
