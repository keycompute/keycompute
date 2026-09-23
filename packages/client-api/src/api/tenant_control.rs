//! Current-tenant control-plane API. Selectors never grant membership.
//! Console reads are fresh; mutations are single-dispatch (no implicit replay).
use crate::{ApiClient, ClientError, Result};
use keycompute_types::{MembershipStatus, TenantRole, UserStatus};
use reqwest::Method;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}
#[derive(Debug, Clone, Deserialize)]
pub struct TenantContext {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub status: String,
    pub default_rpm_limit: i32,
    pub default_tpm_limit: i32,
    pub authz_version: i64,
    pub tenant_role: TenantRole,
    pub membership_authz_version: i64,
}
#[derive(Debug, Clone, Deserialize)]
pub struct TenantMember {
    pub user_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub user_status: UserStatus,
    pub tenant_role: TenantRole,
    pub membership_status: MembershipStatus,
    pub authz_version: i64,
    pub invited_by: Option<Uuid>,
    pub joined_at: String,
    pub removed_at: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct TenantInvitation {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub invited_by: Uuid,
    pub email: String,
    pub tenant_role: TenantRole,
    pub status: String,
    pub expires_at: String,
    pub accepted_by: Option<Uuid>,
    pub accepted_at: Option<String>,
    pub revoked_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct TenantAuditEvent {
    pub id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub actor_user_id: Uuid,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub request_id: Option<Uuid>,
    pub credential_kind: String,
    pub platform_role: String,
    pub tenant_role: Option<TenantRole>,
    pub metadata: serde_json::Value,
    pub result: String,
    pub created_at: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct TenantPatch {
    pub expected_authz_version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_rpm_limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_tpm_limit: Option<i32>,
}
#[derive(Debug, Clone, Serialize)]
pub struct MemberPatch {
    pub expected_authz_version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_role: Option<TenantRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<MembershipStatus>,
}
#[derive(Debug, Clone, Serialize)]
pub struct CreateInvitation {
    pub email: String,
    pub tenant_role: TenantRole,
    pub expires_in_seconds: i64,
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InvitationOutcome {
    Created,
    AlreadyPending,
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationStatus {
    Sent,
    Failed,
    Unconfigured,
    NotApplicable,
}
#[derive(Clone, Deserialize)]
pub struct InvitationCreateResponse {
    pub invitation: TenantInvitation,
    pub outcome: InvitationOutcome,
    pub notification: NotificationStatus,
    /// One-time recovery link. Keep in component memory, never persistent state.
    pub acceptance_link: Option<String>,
}
impl std::fmt::Debug for InvitationCreateResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InvitationCreateResponse")
            .field("invitation_id", &self.invitation.id)
            .field("outcome", &self.outcome)
            .field("notification", &self.notification)
            .field("acceptance_link", &"[REDACTED]")
            .finish()
    }
}
#[derive(Debug, Clone, Deserialize)]
pub struct AcceptInvitationResponse {
    pub tenant: TenantContext,
    pub membership: TenantMember,
}
/// An in-memory one-time secret; not serializable and never included in Debug.
#[derive(Clone, PartialEq, Eq)]
pub struct InvitationToken(String);
impl std::fmt::Debug for InvitationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InvitationToken([REDACTED])")
    }
}
impl InvitationToken {
    pub fn parse(value: &str) -> Result<Self> {
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ClientError::Config("invalid invitation token".into()));
        }
        Ok(Self(value.to_owned()))
    }
    pub fn from_fragment(fragment: &str) -> Result<Self> {
        let value = fragment
            .strip_prefix("#token=")
            .ok_or_else(|| ClientError::Config("invalid invitation fragment".into()))?;
        Self::parse(value)
    }
}
fn real_id(id: Uuid) -> Result<()> {
    if id.is_nil() {
        return Err(ClientError::Config(
            "a nonzero resource ID is required".into(),
        ));
    }
    Ok(())
}
fn version(value: i64) -> Result<()> {
    if value <= 0 {
        return Err(ClientError::Config(
            "a current positive revision is required".into(),
        ));
    }
    Ok(())
}
fn page_query(page: u32, size: u32) -> Result<String> {
    if !(1..=1_000_000).contains(&page) || !(1..=100).contains(&size) {
        return Err(ClientError::Config("invalid tenant page".into()));
    }
    Ok(format!("page={page}&page_size={size}"))
}
#[derive(Debug, Clone)]
pub struct TenantControlApi {
    client: ApiClient,
    base: String,
}
impl TenantControlApi {
    pub fn new(client: &ApiClient, tenant_id: Uuid) -> Result<Self> {
        real_id(tenant_id)?;
        Ok(Self {
            client: client.clone(),
            base: format!("/api/v1/tenants/{tenant_id}"),
        })
    }
    async fn command<T: DeserializeOwned, B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: &B,
        token: &str,
    ) -> Result<T> {
        let request = self
            .client
            .request_with_auth(method, path, Some(token))
            .await?;
        self.client.send_and_parse(request.json(body)).await
    }
    pub async fn context(&self, token: &str) -> Result<TenantContext> {
        self.client.get_json_fresh(&self.base, Some(token)).await
    }
    pub async fn patch_context(&self, body: &TenantPatch, token: &str) -> Result<TenantContext> {
        version(body.expected_authz_version)?;
        self.command(Method::PATCH, &self.base, body, token).await
    }
    pub async fn members(
        &self,
        page: u32,
        size: u32,
        search: Option<&str>,
        token: &str,
    ) -> Result<Page<TenantMember>> {
        let mut path = format!("{}/members?{}", self.base, page_query(page, size)?);
        if let Some(search) = search.filter(|s| !s.trim().is_empty()) {
            if search.len() > 255 {
                return Err(ClientError::Config("member search is too long".into()));
            }
            path.push_str(&format!(
                "&search={}",
                super::common::encode_query_value(search)
            ));
        }
        self.client.get_json_fresh(&path, Some(token)).await
    }
    pub async fn member(&self, user_id: Uuid, token: &str) -> Result<TenantMember> {
        real_id(user_id)?;
        self.client
            .get_json_fresh(&format!("{}/members/{user_id}", self.base), Some(token))
            .await
    }
    pub async fn patch_member(
        &self,
        user_id: Uuid,
        body: &MemberPatch,
        token: &str,
    ) -> Result<TenantMember> {
        real_id(user_id)?;
        version(body.expected_authz_version)?;
        self.command(
            Method::PATCH,
            &format!("{}/members/{user_id}", self.base),
            body,
            token,
        )
        .await
    }
    pub async fn remove_member(
        &self,
        user_id: Uuid,
        expected_authz_version: i64,
        token: &str,
    ) -> Result<TenantMember> {
        real_id(user_id)?;
        version(expected_authz_version)?;
        self.command(
            Method::DELETE,
            &format!("{}/members/{user_id}", self.base),
            &serde_json::json!({"expected_authz_version":expected_authz_version}),
            token,
        )
        .await
    }
    pub async fn invitations(
        &self,
        page: u32,
        size: u32,
        token: &str,
    ) -> Result<Page<TenantInvitation>> {
        self.client
            .get_json_fresh(
                &format!("{}/invitations?{}", self.base, page_query(page, size)?),
                Some(token),
            )
            .await
    }
    pub async fn create_invitation(
        &self,
        body: &CreateInvitation,
        token: &str,
    ) -> Result<InvitationCreateResponse> {
        if !(300..=604800).contains(&body.expires_in_seconds) {
            return Err(ClientError::Config(
                "invitation duration is outside the supported range".into(),
            ));
        }
        self.client
            .post_json(&format!("{}/invitations", self.base), body, Some(token))
            .await
    }
    pub async fn revoke_invitation(
        &self,
        invitation_id: Uuid,
        token: &str,
    ) -> Result<TenantInvitation> {
        real_id(invitation_id)?;
        self.client
            .post_json(
                &format!("{}/invitations/{invitation_id}/revoke", self.base),
                &serde_json::json!({}),
                Some(token),
            )
            .await
    }
    pub async fn transfer_ownership(&self, new_owner: Uuid, token: &str) -> Result<TenantContext> {
        real_id(new_owner)?;
        self.client
            .post_json(
                &format!("{}/transfer-ownership", self.base),
                &serde_json::json!({"new_owner_user_id":new_owner}),
                Some(token),
            )
            .await
    }
    pub async fn audit(&self, page: u32, size: u32, token: &str) -> Result<Page<TenantAuditEvent>> {
        self.client
            .get_json_fresh(
                &format!("{}/audit-events?{}", self.base, page_query(page, size)?),
                Some(token),
            )
            .await
    }
}
/// This URL contains a one-time secret: redact transport and reflected server errors.
/// Single-dispatch policy is deliberate, even when a network outcome is ambiguous.
pub async fn accept_invitation(
    client: &ApiClient,
    invitation: &InvitationToken,
    token: &str,
) -> Result<AcceptInvitationResponse> {
    let path = format!("/api/v1/invitations/{}/accept", invitation.0);
    client
        .post_json(&path, &serde_json::json!({}), Some(token))
        .await
        .map_err(safe_invitation_error)
}
fn safe_invitation_error(error: ClientError) -> ClientError {
    let message =
        "Invitation acceptance failed; refresh memberships to check the outcome".to_owned();
    match error {
        ClientError::Unauthorized(_) => ClientError::Unauthorized(message),
        ClientError::Forbidden(_) => ClientError::Forbidden(message),
        ClientError::NotFound(_) => ClientError::NotFound(message),
        ClientError::RateLimited(mut info) => {
            info.message = message;
            info.scope = None;
            ClientError::RateLimited(info)
        }
        ClientError::Verification(_) => ClientError::Verification(message),
        ClientError::ServiceUnavailable(_) => ClientError::ServiceUnavailable(message),
        ClientError::ServerError(_) => ClientError::ServerError(message),
        ClientError::Network(_) => ClientError::Network(message),
        _ => ClientError::Other(message),
    }
}
