//! Tenant node control uses only current selected tenant administration.
mod command;
mod table;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
mod types;

use super::common::{WorkspaceLinks, WorkspaceScope};
use crate::{
    hooks::use_i18n::use_i18n,
    stores::{auth_store::AuthStore, user_store::UserStore},
};
use dioxus::prelude::*;
use types::Kind;

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
            for identity in [format!("{scope:?}")] {NodeWorkspace {key:"{identity}",scope}}
        }else{p {role:"alert",{i18n.t("tenant.admin_required")}}}
    }
}
#[component]
fn NodeWorkspace(scope: WorkspaceScope) -> Element {
    let i18n = use_i18n();
    let mut kind = use_signal(|| Kind::Nodes);
    rsx! {div {class:"page-container tenant-node-admin",
        ui::PageHeader {title:i18n.t("tenant_nodes.title").to_string(),description:i18n.t("tenant_nodes.hint").to_string()}
        WorkspaceLinks {}
        p {class:"alert alert-info",{i18n.t("tenant_nodes.scope")} " {scope.tenant_id}"}
        nav {class:"toolbar",aria_label:i18n.t("tenant_nodes.title"),
            for selected in Kind::ALL {button {class:"btn btn-secondary",aria_pressed:kind()==selected,onclick:move |_|kind.set(selected),{i18n.t(selected.label())}}}
        }
        for key in [format!("{:?}",kind())] {table::ResourceTable {key:"{key}",scope,kind:kind()}}
    }}
}
