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
    fn query(&self) -> Result<String> {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone)]
pub struct ResponseControlApi {
    client: ApiClient,
    base: String,
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
        })
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
        self.client
            .get_json_fresh(
                &format!("{}/responses?{}", self.base, q.query()?),
                Some(token),
            )
            .await
    }

    pub async fn response_count(&self, q: &ResourceListQuery, token: &str) -> Result<Count> {
        self.client
            .get_json_fresh(
                &format!("{}/responses/count?{}", self.base, q.query()?),
                Some(token),
            )
            .await
    }

    pub async fn response(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        reason: Option<&str>,
        token: &str,
    ) -> Result<ResponseDetail> {
        let mut path = self.response_resource(mode, owner, id)?;
        if let Some(reason) = reason {
            path.push_str(&format!(
                "?reason={}",
                super::common::encode_query_value(reason)
            ));
        }
        self.client.get_json_fresh(&path, Some(token)).await
    }

    pub async fn cancel_response(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        body: &RevisionCommand,
        token: &str,
    ) -> Result<Value> {
        self.client
            .post_json(
                &format!("{}/cancel", self.response_resource(mode, owner, id)?),
                body,
                Some(token),
            )
            .await
    }

    pub async fn delete_response(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        body: &RevisionCommand,
        token: &str,
    ) -> Result<Deleted> {
        let request = self
            .client
            .request_with_auth(
                reqwest::Method::DELETE,
                &self.response_resource(mode, owner, id)?,
                Some(token),
            )
            .await?;
        self.client.send_and_parse(request.json(body)).await
    }

    pub async fn response_input_items(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        reason: Option<&str>,
        token: &str,
    ) -> Result<Value> {
        let mut path = format!("{}/input_items", self.response_resource(mode, owner, id)?);
        if let Some(reason) = reason {
            path.push_str(&format!(
                "?reason={}",
                super::common::encode_query_value(reason)
            ));
        }
        self.client.get_json_fresh(&path, Some(token)).await
    }

    pub async fn conversations(
        &self,
        q: &ResourceListQuery,
        token: &str,
    ) -> Result<Page<ConversationSummary>> {
        self.client
            .get_json_fresh(
                &format!("{}/conversations?{}", self.base, q.query()?),
                Some(token),
            )
            .await
    }

    pub async fn conversation_count(&self, q: &ResourceListQuery, token: &str) -> Result<Count> {
        self.client
            .get_json_fresh(
                &format!("{}/conversations/count?{}", self.base, q.query()?),
                Some(token),
            )
            .await
    }

    pub async fn conversation(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        reason: Option<&str>,
        token: &str,
    ) -> Result<ConversationDetail> {
        let mut path = self.conversation_resource(mode, owner, id)?;
        if let Some(reason) = reason {
            path.push_str(&format!(
                "?reason={}",
                super::common::encode_query_value(reason)
            ));
        }
        self.client.get_json_fresh(&path, Some(token)).await
    }

    pub async fn update_conversation(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        body: &MetadataCommand,
        token: &str,
    ) -> Result<Value> {
        let request = self
            .client
            .request_with_auth(
                reqwest::Method::PATCH,
                &self.conversation_resource(mode, owner, id)?,
                Some(token),
            )
            .await?;
        self.client.send_and_parse(request.json(body)).await
    }

    pub async fn delete_conversation(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        body: &RevisionCommand,
        token: &str,
    ) -> Result<Deleted> {
        let request = self
            .client
            .request_with_auth(
                reqwest::Method::DELETE,
                &self.conversation_resource(mode, owner, id)?,
                Some(token),
            )
            .await?;
        self.client.send_and_parse(request.json(body)).await
    }

    pub async fn conversation_items(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        reason: Option<&str>,
        token: &str,
    ) -> Result<Value> {
        let mut path = format!("{}/items", self.conversation_resource(mode, owner, id)?);
        if let Some(reason) = reason {
            path.push_str(&format!(
                "?reason={}",
                super::common::encode_query_value(reason)
            ));
        }
        self.client.get_json_fresh(&path, Some(token)).await
    }

    pub async fn append_conversation_items(
        &self,
        mode: ResponseMode,
        owner: Uuid,
        id: &str,
        body: &AppendItemsCommand,
        token: &str,
    ) -> Result<Value> {
        self.client
            .post_json(
                &format!("{}/items", self.conversation_resource(mode, owner, id)?),
                body,
                Some(token),
            )
            .await
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
        self.client.send_and_parse(request.json(body)).await
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
