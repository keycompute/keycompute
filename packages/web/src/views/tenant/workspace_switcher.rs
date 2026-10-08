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
use std::collections::BTreeSet;
use uuid::Uuid;

/// Shared registry for drafts and one-time values that would be destroyed by
/// workspace or route navigation. Independent owners prevent one component's
/// cleanup from clearing another component's protection.
#[derive(Clone, Copy)]
pub struct WorkspaceDraftState(pub Signal<BTreeSet<Uuid>>);

impl WorkspaceDraftState {
    pub fn is_blocked(self) -> bool {
        !self.0.peek().is_empty()
    }

    pub fn set(mut self, owner: Uuid, blocked: bool) {
        if self.0.peek().contains(&owner) == blocked {
            return;
        }
        if blocked {
            self.0.write().insert(owner);
        } else {
            self.0.write().remove(&owner);
        }
    }

    pub fn clear(mut self) {
        self.0.write().clear();
    }
}

/// Last route accepted by the global navigation guard. Keeping this outside
/// the router lets history navigation restore the page that still owns a
/// draft or one-time secret.
#[derive(Clone, Copy)]
pub struct WorkspaceRouteState(pub Signal<Option<Route>>);

impl WorkspaceRouteState {
    pub fn current(self) -> Option<Route> {
        self.0.peek().clone()
    }

    pub fn track(mut self, route: Route) {
        self.0.set(Some(route));
    }
}

/// Register a component whose whole lifetime contains an editable draft or a
/// one-time value. Dynamic pages should register only while actually dirty.
pub fn use_workspace_blocker() {
    use_workspace_dirty_blocker(|| true);
}

/// Register a component only while its local state would be meaningful to
/// lose. The owner token keeps overlapping dialogs and one-time values from
/// clearing each other's protection.
pub fn use_workspace_dirty_blocker(mut blocked: impl FnMut() -> bool + 'static) {
    let blockers = use_context::<WorkspaceDraftState>();
    let owner = use_hook(Uuid::new_v4);
    use_effect(move || blockers.set(owner, blocked()));
    use_drop(move || blockers.set(owner, false));
}

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

pub(crate) fn confirm_discard(message: &str) -> bool {
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

pub(crate) fn confirm_workspace_leave(drafts: WorkspaceDraftState, message: &str) -> bool {
    !drafts.is_blocked() || confirm_discard(message)
}

pub(crate) fn should_restore_route(blocked: bool, same_route: bool, confirmed: bool) -> bool {
    blocked && !same_route && !confirmed
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    static BEFORE_UNLOAD_BLOCKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static BEFORE_UNLOAD_INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static BEFORE_UNLOAD_HANDLER: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::Event)> =
        wasm_bindgen::closure::Closure::new(|event: web_sys::Event| {
            if BEFORE_UNLOAD_BLOCKED.with(std::cell::Cell::get) {
                event.prevent_default();
                let _ = js_sys::Reflect::set(
                    event.as_ref(),
                    &wasm_bindgen::JsValue::from_str("returnValue"),
                    &wasm_bindgen::JsValue::from_str(""),
                );
            }
        });
}

#[cfg(target_arch = "wasm32")]
fn sync_before_unload(blocked: bool) {
    use wasm_bindgen::JsCast;
    BEFORE_UNLOAD_BLOCKED.with(|value| value.set(blocked));
    BEFORE_UNLOAD_INSTALLED.with(|installed| {
        if installed.replace(true) {
            return;
        }
        BEFORE_UNLOAD_HANDLER.with(|handler| {
            if let Some(window) = web_sys::window() {
                let _ = window.add_event_listener_with_callback(
                    "beforeunload",
                    handler.as_ref().unchecked_ref(),
                );
            }
        });
    });
}

#[component]
pub fn WorkspaceUnloadGuard() -> Element {
    let drafts = use_context::<WorkspaceDraftState>();
    use_effect(move || {
        let blocked = !drafts.0.read().is_empty();
        #[cfg(target_arch = "wasm32")]
        sync_before_unload(blocked);
        #[cfg(not(target_arch = "wasm32"))]
        let _ = blocked;
    });
    rsx! {}
}

#[component]
pub fn WorkspaceSwitcher() -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut ui = use_context::<UiStore>();
    let drafts = use_context::<WorkspaceDraftState>();
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
                    if drafts.is_blocked()
                        && !confirm_discard(i18n.t("tenant.unsaved_switch_confirm"))
                    {
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
                                    drafts.clear();
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
    fn route_guard_only_restores_rejected_route_changes() {
        assert!(should_restore_route(true, false, false));
        assert!(!should_restore_route(true, false, true));
        assert!(!should_restore_route(true, true, false));
        assert!(!should_restore_route(false, false, false));
    }

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
