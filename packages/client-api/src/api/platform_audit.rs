//! Read-only platform audit API for root and operator console sessions.

use super::common::encode_query_value;
use crate::{ApiClient, ClientError, Result};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlatformAuditQuery {
    pub tenant_id: Option<Uuid>,
    pub request_id: Option<Uuid>,
    pub cursor: Option<String>,
    pub page_size: u32,
}

impl PlatformAuditQuery {
    fn query_string(&self) -> Result<String> {
        let page_size = if self.page_size == 0 {
            20
        } else {
            self.page_size
        };
        if page_size > 100 {
            return Err(ClientError::Config(
                "Platform audit pagination is outside the supported range".into(),
            ));
        }
        if self.tenant_id.is_some_and(|id| id.is_nil())
            || self.request_id.is_some_and(|id| id.is_nil())
        {
            return Err(ClientError::Config(
                "Platform audit filters require nonzero UUIDs".into(),
            ));
        }
        if self.cursor.as_ref().is_some_and(|cursor| {
            cursor.is_empty()
                || cursor.len() > 1024
                || cursor
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control())
        }) {
            return Err(ClientError::Config(
                "Platform audit cursor is invalid".into(),
            ));
        }
        let mut values = vec![format!("page_size={page_size}")];
        if let Some(cursor) = self.cursor.as_deref() {
            values.push(format!("cursor={}", encode_query_value(cursor)));
        }
        if let Some(tenant_id) = self.tenant_id {
            values.push(format!("tenant_id={tenant_id}"));
        }
        if let Some(request_id) = self.request_id {
            values.push(format!("request_id={request_id}"));
        }
        Ok(values.join("&"))
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PlatformAuditRecord {
    pub id: Uuid,
    pub scope_type: String,
    pub tenant_id: Option<Uuid>,
    pub tenant_name: Option<String>,
    pub tenant_slug: Option<String>,
    pub actor_user_id: Uuid,
    pub actor_email: Option<String>,
    pub actor_name: Option<String>,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub request_id: Option<Uuid>,
    /// Older platform-audit servers do not return the correlation count yet.
    /// Treat that response as a single unaggregated record during rollout.
    #[serde(default)]
    pub request_event_count: i64,
    pub credential_kind: String,
    pub platform_role: String,
    pub tenant_role: Option<String>,
    pub metadata: serde_json::Value,
    pub result: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PlatformAuditPage {
    pub items: Vec<PlatformAuditRecord>,
    pub next_cursor: Option<String>,
    pub page_size: i64,
}

#[derive(Debug, Clone)]
pub struct PlatformAuditApi {
    client: ApiClient,
}

impl PlatformAuditApi {
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    pub async fn list(&self, query: &PlatformAuditQuery, token: &str) -> Result<PlatformAuditPage> {
        self.client
            .get_json_fresh(
                &format!("/api/v1/platform/audit-events?{}", query.query_string()?),
                Some(token),
            )
            .await
    }
}
