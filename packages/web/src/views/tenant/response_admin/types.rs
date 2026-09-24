use client_api::{
    ClientError, Result,
    api::response_control::{
        ConversationSummary, ResourceAddress, ResourceListQuery, ResponseMode, ResponseSummary,
    },
};
use serde_json::Value;
use uuid::Uuid;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Responses,
    Conversations,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub kind: Kind,
    pub mode: ResponseMode,
    pub owner: Option<Uuid>,
    pub page: i64,
}
impl Default for Query {
    fn default() -> Self {
        Self {
            kind: Kind::Responses,
            mode: ResponseMode::Passthrough,
            owner: None,
            page: 1,
        }
    }
}
impl Query {
    pub fn api(&self) -> ResourceListQuery {
        ResourceListQuery {
            mode: Some(self.mode),
            owner_user_id: self.owner,
            page: Some(self.page),
            page_size: Some(20),
            reason: None,
        }
    }
}
pub fn mode(value: &str) -> Option<ResponseMode> {
    match value {
        "passthrough" => Some(ResponseMode::Passthrough),
        "node_dispatch" => Some(ResponseMode::NodeDispatch),
        _ => None,
    }
}
pub fn owner(value: &str) -> Result<Option<Uuid>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    Uuid::parse_str(value)
        .ok()
        .filter(|id| !id.is_nil())
        .map(Some)
        .ok_or_else(|| ClientError::Config("Use a real owner UUID".into()))
}
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Response(ResponseSummary),
    Conversation(ConversationSummary),
}
/// Full logical identity; resource IDs can collide between owners and families.
/// This key carries identifiers only, never conversation content or metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceIdentity {
    pub tenant: Uuid,
    pub owner: Uuid,
    pub kind: Kind,
    pub mode: String,
    pub id: String,
}
impl ResourceIdentity {
    fn key(&self) -> String {
        serde_json::json!([
            self.tenant,
            self.owner,
            match self.kind {
                Kind::Responses => "response",
                Kind::Conversations => "conversation",
            },
            self.mode,
            self.id
        ])
        .to_string()
    }
}
impl Row {
    pub fn identity(&self) -> ResourceIdentity {
        let (tenant, kind, mode) = match self {
            Self::Response(r) => (r.tenant_id, Kind::Responses, r.mode.clone()),
            Self::Conversation(r) => (r.tenant_id, Kind::Conversations, r.mode.clone()),
        };
        ResourceIdentity {
            tenant,
            owner: self.owner(),
            kind,
            mode,
            id: self.id().into(),
        }
    }
    pub fn key(&self) -> String {
        self.identity().key()
    }
    pub fn state_key(&self) -> String {
        serde_json::json!([self.key(), self.revision().ok()]).to_string()
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Response(r) => &r.id,
            Self::Conversation(r) => &r.id,
        }
    }
    pub fn owner(&self) -> Uuid {
        match self {
            Self::Response(r) => r.owner_user_id,
            Self::Conversation(r) => r.owner_user_id,
        }
    }
    pub fn mode(&self) -> Result<ResponseMode> {
        let value = match self {
            Self::Response(r) => &r.mode,
            Self::Conversation(r) => &r.mode,
        };
        mode(value).ok_or_else(|| {
            ClientError::InvalidResponse("Unsupported managed resource family".into())
        })
    }
    pub fn revision(&self) -> Result<i64> {
        match self {
            Self::Response(r) => r.revision,
            Self::Conversation(r) => r.revision,
        }
        .filter(|v| *v > 0)
        .ok_or_else(|| ClientError::InvalidResponse("Current resource revision is required".into()))
    }
    pub fn address(&self) -> Result<ResourceAddress> {
        Ok(ResourceAddress {
            mode: self.mode()?,
            owner: self.owner(),
            id: self.id().into(),
        })
    }
    pub fn is_conversation(&self) -> bool {
        matches!(self, Self::Conversation(_))
    }
    pub fn model(&self) -> &str {
        match self {
            Self::Response(r) => r.model.as_deref().unwrap_or("—"),
            Self::Conversation(r) => r.model.as_deref().unwrap_or("—"),
        }
    }
    pub fn status(&self) -> &str {
        match self {
            Self::Response(r) => &r.status,
            Self::Conversation(r) => {
                if r.deleted {
                    "deleted"
                } else if r.active_response_id.is_some() {
                    "active_response"
                } else {
                    "ready"
                }
            }
        }
    }
    pub fn created(&self) -> &str {
        match self {
            Self::Response(r) => &r.created_at,
            Self::Conversation(r) => &r.created_at,
        }
    }
    pub fn expires(&self) -> &str {
        match self {
            Self::Response(r) => &r.expires_at,
            Self::Conversation(r) => &r.expires_at,
        }
    }
    pub fn can_cancel(&self) -> bool {
        matches!(self,Self::Response(r) if !r.deleted && matches!(r.status.as_str(),"queued"|"in_progress"))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadKind {
    Detail,
    Items,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Inspection {
    pub row: Row,
    pub read: ReadKind,
}
#[derive(Debug, Clone, PartialEq)]
pub enum Mutation {
    Cancel(Row),
    Delete(Row),
    Metadata(Row),
    Append(Row),
    RemoveItem(Row, String),
}
impl Inspection {
    pub fn key(&self) -> String {
        serde_json::json!([
            self.row.state_key(),
            match self.read {
                ReadKind::Detail => "detail",
                ReadKind::Items => "items",
            }
        ])
        .to_string()
    }
}
impl Mutation {
    pub fn key(&self) -> String {
        let item = match self {
            Self::RemoveItem(_, id) => Some(id.as_str()),
            _ => None,
        };
        serde_json::json!([self.row().state_key(), self.label(), item]).to_string()
    }

    pub fn row(&self) -> &Row {
        match self {
            Self::Cancel(r)
            | Self::Delete(r)
            | Self::Metadata(r)
            | Self::Append(r)
            | Self::RemoveItem(r, _) => r,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Cancel(_) => "tenant_responses.cancel",
            Self::Delete(_) => "tenant_responses.delete",
            Self::Metadata(_) => "tenant_responses.metadata",
            Self::Append(_) => "tenant_responses.append",
            Self::RemoveItem(_, _) => "tenant_responses.remove_item",
        }
    }
    pub fn initial(&self) -> String {
        match self {
            Self::Metadata(Row::Conversation(r)) => {
                serde_json::to_string_pretty(&r.metadata).unwrap_or_else(|_| "{}".into())
            }
            Self::Append(_) => "[]".into(),
            _ => String::new(),
        }
    }
}
pub fn metadata(text: &str) -> Result<Value> {
    if text.len() > 128 * 1024 {
        return Err(ClientError::Config("Metadata is too large".into()));
    }
    let v: Value = serde_json::from_str(text)
        .map_err(|_| ClientError::Config("Metadata must be valid JSON".into()))?;
    let v = if v.is_null() {
        serde_json::json!({})
    } else {
        v
    };
    if v.as_object().is_none_or(|m| {
        m.len() > 16
            || m.iter().any(|(k, v)| {
                k.chars().count() > 64 || v.as_str().is_none_or(|v| v.chars().count() > 512)
            })
    }) {
        return Err(ClientError::Config(
            "Use at most 16 string metadata pairs (key64, value512 characters)".into(),
        ));
    }
    Ok(v)
}
pub fn items(text: &str) -> Result<Vec<Value>> {
    if text.len() > 2 * 1024 * 1024 {
        return Err(ClientError::Config("Items exceed two MiB".into()));
    }
    let values: Vec<Value> = serde_json::from_str(text)
        .map_err(|_| ClientError::Config("Use a JSON array of item objects".into()))?;
    if values.is_empty() || values.len() > 512 || values.iter().any(|v| !v.is_object()) {
        return Err(ClientError::Config("Use one to 512 item objects".into()));
    }
    Ok(values)
}
