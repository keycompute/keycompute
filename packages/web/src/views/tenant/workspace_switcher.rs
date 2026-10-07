use super::workspace::install_selection;
use crate::{
    hooks::use_i18n::use_i18n,
    router::Route,
    services::api_client::{get_client, user_error_message},
    stores::{
        auth_store::AuthStore,
        ui_store::UiStore,
        user_store::{UserInfo, UserStore},
    },
};
use client_api::{AuthApi, api::auth::SelectTenantRequest};
use dioxus::prelude::*;

/// Shared dirty-state fence for workspace settings. A context switch destroys
/// page-local drafts, so the switcher must obtain explicit confirmation first.
#[derive(Clone, Copy)]
pub struct WorkspaceDraftState(pub Signal<bool>);

pub fn landing_route(user: &UserInfo) -> Route {
    if user.selected_tenant.is_some() {
        Route::Dashboard {}
    } else if user.can_manage_platform() {
        Route::Tenants {}
    } else if user.can_view_operations() {
        Route::PlatformOperations {}
    } else {
        Route::TenantWorkspace {}
    }
}

fn confirm_discard(message: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|window| window.confirm_with_message(message).ok())
            .unwrap_or(false)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = message;
        true
    }
}

#[component]
pub fn WorkspaceSwitcher() -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut ui = use_context::<UiStore>();
    let mut drafts = use_context::<WorkspaceDraftState>();
    let nav = use_navigator();
    let user = (users.info)();
    let current = user
        .as_ref()
        .and_then(UserInfo::active_tenant_id)
        .unwrap_or_default()
        .to_owned();
    let mut target = use_signal(|| current.clone());
    let mut busy = use_signal(|| false);

    let Some(user) = user else {
        return rsx! {};
    };
    let active_memberships = user
        .memberships
        .iter()
        .filter(|membership| membership.status.as_deref() == Some("active"))
        .cloned()
        .collect::<Vec<_>>();
    let global_label = if user.can_manage_platform() || user.can_view_operations() {
        i18n.t("tenant.platform_mode")
    } else {
        i18n.t("tenant.personal_mode")
    };
    let current_role = user
        .selected_tenant
        .as_ref()
        .map(|tenant| match tenant.tenant_role {
            client_api::TenantRole::Admin => i18n.t("tenant.role_admin"),
            client_api::TenantRole::Member => i18n.t("tenant.role_member"),
        });

    rsx! {
        div { class: "header-workspace-switcher",
            label { class: "sr-only", r#for: "header-workspace-select", {i18n.t("tenant.switch")} }
            select {
                id: "header-workspace-select",
                class: "header-workspace-select",
                value: "{target}",
                disabled: busy(),
                onchange: move |event| {
                    let value = event.value();
                    target.set(value.clone());
                    if busy() || value == current {
                        target.set(current.clone());
                        return;
                    }
                    if drafts.0() && !confirm_discard(i18n.t("tenant.unsaved_switch_confirm")) {
                        target.set(current.clone());
                        return;
                    }
                    let observed = auth.state.peek().clone();
                    let Some(user) = users
                        .info
                        .peek()
                        .clone()
                        .filter(|_| *users.loaded_session_id.peek() == observed.session_id)
                    else {
                        target.set(current.clone());
                        return;
                    };
                    let selected = (!value.is_empty()).then_some(value);
                    let reset_value = current.clone();
                    busy.set(true);
                    spawn(async move {
                        let request = SelectTenantRequest {
                            tenant_id: selected.clone(),
                        };
                        let result = AuthApi::new(&get_client())
                            .select_tenant(
                                &request,
                                observed.access_token.as_deref().unwrap_or_default(),
                            )
                            .await;
                        if !auth.same_session(&observed) {
                            return;
                        }
                        busy.set(false);
                        match result {
                            Ok(response) => {
                                if install_selection(
                                    auth,
                                    users,
                                    &observed,
                                    &user.id,
                                    selected.as_deref(),
                                    response,
                                ) {
                                    drafts.0.set(false);
                                    let next = users
                                        .info
                                        .peek()
                                        .as_ref()
                                        .map(landing_route)
                                        .unwrap_or(Route::TenantWorkspace {});
                                    nav.replace(next);
                                } else {
                                    target.set(reset_value.clone());
                                    ui.show_error(i18n.t("tenant.session_changed"));
                                }
                            }
                            Err(error) => {
                                target.set(reset_value.clone());
                                ui.show_error(user_error_message(i18n, &error));
                            }
                        }
                    });
                },
                option { value: "", "{global_label}" }
                for membership in active_memberships.iter() {
                    option {
                        value: "{membership.tenant_id}",
                        "{membership.tenant_name.as_deref().unwrap_or(&membership.tenant_id)}"
                    }
                }
            }
            if busy() {
                span { class: "header-workspace-status", role: "status", {i18n.t("tenant.switching")} }
            } else if let Some(role) = current_role {
                span { class: "header-workspace-role", "{role}" }
            } else if active_memberships.is_empty()
                && !user.can_manage_platform()
                && !user.can_view_operations()
            {
                span { class: "header-workspace-status", {i18n.t("tenant.no_workspace_short")} }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use client_api::{PlatformRole, api::auth::SessionCapabilities};

    #[test]
    fn landing_is_capability_and_workspace_aware() {
        let ordinary = UserInfo::default();
        assert_eq!(landing_route(&ordinary), Route::TenantWorkspace {});

        let operator = UserInfo {
            platform_role: Some(PlatformRole::Operator),
            capabilities: SessionCapabilities {
                platform: vec!["platform:tenant_health".into()],
                tenant: vec![],
            },
            ..Default::default()
        };
        assert_eq!(landing_route(&operator), Route::PlatformOperations {});

        let root = UserInfo {
            platform_role: Some(PlatformRole::Root),
            capabilities: SessionCapabilities {
                platform: vec!["users:manage".into()],
                tenant: vec![],
            },
            ..Default::default()
        };
        assert_eq!(landing_route(&root), Route::Tenants {});
    }
}
