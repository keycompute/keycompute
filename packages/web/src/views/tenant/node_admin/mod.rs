//! Tenant node control uses only current selected tenant administration.
mod command;
mod table;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
mod types;

use super::common::{WorkspaceContext, WorkspaceLinks, WorkspaceScope};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, with_auto_refresh},
    stores::{auth_store::AuthStore, user_store::UserStore},
};
use client_api::{ClientError, Result, api::node_control::NodeControlApi};
use dioxus::prelude::*;
use types::Kind;
use uuid::Uuid;

/// Stable identity for node administration. Tenant pages follow the verified
/// workspace session; platform pages must carry an explicit tenant target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeAdminScope {
    Tenant(WorkspaceScope),
    Platform {
        session_id: Uuid,
        user_id: Uuid,
        tenant_id: Uuid,
    },
}

impl NodeAdminScope {
    pub(crate) fn platform(auth: AuthStore, users: UserStore, tenant_id: Uuid) -> Option<Self> {
        let state = auth.state.peek();
        let user = users.info.peek();
        let user = user.as_ref()?;
        let user_id = Uuid::parse_str(&user.id).ok().filter(|id| !id.is_nil())?;
        if !state.is_authenticated
            || *users.loaded_session_id.peek() != state.session_id
            || tenant_id.is_nil()
            || !user.can_manage_platform()
        {
            return None;
        }
        Some(Self::Platform {
            session_id: state.session_id,
            user_id,
            tenant_id,
        })
    }

    pub(crate) fn tenant_id(self) -> Uuid {
        match self {
            Self::Tenant(scope) => scope.tenant_id,
            Self::Platform { tenant_id, .. } => tenant_id,
        }
    }

    fn is_current(self, auth: AuthStore, users: UserStore) -> bool {
        match self {
            Self::Tenant(scope) => scope.is_current(auth, users),
            Self::Platform {
                session_id,
                user_id,
                tenant_id,
            } => {
                let state = auth.state.peek();
                let user = users.info.peek();
                state.is_authenticated
                    && state.session_id == session_id
                    && *users.loaded_session_id.peek() == session_id
                    && !tenant_id.is_nil()
                    && user.as_ref().is_some_and(|value| {
                        value.can_manage_platform()
                            && Uuid::parse_str(&value.id).ok() == Some(user_id)
                    })
            }
        }
    }

    fn api(self) -> Result<NodeControlApi> {
        match self {
            Self::Tenant(scope) => NodeControlApi::tenant(&get_client(), scope.tenant_id),
            Self::Platform { tenant_id, .. } => {
                NodeControlApi::platform_tenant(&get_client(), tenant_id)
            }
        }
    }

    async fn read<T, F, Fut>(self, auth: AuthStore, users: UserStore, request: F) -> Result<T>
    where
        F: Fn(String, NodeControlApi) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        if !self.is_current(auth, users) {
            return Err(scope_changed());
        }
        let api = self.api()?;
        let result = with_auto_refresh(auth, move |token| request(token, api.clone())).await;
        if !self.is_current(auth, users) {
            return Err(scope_changed());
        }
        result
    }

    async fn command<T, F, Fut>(self, auth: AuthStore, users: UserStore, request: F) -> Result<T>
    where
        F: FnOnce(String, NodeControlApi) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        if !self.is_current(auth, users) {
            return Err(scope_changed());
        }
        let token = auth
            .state
            .peek()
            .access_token
            .clone()
            .ok_or_else(scope_changed)?;
        let result = request(token, self.api()?).await;
        if !self.is_current(auth, users) {
            return Err(scope_changed());
        }
        result
    }
}

fn scope_changed() -> ClientError {
    ClientError::Other(
        "The selected node administration scope changed; refresh and try again".into(),
    )
}

#[component]
pub fn TenantNodes() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let scope = WorkspaceScope::from_stores(auth, users);
    let allowed = users
        .info
        .read()
        .as_ref()
        .is_some_and(|u| u.can_manage_tenant());
    rsx! {
        if let Some(scope)=scope.filter(|_|allowed) {
            for identity in [format!("{scope:?}")] {
                TenantNodeWorkspace { key:"{identity}", scope: NodeAdminScope::Tenant(scope) }
            }
        }else{p {role:"alert",{i18n.t("tenant.admin_required")}}}
    }
}
#[component]
fn TenantNodeWorkspace(scope: NodeAdminScope) -> Element {
    let i18n = use_i18n();
    rsx! {div {class:"page-container tenant-node-admin",
        ui::PageHeader {title:i18n.t("tenant_nodes.title").to_string(),description:i18n.t("tenant_nodes.hint").to_string()}
        WorkspaceLinks {}
        WorkspaceContext {}
        NodeResourceConsole { scope }
    }}
}

/// Shared, fully scoped node resource console used by tenant and platform
/// administration. The parent owns target selection and explanatory copy.
#[component]
pub(crate) fn NodeResourceConsole(scope: NodeAdminScope) -> Element {
    let i18n = use_i18n();
    let mut kind = use_signal(|| Kind::Nodes);
    rsx! {div {class:"node-resource-console",
        nav {class:"toolbar",aria_label:i18n.t("tenant_nodes.title"),
            for selected in Kind::ALL {button {class:"btn btn-secondary",aria_pressed:kind()==selected,onclick:move |_|kind.set(selected),{i18n.t(selected.label())}}}
        }
        for key in [format!("{:?}",kind())] {table::ResourceTable {key:"{key}",scope,kind:kind()}}
    }}
}
