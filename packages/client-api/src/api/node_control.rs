//! Canonical node control client. Tenant targets are explicit, never inferred.
use crate::{
    client::ApiClient,
    error::{ClientError, Result},
};
use serde::{Deserialize, Serialize};
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
pub struct NodeInfo {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub owner_user_id: Uuid,
    pub display_name: String,
    pub status: String,
    pub consecutive_failure_count: i32,
    pub failure_threshold: i32,
    pub last_heartbeat_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct TaskInfo {
    pub id: Uuid,
    pub request_id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub model: String,
    pub status: String,
    pub assigned_node_id: Option<Uuid>,
    pub failure_count: i32,
    pub failure_threshold: i32,
    pub queued_at: String,
    pub claimed_at: Option<String>,
    pub finished_at: Option<String>,
    pub deadline_at: String,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct RegistrationInfo {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub token_preview: String,
    pub status: String,
    pub is_revealed: bool,
    pub approved_by: Option<Uuid>,
    pub actioned_at: Option<String>,
    pub consumed_at: Option<String>,
    pub consumed_node_id: Option<Uuid>,
    pub issued_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct NodeChange {
    pub node: NodeInfo,
    pub changed: bool,
    pub deleted: bool,
}
#[derive(Debug, Clone, Deserialize)]
pub struct RegistrationChange {
    pub token: RegistrationInfo,
    pub changed: bool,
    pub notification: String,
}
#[derive(Debug, Clone, Default)]
pub struct NodeListQuery {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub owner_user_id: Option<Uuid>,
    pub status: Option<String>,
    pub search: Option<String>,
}
impl NodeListQuery {
    fn query(&self) -> String {
        use super::common::encode_query_value;
        let mut values = Vec::new();
        if let Some(v) = self.page {
            values.push(format!("page={v}"));
        }
        if let Some(v) = self.page_size {
            values.push(format!("page_size={v}"));
        }
        if let Some(v) = self.owner_user_id {
            values.push(format!("owner_user_id={v}"));
        }
        if let Some(v) = &self.status {
            values.push(format!("status={}", encode_query_value(v)));
        }
        if let Some(v) = &self.search {
            values.push(format!("search={}", encode_query_value(v)));
        }
        values.join("&")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeCommand {
    pub expected_updated_at: String,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct NodePatch {
    pub expected_updated_at: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_threshold: Option<i32>,
}
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationAction {
    Approve,
    Reject,
    Revoke,
}
#[derive(Debug, Clone, Serialize)]
pub struct RegistrationCommand {
    pub expected_updated_at: String,
    pub reason: String,
    pub action: RegistrationAction,
}
#[derive(Debug, Clone, Copy)]
pub enum NodeOperation {
    Exclude,
    Recover,
    Revoke,
}
#[derive(Debug, Clone)]
pub struct NodeControlApi {
    client: ApiClient,
    base: String,
    personal: bool,
}
impl NodeControlApi {
    pub fn tenant(client: &ApiClient, tenant: Uuid) -> Result<Self> {
        Self::target(client, tenant, false)
    }
    pub fn platform_tenant(client: &ApiClient, tenant: Uuid) -> Result<Self> {
        Self::target(client, tenant, true)
    }
    pub fn personal(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
            base: "/api/v1/me".into(),
            personal: true,
        }
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
            personal: false,
        })
    }
    fn resource(&self, kind: &str, id: Uuid) -> Result<String> {
        if id.is_nil() {
            return Err(ClientError::Config("A real resource ID is required".into()));
        }
        Ok(format!("{}/{kind}/{id}", self.base))
    }
    fn writable(&self) -> Result<()> {
        if self.personal {
            Err(ClientError::Config(
                "Use tenant/platform administrative actions, not personal metadata routes".into(),
            ))
        } else {
            Ok(())
        }
    }
    pub async fn nodes(&self, q: &NodeListQuery, token: &str) -> Result<Page<NodeInfo>> {
        self.client
            .get_json_fresh(&format!("{}/nodes?{}", self.base, q.query()), Some(token))
            .await
    }
    pub async fn node(&self, id: Uuid, token: &str) -> Result<NodeInfo> {
        self.client
            .get_json_fresh(&self.resource("nodes", id)?, Some(token))
            .await
    }
    pub async fn tasks(&self, q: &NodeListQuery, token: &str) -> Result<Page<TaskInfo>> {
        self.client
            .get_json_fresh(&format!("{}/tasks?{}", self.base, q.query()), Some(token))
            .await
    }
    pub async fn task(&self, id: Uuid, token: &str) -> Result<TaskInfo> {
        self.client
            .get_json_fresh(&self.resource("tasks", id)?, Some(token))
            .await
    }
    pub async fn registrations(
        &self,
        q: &NodeListQuery,
        token: &str,
    ) -> Result<Page<RegistrationInfo>> {
        self.client
            .get_json_fresh(
                &format!("{}/node-registrations?{}", self.base, q.query()),
                Some(token),
            )
            .await
    }
    pub async fn registration(&self, id: Uuid, token: &str) -> Result<RegistrationInfo> {
        self.client
            .get_json_fresh(&self.resource("node-registrations", id)?, Some(token))
            .await
    }
    pub async fn configure(&self, id: Uuid, patch: &NodePatch, token: &str) -> Result<NodeChange> {
        self.writable()?;
        let r = self
            .client
            .request_with_auth(
                reqwest::Method::PATCH,
                &self.resource("nodes", id)?,
                Some(token),
            )
            .await?;
        self.client.send_and_parse(r.json(patch)).await
    }
    pub async fn operate(
        &self,
        id: Uuid,
        op: NodeOperation,
        command: &NodeCommand,
        token: &str,
    ) -> Result<NodeChange> {
        self.writable()?;
        let action = match op {
            NodeOperation::Exclude => "exclude",
            NodeOperation::Recover => "recover",
            NodeOperation::Revoke => "revoke",
        };
        self.client
            .post_json(
                &format!("{}/{}", self.resource("nodes", id)?, action),
                command,
                Some(token),
            )
            .await
    }
    pub async fn delete(&self, id: Uuid, command: &NodeCommand, token: &str) -> Result<NodeChange> {
        self.writable()?;
        let q = format!(
            "expected_updated_at={}&reason={}",
            super::common::encode_query_value(&command.expected_updated_at),
            super::common::encode_query_value(&command.reason)
        );
        self.client
            .delete_json(&format!("{}?{q}", self.resource("nodes", id)?), Some(token))
            .await
    }
    pub async fn decide_registration(
        &self,
        id: Uuid,
        command: &RegistrationCommand,
        token: &str,
    ) -> Result<RegistrationChange> {
        self.writable()?;
        self.client
            .post_json(
                &self.resource("node-registrations", id)?,
                command,
                Some(token),
            )
            .await
    }
}
