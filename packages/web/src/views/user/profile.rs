#![allow(clippy::clone_on_copy)]

use dioxus::prelude::*;
use ui::PageHeader;

use crate::hooks::use_i18n::use_i18n;
use crate::services::api_client::with_auto_refresh;
use crate::services::user_service;
use crate::stores::auth_store::AuthStore;
use crate::stores::user_store::{UserInfo, UserStore};

#[component]
pub fn UserProfile() -> Element {
    let i18n = use_i18n();
    let mut auth_store = use_context::<AuthStore>();
    let mut user_store = use_context::<UserStore>();

    let mut tenant_saving = use_signal(|| false);
    let mut tenant_error = use_signal(|| Option::<String>::None);
    let mut edit_mode = use_signal(|| false);
    let mut edit_name = use_signal(String::new);
    let mut saving = use_signal(|| false);
    let mut save_msg = use_signal(|| Option::<String>::None);
    let mut save_error = use_signal(|| Option::<String>::None);

    // 如果 UserStore 没有数据，主动获取
    let _user_data = use_resource(move || {
        let auth = auth_store.clone();
        async move {
            // 先检查是否已有数据
            if user_store.info.read().is_some() {
                return Ok(());
            }
            let observed = (auth.state)();
            let result = with_auto_refresh(auth, |token| async move {
                user_service::get_current_user(&token).await
            })
            .await;
            if !auth.matches(&observed) {
                return Err(client_api::ClientError::Other("登录状态已变更".into()));
            }
            let user = result?;
            *user_store.info.write() = Some(UserInfo {
                id: user.id.to_string(),
                email: user.email,
                name: user.name,
                platform_role: user.platform_role,
                status: user.status,
                memberships: user.memberships,
                selected_tenant: user.selected_tenant,
                capabilities: user.capabilities,
            });
            user_store.loaded_session_id.set(observed.session_id);
            Ok(())
        }
    });

    // 从 UserStore 读取当前用户
    let user_info = user_store.info.read();
    let display_name = user_info
        .as_ref()
        .map(|u| u.name.as_deref().unwrap_or("-").to_string())
        .unwrap_or_default();
    let email = user_info
        .as_ref()
        .map(|u| u.email.clone())
        .unwrap_or_default();
    let platform_role = user_info
        .as_ref()
        .and_then(|u| u.platform_role.map(|role| role.to_string()))
        .unwrap_or_default();
    let user_id = user_info.as_ref().map(|u| u.id.clone()).unwrap_or_default();
    let tenant_id = user_info
        .as_ref()
        .and_then(|u| u.active_tenant_id().map(str::to_owned))
        .unwrap_or_default();
    let memberships = user_info
        .as_ref()
        .map(|u| u.memberships.clone())
        .unwrap_or_default();
    let avatar = user_info.as_ref().map(|u| u.avatar_char()).unwrap_or('U');
    let has_user = user_info.is_some();
    drop(user_info);

    let on_edit_start = move |_| {
        let name = user_store
            .info
            .read()
            .as_ref()
            .and_then(|u| u.name.clone())
            .unwrap_or_default();
        edit_name.set(name);
        save_msg.set(None);
        save_error.set(None);
        edit_mode.set(true);
    };

    let on_save = move |evt: Event<FormData>| {
        evt.prevent_default();
        saving.set(true);
        save_error.set(None);
        let name_val = edit_name();
        let name_opt = if name_val.trim().is_empty() {
            None
        } else {
            Some(name_val)
        };
        spawn(async move {
            let observed = (auth_store.state)();
            let token = observed.access_token.clone().unwrap_or_default();
            let result = user_service::update_profile(name_opt.clone(), &token).await;
            if !auth_store.matches(&observed) {
                return;
            }
            match result {
                Ok(updated) => {
                    *user_store.info.write() = Some(UserInfo {
                        id: updated.id.to_string(),
                        email: updated.email,
                        name: updated.name,
                        platform_role: updated.platform_role,
                        status: updated.status,
                        memberships: updated.memberships,
                        selected_tenant: updated.selected_tenant,
                        capabilities: updated.capabilities,
                    });
                    save_msg.set(Some(i18n.t("profile.saved").to_string()));
                    edit_mode.set(false);
                    saving.set(false);
                }
                Err(e) => {
                    save_error.set(Some(format!("{}：{e}", i18n.t("profile.save_failed"))));
                    saving.set(false);
                }
            }
        });
    };

    let on_tenant_change = move |event: Event<FormData>| {
        if tenant_saving() {
            return;
        }
        let target = event.value();
        let observed = (auth_store.state)();
        let Some(current_user) = user_store.info.peek().clone().filter(|_| {
            observed.is_authenticated && *user_store.loaded_session_id.peek() == observed.session_id
        }) else {
            return;
        };
        if selection_matches_profile(&current_user, &target) {
            return;
        }
        let known = target.is_empty()
            || user_store.info.read().as_ref().is_some_and(|u| {
                u.memberships
                    .iter()
                    .any(|m| m.tenant_id == target && m.status.as_deref() == Some("active"))
            });
        if !known {
            tenant_error.set(Some("Select an active membership".into()));
            return;
        }
        tenant_saving.set(true);
        tenant_error.set(None);
        spawn(async move {
            let request = if target.is_empty() {
                client_api::api::auth::SelectTenantRequest::global()
            } else {
                client_api::api::auth::SelectTenantRequest::new(&target)
            };
            let result = client_api::AuthApi::new(&crate::services::api_client::get_client())
                .select_tenant(
                    &request,
                    observed.access_token.as_deref().unwrap_or_default(),
                )
                .await;
            if !auth_store.same_session(&observed) {
                return;
            }
            match result {
                Ok(session) => {
                    let target = (!target.is_empty()).then_some(target.as_str());
                    if !auth_store.select_session_if_current(
                        &observed,
                        &current_user.id,
                        target,
                        &session,
                    ) {
                        tenant_error.set(Some("The returned session does not match this identity and tenant selection".into()));
                        tenant_saving.set(false);
                        return;
                    }
                    // The new epoch invalidates old cached reads and remounts the profile.
                    user_store.clear();
                }
                Err(error) => tenant_error.set(Some(
                    crate::services::api_client::user_error_message(&error),
                )),
            }
            tenant_saving.set(false);
        });
    };

    rsx! {
        div {
            class: "page-container",
            PageHeader {
                title: i18n.t("page.profile").to_string(),
                description: i18n.t("profile.page_desc").to_string(),
            }

            if let Some(msg) = save_msg() {
                div { class: "alert alert-success", "{msg}" }
            }

            div { class: "card",
                label { class: "form-label", "Selected tenant" }
                select {
                    class: "input-field",
                    value: "{tenant_id}",
                    disabled: tenant_saving(),
                    onchange: on_tenant_change,
                    option { value: "", "Global session (no tenant selected)" }
                    for membership in memberships.iter().filter(|m| m.status.as_deref() == Some("active")) {
                        option { value: "{membership.tenant_id}",
                            "{membership.tenant_name.as_deref().unwrap_or(&membership.tenant_id)} ({membership.tenant_role})"
                        }
                    }
                }
                if let Some(error) = tenant_error() { p { class: "text-error", "{error}" } }
            }

            div {
                class: "card profile-card",
                if has_user {
                    div {
                        class: "profile-hero",
                        div {
                            class: "profile-avatar",
                            span { class: "avatar-char", "{avatar}" }
                        }
                        div {
                            class: "profile-hero-copy",
                            h2 { class: "profile-name", "{display_name}" }
                            p { class: "profile-email", "{email}" }
                            div {
                                class: "profile-badges",
                                span { class: "profile-badge", "{platform_role}" }
                                span { class: "profile-badge profile-badge-muted", "{i18n.t(\"profile.tenant\")} {tenant_id}" }
                            }
                        }
                    }

                    if edit_mode() {
                        // 编辑模式
                        form {
                            class: "profile-form",
                            onsubmit: on_save,
                            div {
                                class: "profile-info-grid",
                                div {
                                    class: "profile-field profile-field-editable",
                                    label { class: "form-label", {i18n.t("auth.name")} }
                                    input {
                                        class: "form-input",
                                        r#type: "text",
                                        value: "{edit_name}",
                                        oninput: move |e| edit_name.set(e.value()),
                                    }
                                }
                                div {
                                    class: "profile-field",
                                    label { class: "form-label", {i18n.t("auth.email")} }
                                    p { class: "form-value text-muted", "{email}" }
                                }
                                div {
                                    class: "profile-field",
                                    label { class: "form-label", {i18n.t("table.role")} }
                            p { class: "form-value", "{platform_role}" }
                                }
                                div {
                                    class: "profile-field",
                                    label { class: "form-label", {i18n.t("profile.user_id")} }
                                    p { class: "form-value profile-mono", "{user_id}" }
                                }
                            }
                            if let Some(err) = save_error() {
                                div { class: "alert alert-error", "{err}" }
                            }
                            div {
                                class: "form-actions profile-actions",
                                button {
                                    class: "btn btn-ghost",
                                    r#type: "button",
                                    onclick: move |_| edit_mode.set(false),
                                    {i18n.t("form.cancel")}
                                }
                                button {
                                    class: "btn btn-primary",
                                    r#type: "submit",
                                    disabled: saving(),
                                    if saving() { {i18n.t("form.saving")} } else { {i18n.t("form.save")} }
                                }
                            }
                        }
                    } else {
                        // 展示模式
                        div {
                            class: "profile-body",
                            div {
                                class: "profile-info-grid",
                                div {
                                    class: "profile-field",
                                    label { class: "form-label", {i18n.t("auth.name")} }
                                    p { class: "form-value", "{display_name}" }
                                }
                                div {
                                    class: "profile-field",
                                    label { class: "form-label", {i18n.t("auth.email")} }
                                    p { class: "form-value", "{email}" }
                                }
                                div {
                                    class: "profile-field",
                                    label { class: "form-label", {i18n.t("table.role")} }
                            p { class: "form-value", "{platform_role}" }
                                }
                                div {
                                    class: "profile-field",
                                    label { class: "form-label", {i18n.t("profile.user_id")} }
                                    p { class: "form-value profile-mono", "{user_id}" }
                                }
                            }
                            div {
                                class: "profile-actions",
                                button {
                                    class: "btn btn-secondary",
                                    onclick: on_edit_start,
                                    {i18n.t("profile.edit")}
                                }
                            }
                        }
                    }
                } else {
                    div { class: "empty-state", p { {i18n.t("table.loading")} } }
                }
            }
        }
    }
}

/// A restored opaque credential may not have a locally selected tenant yet.
/// The loaded server profile, not that local placeholder, describes the UI.
fn selection_matches_profile(user: &crate::stores::user_store::UserInfo, target: &str) -> bool {
    user.selected_tenant
        .as_ref()
        .map(|tenant| tenant.id.as_str())
        .unwrap_or_default()
        == target
}

#[cfg(test)]
mod workspace_selection_tests {
    use super::selection_matches_profile;
    use crate::stores::{auth_store::AuthState, user_store::UserInfo};
    use client_api::api::auth::SelectedTenant;

    #[test]
    fn restored_workspace_uses_the_loaded_profile_when_selecting_global() {
        let restored = AuthState::logged_in("restored-opaque-token".into());
        assert!(restored.selected_tenant_id.is_none());
        let tenant = uuid::Uuid::new_v4();
        let user = UserInfo {
            selected_tenant: Some(SelectedTenant {
                id: tenant.to_string(),
                name: None,
                slug: None,
                tenant_role: client_api::TenantRole::Member,
                authz_version: Some(1),
                membership_authz_version: Some(1),
            }),
            ..Default::default()
        };
        assert!(!selection_matches_profile(&user, ""));
        assert!(selection_matches_profile(&user, &tenant.to_string()));
        assert!(!selection_matches_profile(
            &user,
            &uuid::Uuid::new_v4().to_string()
        ));
        assert!(selection_matches_profile(&UserInfo::default(), ""));
    }
}
