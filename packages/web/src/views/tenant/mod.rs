//! Selected-workspace controls. Backend authorization remains authoritative.
mod audit;
pub(crate) mod common;
pub(crate) mod invitation_entry;
mod invitations;
mod members;
pub(crate) mod node_admin;
pub use node_admin::TenantNodes;
mod settings;
mod workspace;
mod workspace_switcher;
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
pub use settings::TenantSettings;
pub use workspace::TenantWorkspace;
pub use workspace_switcher::{WorkspaceDraftState, WorkspaceSwitcher};

fn can_access_admin_route(user: &crate::stores::user_store::UserInfo, route: &Route) -> bool {
    match route {
        Route::TenantSettings {}
        | Route::TenantResponses {}
        | Route::TenantNodes {}
        | Route::TenantDistribution {}
        | Route::TenantAudit {} => user.can_manage_tenant(),
        Route::TenantMembers {} => user.can_manage_members(),
        Route::TenantInvitations {} => user.can_invite_members(),
        Route::TenantProviders {} => user.can_manage_providers(),
        Route::TenantKeys {} => user.can_manage_api_keys(),
        Route::TenantPricing {} => user.can_manage_pricing(),
        Route::TenantFinance {} | Route::TenantFinancialControls {} => user.can_manage_billing(),
        _ => false,
    }
}

#[component]
pub fn TenantAdminLayout() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let route = use_route::<Route>();
    let scope = common::WorkspaceScope::from_stores(auth, users);
    if scope.is_none() {
        return rsx! { div { class:"page-container", role:"alert",
            p { class: "alert alert-info", {i18n.t("tenant.selection_required")} }
            Link { to:Route::TenantWorkspace {}, {i18n.t("tenant.workspace")} }
        }};
    }
    if !users
        .info
        .read()
        .as_ref()
        .is_some_and(|user| can_access_admin_route(user, &route))
    {
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

mod distribution_admin;
pub use distribution_admin::TenantDistribution;

mod finance_control;
pub use finance_control::TenantFinancialControls;

#[cfg(test)]
mod capability_route_tests {
    use super::*;
    use client_api::api::auth::{SelectedTenant, SessionCapabilities};

    fn user(capability: &str) -> crate::stores::user_store::UserInfo {
        crate::stores::user_store::UserInfo {
            selected_tenant: Some(SelectedTenant {
                id: uuid::Uuid::new_v4().to_string(),
                name: Some("Workspace".into()),
                slug: Some("workspace".into()),
                tenant_role: client_api::TenantRole::Admin,
                authz_version: Some(1),
                membership_authz_version: Some(1),
            }),
            capabilities: SessionCapabilities {
                tenant: vec![capability.into()],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn routes_require_their_specific_presentation_capability() {
        assert!(can_access_admin_route(
            &user("providers:manage"),
            &Route::TenantProviders {}
        ));
        assert!(!can_access_admin_route(
            &user("providers:manage"),
            &Route::TenantFinance {}
        ));
        assert!(can_access_admin_route(
            &user("billing:manage"),
            &Route::TenantFinancialControls {}
        ));
        assert!(can_access_admin_route(
            &user("members:manage"),
            &Route::TenantMembers {}
        ));
    }
}
