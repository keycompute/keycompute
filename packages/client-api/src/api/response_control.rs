//! Tenant/root Responses and Conversation administration.
use crate::{
    client::ApiClient,
    error::{ClientError, Result},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseMode {
    Passthrough,
    NodeDispatch,
    AccountPool,
}

impl ResponseMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passthrough => "passthrough",
            Self::NodeDispatch => "node_dispatch",
            Self::AccountPool => "account_pool",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ResourceListQuery {
    pub mode: Option<ResponseMode>,
    pub owner_user_id: Option<Uuid>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub reason: Option<String>,
}

impl ResourceListQuery {
    fn validate(&self) -> Result<()> {
        if !(1..=1_000_000).contains(&self.page.unwrap_or(1))
            || !(1..=100).contains(&self.page_size.unwrap_or(20))
        {
            return Err(ClientError::Config("Invalid resource pagination".into()));
        }
        Ok(())
    }
    fn query(&self) -> Result<String> {
        self.validate()?;
        let mode = self
            .mode
            .ok_or_else(|| ClientError::Config("Responses mode is required".into()))?;
        let mut values = vec![format!("mode={}", mode.as_str())];
        if let Some(owner) = self.owner_user_id {
            if owner.is_nil() {
                return Err(ClientError::Config("owner_user_id must be real".into()));
            }
            values.push(format!("owner_user_id={owner}"));
        }
        if let Some(page) = self.page {
            values.push(format!("page={page}"));
        }
        if let Some(page_size) = self.page_size {
            values.push(format!("page_size={page_size}"));
        }
        if let Some(reason) = &self.reason {
            values.push(format!(
                "reason={}",
                super::common::encode_query_value(reason)
            ));
        }
        Ok(values.join("&"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Count {
    pub total: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseSummary {
    pub id: String,
    pub tenant_id: Uuid,
    pub owner_user_id: Uuid,
    pub mode: String,
    pub provider: Option<String>,
    pub account_id: Option<Uuid>,
    pub model: Option<String>,
    pub status: String,
    pub background: bool,
    pub store_response: bool,
    pub stream: bool,
    pub previous_response_id: Option<String>,
    pub conversation_id: Option<String>,
    pub revision: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    pub expires_at: String,
    pub deleted: bool,
    pub local_content_available: bool,
    pub native_content_available: bool,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub id: String,
    pub tenant_id: Uuid,
    pub owner_user_id: Uuid,
    pub mode: String,
    pub account_id: Option<Uuid>,
    pub model: Option<String>,
    pub metadata: Value,
    pub active_response_id: Option<String>,
    pub revision: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    pub expires_at: String,
    pub deleted: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ResponseDetail {
    pub summary: ResponseSummary,
    pub response: Option<Value>,
    #[serde(default)]
    pub native_body: Option<Value>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ConversationDetail {
    pub summary: ConversationSummary,
    pub conversation: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deleted {
    pub id: String,
    pub object: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionCommand {
    pub expected_revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct MetadataCommand {
    pub expected_revision: i64,
    pub metadata: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AppendItemsCommand {
    pub expected_revision: i64,
    pub items: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceAddress {
    pub mode: ResponseMode,
    pub owner: Uuid,
    pub id: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ItemOrder {
    Asc,
    #[default]
    Desc,
}
impl ItemOrder {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemQuery {
    pub after: Option<String>,
    pub limit: u32,
    pub order: ItemOrder,
}
impl Default for ItemQuery {
    fn default() -> Self {
        Self {
            after: None,
            limit: 20,
            order: ItemOrder::Desc,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ItemPage {
    pub object: String,
    pub data: Vec<Value>,
    pub first_id: Option<String>,
    pub last_id: Option<String>,
    pub has_more: bool,
}
impl std::fmt::Debug for ItemPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ItemPage")
            .field("count", &self.data.len())
            .field("content", &"[REDACTED]")
            .finish()
    }
}
fn scope_error() -> ClientError {
    ClientError::InvalidResponse(
        "Response resource did not match the requested tenant, owner, mode or revision".into(),
    )
}
fn checked_revision(value: i64) -> Result<()> {
    if value <= 0 {
        Err(ClientError::Config(
            "A current positive resource revision is required".into(),
        ))
    } else {
        Ok(())
    }
}
fn content_limit(value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| ClientError::Config("Invalid resource content".into()))?;
    if bytes.len() > 2 * 1024 * 1024 {
        Err(ClientError::Config("Resource content is too large".into()))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ResponseControlApi {
    client: ApiClient,
    base: String,
    tenant: Uuid,
    platform: bool,
}

impl ResponseControlApi {
    pub fn tenant(client: &ApiClient, tenant: Uuid) -> Result<Self> {
        Self::target(client, tenant, false)
    }

    pub fn platform_tenant(client: &ApiClient, tenant: Uuid) -> Result<Self> {
        Self::target(client, tenant, true)
    }

    fn target(client: &ApiClient, tenant: Uuid, platform: bool) -> Result<Self> {
        if tenant.is_nil() {
            return Err(ClientError::Config("An explicit tenant is required".into()));
        }
        let base = if platform {
            format!("/api/v1/platform/tenants/{tenant}")
        } else {
            format!("/api/v1/tenants/{tenant}")
        };
        Ok(Self {
            client: client.clone(),
            base,
            tenant,
            platform,
        })
    }

    fn reason(&self, reason: Option<&str>) -> Result<()> {
        if self.platform && reason.is_none() {
            return Err(ClientError::Config(
                "A reason is required for platform resource access".into(),
            ));
        }
        if reason.is_some_and(|v| {
            v.trim().is_empty() || v.len() > 500 || v.chars().any(char::is_control)
        }) {
            return Err(ClientError::Config(
                "Use a bounded nonempty resource access reason".into(),
            ));
        }
        Ok(())
    }
    fn check_response(
        &self,
        row: &ResponseSummary,
        mode: ResponseMode,
        owner: Option<Uuid>,
        id: Option<&str>,
    ) -> Result<()> {
        if row.tenant_id != self.tenant
            || row.owner_user_id.is_nil()
            || row.mode != mode.as_str()
            || owner.is_some_and(|v| v != row.owner_user_id)
            || id.is_some_and(|v| v != row.id)
            || resource_segment(&row.id).is_err()
            || (mode != ResponseMode::AccountPool && row.revision.is_none_or(|v| v <= 0))
        {
            return Err(scope_error());
        }
        Ok(())
    }
    fn check_conversation(
        &self,
        row: &ConversationSummary,
        mode: ResponseMode,
        owner: Option<Uuid>,
        id: Option<&str>,
    ) -> Result<()> {
        if row.tenant_id != self.tenant
            || row.owner_user_id.is_nil()
            || row.mode != mode.as_str()
            || owner.is_some_and(|v| v != row.owner_user_id)
            || id.is_some_and(|v| v != row.id)
            || resource_segment(&row.id).is_err()
            || (mode != ResponseMode::AccountPool && row.revision.is_none_or(|v| v <= 0))
        {
            return Err(scope_error());
        }
        Ok(())
    }
    fn check_page<T>(&self, page: &Page<T>, q: &ResourceListQuery) -> Result<()> {
        let size = q.page_size.unwrap_or(20);
        if page.page != q.page.unwrap_or(1)
            || page.page_size != size
            || page.items.len() > size as usize
            || page.total < 0
            || page.total_pages != page.total / size + i64::from(page.total % size != 0)
        {
            return Err(scope_error());
        }
        Ok(())
    }
    fn check_deleted(&self, result: &Deleted, id: &str, kind: &str) -> Result<()> {
        if result.id != id || result.object != kind || !result.deleted {
            return Err(scope_error());
        }
        Ok(())
    }
    fn check_body(&self, result: &Value, id: &str, kind: &str) -> Result<()> {
        if result.get("id").and_then(Value::as_str) != Some(id)
            || result.get("object").and_then(Value::as_str) != Some(kind)
        {
            return Err(scope_error());
        }
        Ok(())
    }
    fn response_resource(&self, mode: ResponseMode, owner: Uuid, id: &str) -> Result<String> {
        if owner.is_nil() {
            return Err(ClientError::Config(
                "A real owner and resource ID are required".into(),
            ));
        }
        let id = resource_segment(id)?;
        Ok(format!(
            "{}/responses/{}/{owner}/{id}",
            self.base,
            mode.as_str()
        ))
    }

    fn conversation_resource(&self, mode: ResponseMode, owner: Uuid, id: &str) -> Result<String> {
        if owner.is_nil() {
            return Err(ClientError::Config(
                "A real owner and resource ID are required".into(),
            ));
        }
        let id = resource_segment(id)?;
        Ok(format!(
            "{}/conversations/{}/{owner}/{id}",
            self.base,
            mode.as_str()
        ))
    }

    pub async fn responses(
        &self,
        q: &ResourceListQuery,
        token: &str,
    ) -> Result<Page<ResponseSummary>> {
        self.reason(q.reason.as_deref())?;
        let query = q.query()?;
        let mode = q
            .mode
            .ok_or_else(|| ClientError::Config("Mode required".into()))?;
        let result: Page<ResponseSummary> = self
            .client
            .get_json_fresh(&format!("{}/responses?{query}", self.base), Some(token))
            .await?;
        self.check_page(&result, q)?;
        let mut identities = std::collections::HashSet::new();
        for row in &result.items {
            self.check_response(row, mode, q.owner_user_id, None)?;
            if !identities.insert((row.owner_user_id, row.id.as_str())) {
                return Err(scope_error());
            }
        }
        Ok(result)
    }

    pub async fn response_count(&self, q: &ResourceListQuery, token: &str) -> Result<Count> {
        self.reason(q.reason.as_deref())?;
        let result: Count = self
            .client
            .get_json_fresh(
                &format!("{}/responses/count?{}", self.base, q.query()?),
                Some(token),
            )
            .await?;
        if result.total < 0 {
            return Err(scope_error());
        }
        Ok(result)
    }

    pub async fn response(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        reason: Option<&str>,
        token: &str,
    ) -> Result<ResponseDetail> {
        self.reason(reason)?;
        let mut path = self.response_resource(mode, owner, id)?;
        if let Some(reason) = reason {
            path.push_str(&format!(
                "?reason={}",
                super::common::encode_query_value(reason)
            ));
        }
        let result: ResponseDetail = self.client.get_json_fresh(&path, Some(token)).await?;
        self.check_response(&result.summary, mode, Some(owner), Some(id))?;
        if mode != ResponseMode::AccountPool
            && (result.response.is_none()
                || result.native_body.is_some()
                || !result.summary.local_content_available
                || result.summary.native_content_available)
        {
            return Err(scope_error());
        }
        for body in [&result.response, &result.native_body]
            .into_iter()
            .flatten()
        {
            self.check_body(body, id, "response")?;
        }
        Ok(result)
    }

    pub async fn cancel_response(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        body: &RevisionCommand,
        token: &str,
    ) -> Result<Value> {
        checked_revision(body.expected_revision)?;
        self.reason(body.reason.as_deref())?;
        let result: Value = self
            .client
            .post_json(
                &format!("{}/cancel", self.response_resource(mode, owner, id)?),
                body,
                Some(token),
            )
            .await?;
        self.check_body(&result, id, "response")?;
        Ok(result)
    }

    pub async fn delete_response(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        body: &RevisionCommand,
        token: &str,
    ) -> Result<Deleted> {
        checked_revision(body.expected_revision)?;
        self.reason(body.reason.as_deref())?;
        let request = self
            .client
            .request_with_auth(
                reqwest::Method::DELETE,
                &self.response_resource(mode, owner, id)?,
                Some(token),
            )
            .await?;
        let result: Deleted = self.client.send_and_parse(request.json(body)).await?;
        self.check_deleted(&result, id, "response")?;
        Ok(result)
    }

    /// Default item page with the same validation as the typed cursor API.
    pub async fn response_input_items(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        reason: Option<&str>,
        token: &str,
    ) -> Result<Value> {
        let page = self
            .response_input_items_page(
                &ResourceAddress {
                    mode,
                    owner,
                    id: id.to_owned(),
                },
                &ItemQuery::default(),
                reason,
                token,
            )
            .await?;
        serde_json::to_value(page).map_err(|_| scope_error())
    }

    pub async fn conversations(
        &self,
        q: &ResourceListQuery,
        token: &str,
    ) -> Result<Page<ConversationSummary>> {
        self.reason(q.reason.as_deref())?;
        let query = q.query()?;
        let mode = q
            .mode
            .ok_or_else(|| ClientError::Config("Mode required".into()))?;
        let result: Page<ConversationSummary> = self
            .client
            .get_json_fresh(&format!("{}/conversations?{query}", self.base), Some(token))
            .await?;
        self.check_page(&result, q)?;
        let mut identities = std::collections::HashSet::new();
        for row in &result.items {
            self.check_conversation(row, mode, q.owner_user_id, None)?;
            if !identities.insert((row.owner_user_id, row.id.as_str())) {
                return Err(scope_error());
            }
        }
        Ok(result)
    }

    pub async fn conversation_count(&self, q: &ResourceListQuery, token: &str) -> Result<Count> {
        self.reason(q.reason.as_deref())?;
        let result: Count = self
            .client
            .get_json_fresh(
                &format!("{}/conversations/count?{}", self.base, q.query()?),
                Some(token),
            )
            .await?;
        if result.total < 0 {
            return Err(scope_error());
        }
        Ok(result)
    }

    pub async fn conversation(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        reason: Option<&str>,
        token: &str,
    ) -> Result<ConversationDetail> {
        self.reason(reason)?;
        let mut path = self.conversation_resource(mode, owner, id)?;
        if let Some(reason) = reason {
            path.push_str(&format!(
                "?reason={}",
                super::common::encode_query_value(reason)
            ));
        }
        let result: ConversationDetail = self.client.get_json_fresh(&path, Some(token)).await?;
        self.check_conversation(&result.summary, mode, Some(owner), Some(id))?;
        self.check_body(&result.conversation, id, "conversation")?;
        Ok(result)
    }

    pub async fn update_conversation(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        body: &MetadataCommand,
        token: &str,
    ) -> Result<Value> {
        checked_revision(body.expected_revision)?;
        self.reason(body.reason.as_deref())?;
        content_limit(&body.metadata)?;
        let request = self
            .client
            .request_with_auth(
                reqwest::Method::PATCH,
                &self.conversation_resource(mode, owner, id)?,
                Some(token),
            )
            .await?;
        let result: Value = self.client.send_and_parse(request.json(body)).await?;
        self.check_body(&result, id, "conversation")?;
        Ok(result)
    }

    pub async fn delete_conversation(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        body: &RevisionCommand,
        token: &str,
    ) -> Result<Deleted> {
        checked_revision(body.expected_revision)?;
        self.reason(body.reason.as_deref())?;
        let request = self
            .client
            .request_with_auth(
                reqwest::Method::DELETE,
                &self.conversation_resource(mode, owner, id)?,
                Some(token),
            )
            .await?;
        let result: Deleted = self.client.send_and_parse(request.json(body)).await?;
        self.check_deleted(&result, id, "conversation")?;
        Ok(result)
    }

    /// Default item page with the same validation as the typed cursor API.
    pub async fn conversation_items(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        reason: Option<&str>,
        token: &str,
    ) -> Result<Value> {
        let page = self
            .conversation_items_page(
                &ResourceAddress {
                    mode,
                    owner,
                    id: id.to_owned(),
                },
                &ItemQuery::default(),
                reason,
                token,
            )
            .await?;
        serde_json::to_value(page).map_err(|_| scope_error())
    }

    pub async fn append_conversation_items(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        body: &AppendItemsCommand,
        token: &str,
    ) -> Result<Value> {
        checked_revision(body.expected_revision)?;
        self.reason(body.reason.as_deref())?;
        if body.items.is_empty()
            || body.items.len() > 512
            || body.items.iter().any(|item| !item.is_object())
        {
            return Err(ClientError::Config(
                "Use between one and 512 conversation items".into(),
            ));
        }
        content_limit(&body.items)?;
        let result: Value = self
            .client
            .post_json(
                &format!("{}/items", self.conversation_resource(mode, owner, id)?),
                body,
                Some(token),
            )
            .await?;
        self.check_body(&result, id, "conversation")?;
        Ok(result)
    }

    pub async fn remove_conversation_item(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        item_id: &str,
        body: &RevisionCommand,
        token: &str,
    ) -> Result<Deleted> {
        checked_revision(body.expected_revision)?;
        self.reason(body.reason.as_deref())?;
        let item_id_raw = item_id;
        let item_id = resource_segment(item_id)?;
        let request = self
            .client
            .request_with_auth(
                reqwest::Method::DELETE,
                &format!(
                    "{}/items/{item_id}",
                    self.conversation_resource(mode, owner, id)?
                ),
                Some(token),
            )
            .await?;
        let result: Deleted = self.client.send_and_parse(request.json(body)).await?;
        self.check_deleted(&result, item_id_raw, "conversation.item")?;
        Ok(result)
    }
    async fn item_page(
        &self,
        address: &ResourceAddress,
        q: &ItemQuery,
        reason: Option<&str>,
        token: &str,
        conversation: bool,
    ) -> Result<ItemPage> {
        self.reason(reason)?;
        if !(1..=100).contains(&q.limit) {
            return Err(ClientError::Config(
                "Item limit must be between one and 100".into(),
            ));
        }
        let resource = if conversation {
            self.conversation_resource(address.mode, address.owner, &address.id)?
        } else {
            self.response_resource(address.mode, address.owner, &address.id)?
        };
        let mut path = format!(
            "{resource}/{}?limit={}&order={}",
            if conversation { "items" } else { "input_items" },
            q.limit,
            q.order.as_str()
        );
        if let Some(after) = &q.after {
            path.push_str(&format!("&after={}", resource_segment(after)?));
        }
        if let Some(reason) = reason {
            path.push_str(&format!(
                "&reason={}",
                super::common::encode_query_value(reason)
            ));
        }
        let result: ItemPage = self.client.get_json_fresh(&path, Some(token)).await?;
        let first = result
            .data
            .first()
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str);
        let last = result
            .data
            .last()
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str);
        if result.object != "list"
            || result.data.len() > q.limit as usize
            || result.has_more && result.data.is_empty()
            || result.first_id.as_deref() != first
            || result.last_id.as_deref() != last
            || result.data.iter().any(|v| {
                v.get("id")
                    .and_then(Value::as_str)
                    .is_none_or(|id| resource_segment(id).is_err())
            })
        {
            return Err(scope_error());
        }
        let mut ids = std::collections::HashSet::new();
        for item in &result.data {
            let id = item["id"].as_str().ok_or_else(scope_error)?;
            if q.after.as_deref() == Some(id) || !ids.insert(id) {
                return Err(scope_error());
            }
        }
        Ok(result)
    }
    pub async fn response_input_items_page(
        &self,
        address: &ResourceAddress,
        q: &ItemQuery,
        reason: Option<&str>,
        token: &str,
    ) -> Result<ItemPage> {
        self.item_page(address, q, reason, token, false).await
    }
    pub async fn conversation_items_page(
        &self,
        address: &ResourceAddress,
        q: &ItemQuery,
        reason: Option<&str>,
        token: &str,
    ) -> Result<ItemPage> {
        self.item_page(address, q, reason, token, true).await
    }
}

// Resource content and caller metadata never enter diagnostic formatting.
macro_rules! redact_content_debug {
    ($($ty:ty),+ $(,)?) => {$ (
        impl std::fmt::Debug for $ty {
            fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {
                f.debug_struct(stringify!($ty)).field("content",&"[REDACTED]").finish_non_exhaustive()
            }
        }
    )+};
}
redact_content_debug!(
    ResponseDetail,
    ConversationDetail,
    ConversationSummary,
    MetadataCommand,
    AppendItemsCommand
);

// Opaque provider IDs remain data even when they contain reserved characters.
fn resource_segment(id: &str) -> Result<String> {
    if id.trim().is_empty()
        || id.len() > 2048
        || id.chars().any(char::is_control)
        || matches!(id, "." | "..")
    {
        return Err(ClientError::Config(
            "A valid opaque resource ID is required".into(),
        ));
    }
    Ok(super::common::encode_query_value(id))
}
#[cfg(test)]
mod resource_path_tests {
    use super::*;
    #[test]
    fn resource_selectors_cannot_change_route_or_query() {
        assert_eq!(resource_segment("resp_123").unwrap(), "resp_123");
        assert_eq!(
            resource_segment("future.conversation/id:1").unwrap(),
            "future.conversation%2Fid%3A1"
        );
        assert_eq!(
            resource_segment("r?reason=override#x").unwrap(),
            "r%3Freason%3Doverride%23x"
        );
        for id in ["", " ", ".", "..", "r\n"] {
            assert!(resource_segment(id).is_err());
        }
    }
}
