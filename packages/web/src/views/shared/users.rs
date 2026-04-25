use client_api::{
    AdminApi, AssignableUserRole, UserRole,
    api::admin::{UpdateUserRequest, UserDetail, UserListResponse, UserQueryParams},
};
use dioxus::prelude::*;
use ui::{Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Pagination, Table, TableHead};

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::api_client::{get_client, with_auto_refresh};
use crate::stores::auth_store::AuthStore;
use crate::stores::ui_store::UiStore;
use crate::stores::user_store::UserStore;
use crate::utils::time::format_time;

const PAGE_SIZE: usize = 20;

#[component]
pub fn Users() -> Element {
    let user_store = use_context::<UserStore>();
    let is_admin = user_store
        .info
        .read()
        .as_ref()
        .map(|u| u.is_admin())
        .unwrap_or(false);

    if is_admin {
        rsx! { AdminUsersView {} }
    } else {
        rsx! { UserSelfView {} }
    }
}

// ── Admin 视图 ────────────────────────────────────────────────────────

#[component]
fn AdminUsersView() -> Element {
    let i18n = use_i18n();
    let user_store = use_context::<UserStore>();
    let auth_store = use_context::<AuthStore>();
    let mut ui_store = use_context::<UiStore>();
    let mut search = use_signal(String::new);
    let mut page = use_signal(|| 1u32);
    let current_user = user_store.info.read().clone();
    let can_current_user_manage_roles = current_user
        .as_ref()
        .map(|u| u.role == UserRole::System.as_str())
        .unwrap_or(false);
    let current_user_id = current_user
        .as_ref()
        .map(|u| u.id.clone())
        .unwrap_or_default();
    let current_user_id_for_edit = current_user_id.clone();
    let can_current_user_manage_roles_for_edit = can_current_user_manage_roles;
    let current_user_id_for_delete = current_user_id.clone();
    let can_current_user_delete_admins = can_current_user_manage_roles;

    // 编辑弹窗状态
    let mut edit_user = use_signal(|| Option::<UserDetail>::None);
    let mut edit_name = use_signal(String::new);
    let mut edit_role = use_signal(String::new);
    let mut edit_saving = use_signal(|| false);

    // 删除确认状态
    let mut delete_user = use_signal(|| Option::<UserDetail>::None);
    let mut delete_saving = use_signal(|| false);

    let mut users = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            let client = get_client();
            let params = UserQueryParams::new().with_page_size(200);
            AdminApi::new(&client)
                .list_all_users(Some(&params), &token)
                .await
                .map(|resp: UserListResponse| resp.users)
        })
        .await
    });

    let filtered_users = move || -> Vec<UserDetail> {
        let q = search().to_lowercase();
        match users() {
            Some(Ok(ref list)) => list
                .iter()
                .filter(|u| {
                    q.is_empty()
                        || u.email.to_lowercase().contains(&q)
                        || u.name.as_deref().unwrap_or("").to_lowercase().contains(&q)
                })
                .cloned()
                .collect::<Vec<UserDetail>>(),
            _ => vec![],
        }
    };

    let total_pages = move || {
        let len = filtered_users().len();
        len.div_ceil(PAGE_SIZE).max(1) as u32
    };

    let paged_users = move || {
        let p = page() as usize;
        let all = filtered_users();
        let start = (p - 1) * PAGE_SIZE;
        all.into_iter()
            .skip(start)
            .take(PAGE_SIZE)
            .collect::<Vec<_>>()
    };

    // 提交编辑
    let on_edit_save = move |_| {
        let Some(u) = edit_user() else { return };
        let name_val = edit_name();
        let role_val = edit_role();
        let can_edit_role = can_current_user_manage_roles_for_edit
            && u.id != current_user_id_for_edit
            && u.role != "system";
        let role = if !can_edit_role || role_val.trim().is_empty() {
            None
        } else {
            match role_val.parse::<AssignableUserRole>() {
                Ok(role) => Some(role),
                Err(err) => {
                    ui_store.show_error(err);
                    return;
                }
            }
        };
        let id = u.id.clone();
        edit_saving.set(true);
        spawn(async move {
            let token = auth_store.token().unwrap_or_default();
            let client = get_client();
            let req = UpdateUserRequest {
                name: if name_val.trim().is_empty() {
                    None
                } else {
                    Some(name_val)
                },
                role,
            };
            match AdminApi::new(&client).update_user(&id, &req, &token).await {
                Ok(_) => {
                    ui_store.show_success(i18n.t("users.updated"));
                    edit_user.set(None);
                    users.restart();
                }
                Err(e) => {
                    ui_store.show_error(format!("{}: {e}", i18n.t("users.update_failed")));
                }
            }
            edit_saving.set(false);
        });
    };

    // 确认删除
    let on_delete_confirm = move |_| {
        let Some(u) = delete_user() else { return };
        if u.id == current_user_id_for_delete
            || u.role == UserRole::System.as_str()
            || (u.role == UserRole::Admin.as_str() && !can_current_user_delete_admins)
        {
            ui_store.show_error(i18n.t("users.delete_failed"));
            delete_user.set(None);
            return;
        }
        let id = u.id.clone();
        delete_saving.set(true);
        spawn(async move {
            let token = auth_store.token().unwrap_or_default();
            let client = get_client();
            match AdminApi::new(&client).delete_user(&id, &token).await {
                Ok(_) => {
                    ui_store.show_success(i18n.t("users.deleted"));
                    delete_user.set(None);
                    users.restart();
                }
                Err(e) => {
                    ui_store.show_error(format!("{}: {e}", i18n.t("users.delete_failed")));
                }
            }
            delete_saving.set(false);
        });
    };

    let edit_save_label = if edit_saving() {
        i18n.t("form.saving")
    } else {
        i18n.t("form.save")
    };
    let delete_button_label = if delete_saving() {
        i18n.t("users.deleting")
    } else {
        i18n.t("users.confirm_delete")
    };
    let can_edit_selected_role = edit_user()
        .as_ref()
        .map(|u| can_current_user_manage_roles && u.id != current_user_id && u.role != "system")
        .unwrap_or(false);

    rsx! {
        div { class: "page-header",
            h1 { class: "page-title", {i18n.t("page.users")} }
            p { class: "page-description", {i18n.t("users.subtitle")} }
        }

        div { class: "toolbar",
            div { class: "toolbar-left",
                div { class: "input-wrapper",
                    input {
                        class: "input-field",
                        r#type: "search",
                        placeholder: "{i18n.t(\"users.search_placeholder\")}",
                        value: "{search}",
                        oninput: move |e| {
                            *search.write() = e.value();
                            page.set(1);
                        },
                    }
                }
            }
        }

        div { class: "card",
            {
                let (is_empty, empty_text) = match users() {
                    None => (true, i18n.t("table.loading")),
                    Some(Err(_)) => (true, i18n.t("common.load_failed")),
                    Some(Ok(_)) if filtered_users().is_empty() => (true, i18n.t("users.empty")),
                    _ => (false, ""),
                };
                rsx! {
                    Table {
                        empty: is_empty,
                        empty_text: empty_text.to_string(),
                        col_count: 5,
                        thead {
                            tr {
                                TableHead { {i18n.t("users.user")} }
                                TableHead { {i18n.t("table.role")} }
                                TableHead { {i18n.t("users.tenant")} }
                                TableHead { {i18n.t("users.registered_at")} }
                                TableHead { {i18n.t("table.actions")} }
                            }
                        }
                        tbody {
                            for u in paged_users().iter() {
                                tr {
                                    td {
                                        div { class: "user-cell",
                                            span { class: "user-name",
                                                { u.name.clone().unwrap_or_else(|| u.email.clone()) }
                                            }
                                            span { class: "user-email text-secondary", "{u.email}" }
                                        }
                                    }
                                    td {
                                        Badge { variant: BadgeVariant::Info, "{u.role}" }
                                    }
                                    td { "{u.tenant_id}" }
                                    td { { format_time(&u.created_at) } }
                                    td {
                                        div { class: "btn-group",
                                            Button {
                                                variant: ButtonVariant::Ghost,
                                                size: ButtonSize::Small,
                                                onclick: {
                                                    let uu = u.clone();
                                                    move |_| {
                                                        edit_name.set(uu.name.clone().unwrap_or_default());
                                                        edit_role.set(uu.role.clone());
                                                        edit_user.set(Some(uu.clone()));
                                                    }
                                                },
                                                {i18n.t("form.edit")}
                                            }
                                            if u.id != current_user_id
                                                && u.role != UserRole::System.as_str()
                                                && (u.role != UserRole::Admin.as_str() || can_current_user_manage_roles) {
                                                Button {
                                                    variant: ButtonVariant::Danger,
                                                    size: ButtonSize::Small,
                                                    onclick: {
                                                        let uu = u.clone();
                                                        move |_| delete_user.set(Some(uu.clone()))
                                                    },
                                                    {i18n.t("form.delete")}
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        div { class: "pagination",
            span { class: "pagination-info",
                "{i18n.t(\"common.total_items\")} {filtered_users().len()} {i18n.t(\"pricing.items_suffix\")}"
            }
            Pagination {
                current: page(),
                total_pages: total_pages(),
                on_page_change: move |p| page.set(p),
            }
        }

        // ── 编辑用户弹窗 ──────────────────────────────────────────
        if edit_user().is_some() {
            div { class: "modal-backdrop",
                onclick: move |_| edit_user.set(None),
                div {
                    class: "modal",
                    onclick: move |e| e.stop_propagation(),
                    div { class: "modal-header",
                        h2 { class: "modal-title", {i18n.t("users.edit_title")} }
                        button {
                            class: "btn btn-ghost btn-sm",
                            r#type: "button",
                            onclick: move |_| edit_user.set(None),
                            "✕"
                        }
                    }
                    div { class: "modal-body",
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("users.display_name")} }
                            input {
                                class: "input-field",
                                placeholder: "{i18n.t(\"users.display_name_placeholder\")}",
                                value: "{edit_name}",
                                oninput: move |e| *edit_name.write() = e.value(),
                            }
                        }
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("table.role")} }
                            if can_edit_selected_role {
                                select {
                                    class: "input-field",
                                    value: "{edit_role}",
                                    onchange: move |e| *edit_role.write() = e.value(),
                                    option { value: "user", "{i18n.t(\"users.role_user\")}" }
                                    option { value: "admin", "{i18n.t(\"users.role_admin\")}" }
                                }
                            } else {
                                input {
                                    class: "input-field",
                                    value: "{edit_role}",
                                    readonly: true,
                                }
                            }
                        }
                    }
                    div { class: "modal-footer",
                        Button {
                            variant: ButtonVariant::Ghost,
                            onclick: move |_| edit_user.set(None),
                            {i18n.t("form.cancel")}
                        }
                        Button {
                            variant: ButtonVariant::Primary,
                            loading: edit_saving(),
                            onclick: on_edit_save,
                            "{edit_save_label}"
                        }
                    }
                }
            }
        }

        // ── 删除确认弹窗 ──────────────────────────────────────────
        if let Some(ref du) = delete_user() {
            div { class: "modal-backdrop",
                onclick: move |_| delete_user.set(None),
                div {
                    class: "modal",
                    onclick: move |e| e.stop_propagation(),
                    div { class: "modal-header",
                        h2 { class: "modal-title", {i18n.t("users.delete_confirm_title")} }
                    }
                    div { class: "modal-body",
                        p {
                            "{i18n.t(\"users.delete_confirm_prefix\")} "
                            strong { { du.name.clone().unwrap_or_else(|| du.email.clone()) } }
                            " ({du.email}) {i18n.t(\"users.delete_confirm_suffix\")}"
                        }
                    }
                    div { class: "modal-footer",
                        Button {
                            variant: ButtonVariant::Ghost,
                            onclick: move |_| delete_user.set(None),
                            {i18n.t("form.cancel")}
                        }
                        Button {
                            variant: ButtonVariant::Danger,
                            loading: delete_saving(),
                            onclick: on_delete_confirm,
                            "{delete_button_label}"
                        }
                    }
                }
            }
        }
    }
}

// ── 普通用户视图 ──────────────────────────────────────────────────────

#[component]
fn UserSelfView() -> Element {
    let i18n = use_i18n();
    let user_store = use_context::<UserStore>();
    let user_info = user_store.info.read();
    let nav = use_navigator();

    let display_name = user_info
        .as_ref()
        .map(|u| u.display_name().to_string())
        .unwrap_or_default();
    let email = user_info
        .as_ref()
        .map(|u| u.email.clone())
        .unwrap_or_default();
    let role = user_info
        .as_ref()
        .map(|u| u.role.clone())
        .unwrap_or_default();

    rsx! {
        div { class: "page-header",
            h1 { class: "page-title", {i18n.t("users.self_title")} }
            p { class: "page-description", {i18n.t("users.self_desc")} }
        }

        div { class: "card",
            div { class: "card-header",
                h3 { class: "card-title", {i18n.t("users.account_info")} }
                Button {
                    variant: ButtonVariant::Secondary,
                    size: ButtonSize::Small,
                    onclick: move |_| { nav.push(Route::UserProfile {}); },
                    {i18n.t("profile.edit")}
                }
            }
            div { class: "card-body",
                div { class: "info-grid",
                    div { class: "info-item",
                        span { class: "info-label", {i18n.t("users.display_name")} }
                        span { class: "info-value", "{display_name}" }
                    }
                    div { class: "info-item",
                        span { class: "info-label", {i18n.t("table.email")} }
                        span { class: "info-value", "{email}" }
                    }
                    div { class: "info-item",
                        span { class: "info-label", {i18n.t("table.role")} }
                        Badge { variant: BadgeVariant::Info, "{role}" }
                    }
                }
            }
        }
    }
}
