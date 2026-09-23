use client_api::{
    ClientError, Result,
    api::node_control::{NodeInfo, NodeListQuery, RegistrationInfo, TaskInfo},
};
use uuid::Uuid;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Nodes,
    Tasks,
    Registrations,
}
impl Kind {
    pub const ALL: [Self; 3] = [Self::Nodes, Self::Tasks, Self::Registrations];
    pub fn label(self) -> &'static str {
        match self {
            Self::Nodes => "tenant_nodes.nodes",
            Self::Tasks => "tenant_nodes.tasks",
            Self::Registrations => "tenant_nodes.registrations",
        }
    }
    pub fn statuses(self) -> &'static [&'static str] {
        match self {
            Self::Nodes => &["online", "offline", "excluded"],
            Self::Tasks => &["queued", "leased", "succeeded", "failed", "expired"],
            Self::Registrations => &["pending", "approved", "rejected", "consumed"],
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Filter {
    pub owner: String,
    pub search: String,
    pub status: String,
    pub archived: bool,
    pub page: u32,
}
impl Filter {
    pub fn initial() -> Self {
        Self {
            page: 1,
            ..Default::default()
        }
    }
    pub fn query(&self, kind: Kind) -> Result<NodeListQuery> {
        if !(1..=1_000_000).contains(&self.page)
            || self.search.len() > 128
            || self.search.chars().any(char::is_control)
            || (!self.status.is_empty() && !kind.statuses().contains(&self.status.as_str()))
        {
            return Err(ClientError::Config("Invalid node query".into()));
        }
        let owner = if self.owner.trim().is_empty() {
            None
        } else {
            Some(
                Uuid::parse_str(self.owner.trim())
                    .ok()
                    .filter(|id| !id.is_nil())
                    .ok_or_else(|| ClientError::Config("A real owner UUID is required".into()))?,
            )
        };
        Ok(NodeListQuery {
            page: Some(self.page),
            page_size: Some(20),
            owner_user_id: owner,
            search: (!self.search.trim().is_empty()).then(|| self.search.trim().into()),
            status: (!self.status.is_empty()).then(|| self.status.clone()),
            archived: (kind == Kind::Tasks).then_some(self.archived),
        })
    }
}
#[derive(Clone, Debug)]
pub(super) enum Row {
    Node(NodeInfo),
    Task(TaskInfo),
    Registration(RegistrationInfo),
}
impl Row {
    pub fn id(&self) -> Uuid {
        match self {
            Self::Node(r) => r.id,
            Self::Task(r) => r.id,
            Self::Registration(r) => r.id,
        }
    }
    pub fn tenant(&self) -> Uuid {
        match self {
            Self::Node(r) => r.tenant_id,
            Self::Task(r) => r.tenant_id,
            Self::Registration(r) => r.tenant_id,
        }
    }
    pub fn owner(&self) -> Uuid {
        match self {
            Self::Node(r) => r.owner_user_id,
            Self::Task(r) => r.user_id,
            Self::Registration(r) => r.user_id,
        }
    }
    pub fn title(&self) -> &str {
        match self {
            Self::Node(r) => &r.display_name,
            Self::Task(r) => &r.model,
            Self::Registration(r) => &r.token_preview,
        }
    }
    pub fn status(&self) -> &str {
        match self {
            Self::Node(r) => &r.status,
            Self::Task(r) => &r.status,
            Self::Registration(r) => &r.status,
        }
    }
    pub fn version(&self) -> &str {
        match self {
            Self::Node(r) => &r.updated_at,
            Self::Task(r) => &r.updated_at,
            Self::Registration(r) => &r.updated_at,
        }
    }
    pub fn actions(&self) -> Vec<Action> {
        match self {
            Self::Node(r) => {
                let mut a = vec![Action::Configure, Action::Recover, Action::Revoke];
                if r.status != "excluded" {
                    a.push(Action::Exclude);
                }
                if r.status != "online" {
                    a.push(Action::Delete);
                }
                a
            }
            Self::Task(r) => {
                if r.archived_at.is_some() {
                    vec![]
                } else if matches!(r.status.as_str(), "succeeded" | "failed" | "expired") {
                    vec![Action::Archive]
                } else if matches!(r.status.as_str(), "queued" | "leased")
                    && r.cancellation_requested_at.is_none()
                {
                    vec![Action::Cancel]
                } else {
                    vec![]
                }
            }
            Self::Registration(r) => {
                if r.status == "pending" {
                    vec![Action::Approve, Action::Reject, Action::RevokeRegistration]
                } else if matches!(r.status.as_str(), "approved" | "consumed") {
                    vec![Action::RevokeRegistration]
                } else {
                    vec![]
                }
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Configure,
    Exclude,
    Recover,
    Revoke,
    Delete,
    Cancel,
    Archive,
    Approve,
    Reject,
    RevokeRegistration,
}
impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Configure => "tenant_nodes.configure",
            Self::Exclude => "tenant_nodes.exclude",
            Self::Recover => "tenant_nodes.recover",
            Self::Revoke => "tenant_nodes.revoke",
            Self::Delete => "tenant_nodes.delete",
            Self::Cancel => "tenant_nodes.cancel",
            Self::Archive => "tenant_nodes.archive",
            Self::Approve => "tenant_nodes.approve",
            Self::Reject => "tenant_nodes.reject",
            Self::RevokeRegistration => "tenant_nodes.revoke_registration",
        }
    }
}
#[derive(Clone, Debug)]
pub(super) struct Pending {
    pub row: Row,
    pub action: Action,
}
impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        self.row.id() == other.row.id()
            && self.row.tenant() == other.row.tenant()
            && self.row.owner() == other.row.owner()
            && self.row.version() == other.row.version()
            && self.action == other.action
    }
}
