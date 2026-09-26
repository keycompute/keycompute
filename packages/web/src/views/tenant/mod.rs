//! Selected-workspace controls. Backend authorization remains authoritative.
mod audit;
pub(crate) mod common;
pub(crate) mod invitation_entry;
mod invitations;
mod members;
mod node_admin;
pub use node_admin::TenantNodes;
mod workspace;
use crate::{
    hooks::use_i18n::use_i18n,
    router::Route,
    stores::{auth_store::AuthStore, user_store::UserStore},
};
pub use audit::TenantAudit;
use dioxus::prelude::*;
pub use invitation_entry::TenantInvitationAccept;
pub use invitations::TenantInvitations;
pub use members::TenantMembers;
pub use workspace::TenantWorkspace;

#[component]
pub fn TenantAdminLayout() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let scope = common::WorkspaceScope::from_stores(auth, users);
    let allowed = scope.is_some()
        && users
            .info
            .read()
            .as_ref()
            .is_some_and(|u| u.can_manage_tenant());
    if !allowed {
        return rsx! { div { class:"page-container", role:"alert",
            p { {i18n.t("tenant.admin_required")} }
            Link { to:Route::TenantWorkspace {}, {i18n.t("tenant.workspace")} }
        }};
    }
    rsx! { Outlet::<Route> {} }
}

mod pricing_admin;
pub use pricing_admin::TenantPricing;

mod response_admin;
pub use response_admin::TenantResponses;

mod key_admin;
pub use key_admin::{OwnerKeyIssuance, TenantKeys};

mod finance;
pub use finance::TenantFinance;

mod provider_admin;
pub use provider_admin::TenantProviders;
