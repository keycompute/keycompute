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
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub status: String,
    pub default_rpm_limit: i32,
    pub default_tpm_limit: i32,
    pub revision: i64,
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
    pub scope_type: String,
    pub tenant_id: Option<Uuid>,
    pub actor_user_id: Uuid,
    #[serde(default)]
    pub actor_email: Option<String>,
    #[serde(default)]
    pub actor_name: Option<String>,
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TenantAuditQuery {
    pub page: u32,
    pub page_size: u32,
    pub actor_user_id: Option<Uuid>,
    pub request_id: Option<Uuid>,
    pub action: Option<String>,
    pub result: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct TenantPatch {
    pub expected_revision: i64,
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
pub struct GrantMember {
    pub user_id: Uuid,
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

    /// Explicit platform-target administration. This does not select the
    /// tenant or create a membership for the root/operator session.
    pub fn platform_tenant(client: &ApiClient, tenant_id: Uuid) -> Result<Self> {
        real_id(tenant_id)?;
        Ok(Self {
            client: client.clone(),
            base: format!("/api/v1/platform/tenants/{tenant_id}/control"),
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
        version(body.expected_revision)?;
        self.command(Method::PATCH, &self.base, body, token).await
    }
    pub async fn members(
        &self,
        page: u32,
        size: u32,
        search: Option<&str>,
        token: &str,
    ) -> Result<Page<TenantMember>> {
        self.members_filtered(page, size, search, None, token).await
    }
    pub async fn members_filtered(
        &self,
        page: u32,
        size: u32,
        search: Option<&str>,
        status: Option<MembershipStatus>,
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
        if let Some(status) = status {
            path.push_str("&status=");
            path.push_str(status.as_str());
        }
        let out: Page<TenantMember> = self.client.get_json_fresh(&path, Some(token)).await?;
        let expected_pages =
            out.total / i64::from(size) + i64::from(out.total % i64::from(size) != 0);
        if out.page != i64::from(page)
            || out.page_size != i64::from(size)
            || out.total < 0
            || out.total_pages != expected_pages
            || out.items.len() > size as usize
            || out.items.len() as i64 > out.total
            || out.items.iter().any(|member| {
                member.user_id.is_nil()
                    || status.is_some_and(|expected| member.membership_status != expected)
            })
        {
            return Err(ClientError::InvalidResponse(
                "Invalid tenant member page".into(),
            ));
        }
        Ok(out)
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
    pub async fn grant_member(&self, body: &GrantMember, token: &str) -> Result<TenantMember> {
        if !self.base.starts_with("/api/v1/platform/") {
            return Err(ClientError::Config(
                "Direct tenant authorization requires a platform target".into(),
            ));
        }
        real_id(body.user_id)?;
        self.client
            .post_json(&format!("{}/members", self.base), body, Some(token))
            .await
    }
    pub async fn invitations(
        &self,
        page: u32,
        size: u32,
        token: &str,
    ) -> Result<Page<TenantInvitation>> {
        if self.base.starts_with("/api/v1/platform/") {
            return Err(ClientError::Config(
                "Invitations require a tenant administrator membership".into(),
            ));
        }
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
        if self.base.starts_with("/api/v1/platform/") {
            return Err(ClientError::Config(
                "Invitations require a tenant administrator membership".into(),
            ));
        }
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
        if self.base.starts_with("/api/v1/platform/") {
            return Err(ClientError::Config(
                "Invitations require a tenant administrator membership".into(),
            ));
        }
        real_id(invitation_id)?;
        self.client
            .post_json(
                &format!("{}/invitations/{invitation_id}/revoke", self.base),
                &serde_json::json!({}),
                Some(token),
            )
            .await
    }
    pub async fn audit(&self, page: u32, size: u32, token: &str) -> Result<Page<TenantAuditEvent>> {
        self.audit_filtered(
            &TenantAuditQuery {
                page,
                page_size: size,
                ..Default::default()
            },
            token,
        )
        .await
    }
    pub async fn audit_filtered(
        &self,
        query: &TenantAuditQuery,
        token: &str,
    ) -> Result<Page<TenantAuditEvent>> {
        if query.actor_user_id.is_some_and(|id| id.is_nil())
            || query.request_id.is_some_and(|id| id.is_nil())
        {
            return Err(ClientError::Config(
                "Audit UUID filters must be nonzero".into(),
            ));
        }
        let mut params = page_query(query.page, query.page_size)?;
        if let Some(id) = query.actor_user_id {
            params.push_str("&actor_user_id=");
            params.push_str(&id.to_string());
        }
        if let Some(id) = query.request_id {
            params.push_str("&request_id=");
            params.push_str(&id.to_string());
        }
        for (name, value) in [("action", &query.action), ("result", &query.result)] {
            if let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                if value.len() > 128 || value.chars().any(char::is_control) {
                    return Err(ClientError::Config("Invalid audit filter".into()));
                }
                params.push('&');
                params.push_str(name);
                params.push('=');
                params.push_str(&super::common::encode_query_value(value));
            }
        }
        if query.result.as_deref().map(str::trim).is_some_and(|value| {
            !value.is_empty() && !matches!(value, "success" | "denied" | "failure")
        }) {
            return Err(ClientError::Config("Invalid audit result filter".into()));
        }
        self.client
            .get_json_fresh(&format!("{}/audit-events?{params}", self.base), Some(token))
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
        ClientError::TenantSelectionRequired(_) => ClientError::Other(message),
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

#[cfg(test)]
mod audit_contract_tests {
    use super::TenantAuditEvent;
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn tenant_audit_event_preserves_authority_source() {
        let id = Uuid::new_v4();
        let actor = Uuid::new_v4();
        let event: TenantAuditEvent = serde_json::from_value(json!({
            "id": id,
            "scope_type": "platform",
            "tenant_id": Uuid::new_v4(),
            "actor_user_id": actor,
            "actor_email": "operator@example.com",
            "actor_name": "Operator",
            "action": "tenant.update",
            "resource_type": "tenant",
            "resource_id": null,
            "request_id": null,
            "credential_kind": "session",
            "platform_role": "operator",
            "tenant_role": null,
            "metadata": {},
            "result": "success",
            "created_at": "2026-10-18T00:00:00Z"
        }))
        .expect("server audit event should deserialize");

        assert_eq!(event.scope_type, "platform");
        assert_eq!(event.actor_user_id, actor);
    }
}
