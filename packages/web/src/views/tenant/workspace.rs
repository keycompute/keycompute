use super::common::{self, WorkspaceScope};
use crate::{
    hooks::use_i18n::use_i18n,
    router::Route,
    services::api_client::user_error_message,
    stores::{
        auth_store::{AuthState, AuthStore},
        user_store::{UserInfo, UserStore},
    },
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::api::{auth::AuthResponse, tenant_control::TenantContext};
use dioxus::prelude::*;

pub(super) fn install_selection(
    mut auth: AuthStore,
    mut users: UserStore,
    observed: &AuthState,
    user: &str,
    target: Option<&str>,
    response: AuthResponse,
) -> bool {
    if !auth.select_session_if_current(observed, user, target, &response) {
        return false;
    }
    users.info.set(Some(UserInfo {
        id: response.user_id,
        email: response.email,
        name: response.name,
        platform_role: response.platform_role,
        status: response.status,
        memberships: response.memberships,
        selected_tenant: response.selected_tenant,
        capabilities: response.capabilities,
    }));
    users.loaded_session_id.set(auth.state.peek().session_id);
    users.load_failed.set(false);
    true
}

fn has_active_membership(user: Option<&UserInfo>) -> bool {
    user.is_some_and(|value| {
        value
            .memberships
            .iter()
            .any(|membership| membership.status.as_deref() == Some("active"))
    })
}

#[component]
pub fn TenantWorkspace() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let epoch = (auth.state)().session_id;
    let scope = WorkspaceScope::from_stores(auth, users);
    let key = format!("{epoch}:{scope:?}");
    rsx! { for identity in [key] { TenantWorkspacePage { key: "{identity}" } } }
}

#[component]
fn TenantWorkspacePage() -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let scope = WorkspaceScope::from_stores(auth, users);
    let user = (users.info)();
    let can_manage_settings = user.as_ref().is_some_and(UserInfo::can_manage_tenant);
    let can_manage_members = user.as_ref().is_some_and(UserInfo::can_manage_members);
    let can_invite_members = user.as_ref().is_some_and(UserInfo::can_invite_members);
    let can_manage_providers = user.as_ref().is_some_and(UserInfo::can_manage_providers);
    let can_manage_keys = user.as_ref().is_some_and(UserInfo::can_manage_api_keys);
    let can_manage_pricing = user.as_ref().is_some_and(UserInfo::can_manage_pricing);
    let can_manage_billing = user.as_ref().is_some_and(UserInfo::can_manage_billing);
    let context = use_resource(move || {
        let scope = WorkspaceScope::from_stores(auth, users);
        async move {
            let result = if let Some(scope) = scope {
                common::read(auth, users, scope, move |token| async move {
                    scope.api()?.context(&token).await
                })
                .await
                .map(Some)
            } else {
                Ok(None)
            };
            KeyedResourceValue::new(scope, result)
        }
    });
    let loaded = current_keyed_value(&scope, context.state().cloned(), context());
    let current: Option<TenantContext> = loaded
        .as_ref()
        .and_then(|value| value.as_ref().ok())
        .and_then(|value| value.as_ref())
        .cloned();
    let role_label = current
        .as_ref()
        .map(|value| {
            if user
                .as_ref()
                .is_some_and(|user| user.id == value.owner_user_id.to_string())
            {
                i18n.t("tenant.role_owner")
            } else if value.tenant_role == client_api::TenantRole::Admin {
                i18n.t("tenant.role_admin")
            } else {
                i18n.t("tenant.role_member")
            }
        })
        .unwrap_or_default()
        .to_string();

    rsx! {
        div { class: "page-container tenant-workspace tenant-overview",
            ui::PageHeader {
                title: i18n.t("tenant.workspace").to_string(),
                description: i18n.t("tenant.workspace_hint").to_string()
            }

            if scope.is_none() {
                section { class: "section tenant-empty-state", aria_label: i18n.t("tenant.no_workspace_title"),
                    div { class: "section-body",
                        h2 { class: "section-body-title", {i18n.t("tenant.no_workspace_title")} }
                        if !has_active_membership(user.as_ref()) {
                            p { class: "text-secondary", {i18n.t("tenant.no_memberships")} }
                            div { class: "tenant-empty-actions",
                                if user.as_ref().is_some_and(UserInfo::can_manage_platform) {
                                    Link { class: "btn btn-primary", to: Route::Tenants {}, {i18n.t("tenant.open_platform_tenants")} }
                                } else if user.as_ref().is_some_and(UserInfo::can_view_operations) {
                                    Link { class: "btn btn-primary", to: Route::PlatformOperations {}, {i18n.t("tenant.open_operations")} }
                                }
                                Link { class: "btn btn-secondary", to: Route::UserProfile {}, {i18n.t("tenant.check_account")} }
                            }
                        } else {
                            p { class: "text-secondary", {i18n.t("tenant.choose_from_header")} }
                        }
                    }
                }
            } else if let Some(value) = current {
                section { class: "section tenant-overview-hero",
                    div { class: "section-body",
                        div { class: "tenant-overview-heading",
                            div {
                                p { class: "tenant-overview-eyebrow", {i18n.t("tenant.current")} }
                                h2 { "{value.name}" }
                                if let Some(description) = value.description.as_deref().filter(|value| !value.is_empty()) {
                                    p { class: "text-secondary", "{description}" }
                                }
                            }
                            span { class: "badge", "{role_label}" }
                        }
                        dl { class: "tenant-overview-facts",
                            div { dt { {i18n.t("tenant.slug")} } dd { "{value.slug}" } }
                            div { dt { {i18n.t("tenant.membership_role")} } dd { "{role_label}" } }
                            div { dt { "RPM" } dd { "{value.default_rpm_limit}" } }
                            div { dt { "TPM" } dd { "{value.default_tpm_limit}" } }
                        }
                    }
                }

                h2 { class: "tenant-overview-section-title", {i18n.t("tenant.my_workspace_actions")} }
                div { class: "tenant-overview-grid",
                    OverviewCard { title: i18n.t("page.home").to_string(), description: i18n.t("tenant.dashboard_card").to_string(), route: Route::Dashboard {} }
                    OverviewCard { title: i18n.t("nav.api_keys").to_string(), description: i18n.t("tenant.keys_card").to_string(), route: Route::ApiKeyList {} }
                    OverviewCard { title: i18n.t("nav.usage").to_string(), description: i18n.t("tenant.usage_card").to_string(), route: Route::Usage {} }
                    OverviewCard { title: i18n.t("nav.payments").to_string(), description: i18n.t("tenant.payments_card").to_string(), route: Route::PaymentsOverview {} }
                }

                if can_manage_settings
                    || can_manage_members
                    || can_invite_members
                    || can_manage_providers
                    || can_manage_keys
                    || can_manage_pricing
                    || can_manage_billing
                {
                    h2 { class: "tenant-overview-section-title", {i18n.t("tenant.admin_get_started")} }
                    div { class: "tenant-overview-grid",
                        if can_manage_settings {
                            OverviewCard { title: i18n.t("tenant.settings").to_string(), description: i18n.t("tenant.settings_card").to_string(), route: Route::TenantSettings {} }
                        }
                        if can_manage_members {
                            OverviewCard { title: i18n.t("tenant.members").to_string(), description: i18n.t("tenant.members_card").to_string(), route: Route::TenantMembers {} }
                        }
                        if can_invite_members {
                            OverviewCard { title: i18n.t("tenant.invitations").to_string(), description: i18n.t("tenant.invitations_hint").to_string(), route: Route::TenantInvitations {} }
                        }
                        if can_manage_providers {
                            OverviewCard { title: i18n.t("tenant_providers.title").to_string(), description: i18n.t("tenant.providers_card").to_string(), route: Route::TenantProviders {} }
                        }
                        if can_manage_keys {
                            OverviewCard { title: i18n.t("tenant_keys.title").to_string(), description: i18n.t("tenant_keys.hint").to_string(), route: Route::TenantKeys {} }
                        }
                        if can_manage_settings {
                            OverviewCard { title: i18n.t("tenant_responses.title").to_string(), description: i18n.t("tenant_responses.hint").to_string(), route: Route::TenantResponses {} }
                            OverviewCard { title: i18n.t("tenant_nodes.title").to_string(), description: i18n.t("tenant_nodes.hint").to_string(), route: Route::TenantNodes {} }
                        }
                        if can_manage_pricing {
                            OverviewCard { title: i18n.t("tenant_pricing.title").to_string(), description: i18n.t("tenant_pricing.hint").to_string(), route: Route::TenantPricing {} }
                        }
                        if can_manage_billing {
                            OverviewCard { title: i18n.t("tenant_finance.title").to_string(), description: i18n.t("tenant.finance_card").to_string(), route: Route::TenantFinance {} }
                            OverviewCard { title: i18n.t("tenant_financial_controls.title").to_string(), description: i18n.t("tenant_financial_controls.hint").to_string(), route: Route::TenantFinancialControls {} }
                        }
                        if can_manage_settings {
                            OverviewCard { title: i18n.t("tenant_distribution.title").to_string(), description: i18n.t("tenant_distribution.hint").to_string(), route: Route::TenantDistribution {} }
                            OverviewCard { title: i18n.t("tenant.audit").to_string(), description: i18n.t("tenant.audit_hint").to_string(), route: Route::TenantAudit {} }
                        }
                    }
                }
            } else if let Some(Err(error)) = loaded {
                div { class: "alert alert-error", role: "alert", {user_error_message(i18n, &error)} }
            } else {
                p { role: "status", {i18n.t("common.loading")} }
            }
        }
    }
}

#[component]
fn OverviewCard(title: String, description: String, route: Route) -> Element {
    rsx! {
        Link { class: "tenant-overview-card", to: route,
            h3 { "{title}" }
            p { "{description}" }
            span { aria_hidden: "true", "→" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_identity_without_memberships_is_explicitly_detected() {
        let user = UserInfo::default();
        assert!(!has_active_membership(Some(&user)));
    }

    #[test]
    fn suspended_memberships_do_not_count_as_available_workspaces() {
        let mut user = UserInfo::default();
        user.memberships
            .push(client_api::api::auth::TenantMembership {
                tenant_id: "tenant-1".into(),
                tenant_name: Some("Tenant".into()),
                tenant_role: client_api::TenantRole::Member,
                status: Some("suspended".into()),
                invited_by: None,
                joined_at: None,
                removed_at: None,
                authz_version: Some(1),
            });
        assert!(!has_active_membership(Some(&user)));
    }
}
