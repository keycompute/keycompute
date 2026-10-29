use super::usage_guide::ModelUsageGuide;
use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::api_key_service;
use crate::stores::auth_store::AuthStore;
use crate::stores::ui_store::UiStore;
use crate::stores::user_store::UserStore;
use crate::utils::on_copy;
use crate::utils::resource::{KeyedResourceValue, current_keyed_value};
use crate::utils::time::format_time;
use crate::views::tenant::common::{self, WorkspaceScope};
use dioxus::prelude::*;
use ui::{
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, ConfirmModal, Modal, Pagination, Table,
    TableHead,
    icons::{IconCopy, IconPlus},
};

const PAGE_SIZE: usize = 20;

#[component]
pub fn ApiKeyList() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    rsx! {if let Some(scope)=WorkspaceScope::from_stores(auth,users){for key in [format!("{scope:?}")]{ApiKeyWorkspace{key:"{key}",scope}}}
    else {div{class:"page-container",p{role:"alert",{i18n.t("tenant_keys.select_workspace")}}Link{to:Route::TenantWorkspace{},{i18n.t("tenant.workspace")}}}}}
}
#[component]
fn ApiKeyWorkspace(scope: WorkspaceScope) -> Element {
    let user_store = use_context::<UserStore>();
    let i18n = use_i18n();
    let auth_store = use_context::<AuthStore>();
    let ui_store = use_context::<UiStore>();
    let mut show_create = use_signal(|| false);
    let mut new_key_name = use_signal(String::new);
    let mut creating = use_signal(|| false);
    let mut create_error = use_signal(|| Option::<String>::None);
    let mut new_key_value = use_signal(|| Option::<String>::None);
    let mut new_key_id = use_signal(|| Option::<uuid::Uuid>::None);
    let mut new_key_revealed = use_signal(|| false);
    let mut delete_candidate = use_signal(|| Option::<(String, String)>::None);
    let mut delete_modal_open = use_signal(|| false);
    let mut deleting = use_signal(|| false);
    let mut delete_error = use_signal(String::new);
    let mut page = use_signal(|| 1u32);
    let mut page_size = use_signal(|| PAGE_SIZE as u32);
    // 是否显示已撤销的 Key（默认不显示）
    let mut include_revoked = use_signal(|| false);
    // 复制状态
    let mut copied = use_signal(|| false);
    let create_failed = i18n.t("api_keys.create_failed");
    // 拉取 key 列表
    let mut keys = use_resource(move || async move {
        let request_key = (include_revoked(), page(), page_size());
        let result = common::read(auth_store, user_store, scope, |token| async move {
            api_key_service::list_page(request_key.0, request_key.1, request_key.2, &token).await
        })
        .await;
        KeyedResourceValue::new(request_key, result)
    });

    let on_create = move |evt: Event<FormData>| {
        evt.prevent_default();
        if creating() || !scope.is_current(auth_store, user_store) {
            return;
        }
        let name = new_key_name().trim().to_string();
        if name.is_empty() {
            create_error.set(Some(i18n.t("api_keys.name_required").to_string()));
            return;
        }
        creating.set(true);
        create_error.set(None);
        spawn(async move {
            let result = common::command(auth_store, user_store, scope, move |token| async move {
                api_key_service::create(&name, &token).await
            })
            .await;
            if !scope.is_current(auth_store, user_store) {
                return;
            }
            match result {
                Ok(resp) => {
                    let key_id = uuid::Uuid::parse_str(&resp.id).ok();
                    new_key_id.set(key_id);
                    new_key_value.set(Some(resp.api_key));
                    new_key_revealed.set(false);
                    show_create.set(false);
                    new_key_name.set(String::new());
                    creating.set(false);
                    page.set(1);
                    // 重新拉取列表
                    keys.restart();
                }
                Err(e) => {
                    create_error.set(Some(format!(
                        "{create_failed}：{}",
                        crate::services::api_client::user_error_message(i18n, &e)
                    )));
                    creating.set(false);
                }
            }
        });
    };

    let mut on_delete = move |id: String| {
        if deleting() {
            return;
        }
        deleting.set(true);
        delete_error.set(String::new());
        spawn(async move {
            let command_id = id.clone();
            let result = common::command(auth_store, user_store, scope, move |token| async move {
                api_key_service::delete(&command_id, &token).await
            })
            .await;
            if !scope.is_current(auth_store, user_store) {
                return;
            }
            deleting.set(false);
            match result {
                Ok(_) => {
                    if let Ok(key_id) = uuid::Uuid::parse_str(&id)
                        && new_key_id() == Some(key_id)
                    {
                        new_key_id.set(None);
                        new_key_value.set(None);
                        new_key_revealed.set(false);
                        copied.set(false);
                    }
                    delete_candidate.set(None);
                    delete_modal_open.set(false);
                    keys.restart();
                }
                Err(error) => {
                    delete_error.set(crate::services::api_client::user_error_message(
                        i18n, &error,
                    ));
                    delete_candidate.set(None);
                    delete_modal_open.set(false);
                }
            }
        });
    };

    rsx! {
        div { class: "page-container kc-api-page",
            div { class: "page-header kc-api-header",
                div { class: "kc-api-heading",
                    h1 { class: "page-title", {i18n.t("page.api_keys")} }
                    p { class: "page-subtitle", {i18n.t("api_keys.subtitle")} }
                }
                div { class: "kc-api-actions",
                    Link {class:"btn btn-secondary",to:Route::OwnerKeyIssuance {},{i18n.t("tenant_keys.my_requests")}}
                    Button {
                        variant: ButtonVariant::Primary,
                        onclick: move |_| {
                            show_create.set(true);
                            create_error.set(None);
                            new_key_name.set(String::new());
                            new_key_id.set(None);
                            new_key_value.set(None);
                            new_key_revealed.set(false);
                        },
                        IconPlus { size: 16 }
                        {i18n.t("api_keys.create")}
                    }
                }
            }

            // 筛选工具栏
            div { class: "toolbar kc-api-toolbar",
                div { class: "toolbar-left",
                    div { class: "filter-tabs",
                        button {
                            class: if !include_revoked() { "filter-tab active" } else { "filter-tab" },
                            r#type: "button",
                            onclick: move |_| {
                                include_revoked.set(false);
                                page.set(1);
                                keys.restart();
                            },
                            {i18n.t("api_keys.current")}
                        }
                        button {
                            class: if include_revoked() { "filter-tab active" } else { "filter-tab" },
                            r#type: "button",
                            onclick: move |_| {
                                include_revoked.set(true);
                                page.set(1);
                                keys.restart();
                            },
                            {i18n.t("api_keys.all_with_revoked")}
                        }
                    }
                }
            }

            // Keep the secret separate from reusable invocation guidance.
            if let Some(key) = new_key_value() {
                section {class:"kc-api-success-panel",
                    h2 {{i18n.t("api_keys.created_title")}}
                    p {{i18n.t("api_keys.created_once")}}
                    div {class:"kc-api-copy-block",pre {class:"kc-api-example",if new_key_revealed(){"{key}"}else{"••••••••••••••••••••"}}
                        button {class:"btn btn-secondary",r#type:"button",onclick:move |_|new_key_revealed.toggle(),{i18n.t(if new_key_revealed(){"api_keys.hide_secret"}else{"api_keys.reveal_secret"})}}
                        button {class:"btn btn-secondary",r#type:"button",disabled:!new_key_revealed(),onclick:on_copy(key.clone(),i18n.t("common.copy_manual_hint").to_string(),ui_store,copied),
                            IconCopy {size:15} {i18n.t("api_keys.copy")}
                        }
                    }
                    Button {variant:ButtonVariant::Ghost,size:ButtonSize::Small,onclick:move|_|{new_key_id.set(None);new_key_value.set(None);new_key_revealed.set(false);copied.set(false);},{i18n.t("api_keys.close_saved")}}
                }
            }
            ModelUsageGuide {key:"{new_key_value().is_some()}:{new_key_revealed()}",api_key:if new_key_revealed(){new_key_value()}else{None}}

            // 创建弹窗
            Modal {
                open: show_create,
                title: i18n.t("api_keys.create_title").to_string(),
                close_label: i18n.t("common.close").to_string(),
                onclose: move |_| if !creating() { show_create.set(false) },
                footer: rsx! {
                    Button {
                        variant: ButtonVariant::Ghost,
                        r#type: "button".to_string(),
                        disabled: creating(),
                        onclick: move |_| {
                            if !creating() {
                                show_create.set(false);
                            }
                        },
                        {i18n.t("form.cancel")}
                    }
                    Button {
                        variant: ButtonVariant::Primary,
                        r#type: "submit".to_string(),
                        form: "api-key-create-form".to_string(),
                        loading: creating(),
                        if creating() {
                            {i18n.t("api_keys.creating")}
                        } else {
                            {i18n.t("form.create")}
                        }
                    }
                },
                if let Some(err) = create_error() {
                    div { class: "alert alert-error", role: "alert", "{err}" }
                }
                form { id: "api-key-create-form", onsubmit: on_create,
                    div { class: "form-group",
                        label { class: "form-label", r#for: "api-key-name", {i18n.t("api_keys.name")} }
                        input {
                            id: "api-key-name",
                            class: "form-input",
                            r#type: "text",
                            autofocus: true,
                            required: true,
                            placeholder: "{i18n.t(\"api_keys.name_placeholder\")}",
                            value: "{new_key_name}",
                            oninput: move |e| new_key_name.set(e.value()),
                        }
                    }
                }
            }

            ConfirmModal {
                open: delete_modal_open,
                title: i18n.t("api_keys.delete_confirm_title").to_string(),
                message: delete_candidate()
                    .as_ref()
                    .map(|(_, name)| {
                        i18n.t_with_args("api_keys.delete_confirm_message", &[("name", name)])
                    })
                    .unwrap_or_default(),
                confirm_text: i18n.t("api_keys.delete_permanently").to_string(),
                cancel_text: i18n.t("form.cancel").to_string(),
                close_label: i18n.t("common.close").to_string(),
                danger: true,
                busy: deleting(),
                onconfirm: move |_| {
                    if let Some((id, _)) = delete_candidate() {
                        on_delete(id);
                    }
                },
                oncancel: move |_| {
                    if !deleting() {
                        delete_candidate.set(None);
                        delete_modal_open.set(false);
                    }
                },
            }

            if !delete_error().is_empty() {
                div {class:"alert alert-error",role:"alert","{delete_error}"}
            }



            {
                let request_key = (include_revoked(), page(), page_size());
                let current_keys = current_keyed_value(
                    &request_key,
                    keys.state().cloned(),
                    keys(),
                );
                match current_keys {
                    None => rsx! {
                        div { class: "loading-state", {i18n.t("table.loading")} }
                    },
                    Some(Err(e)) => rsx! {
                        div { class: "alert alert-error",
                            {format!("{}：{}", i18n.t("api_keys.loading_failed"), crate::services::api_client::user_error_message(i18n, &e))}
                        }
                    },
                    Some(Ok(result)) => {
                        let total = result.total.max(0) as usize;
                        let total_pages = result.total_pages.max(1) as u32;
                        let paged = &result.keys;
                        // 空态与非空态共用同一面板结构，仅 meta 文案与表格空态不同。
                        let registry_empty = paged.is_empty() && total == 0;
                        rsx! {
                            div { class: "kc-api-table-panel table-pagination-panel",
                                div { class: "kc-api-table-meta",
                                    div {
                                        span { {i18n.t("api_keys.registry")} }
                                        strong { "{total}" }
                                    }
                                    p {
                                        if registry_empty {
                                            {i18n.t("api_keys.empty_meta")}
                                        } else if include_revoked() {
                                            {i18n.t("api_keys.all_meta")}
                                        } else {
                                            {i18n.t("api_keys.active_meta")}
                                        }
                                    }
                                }
                                Table {
                                    class: "kc-api-table".to_string(),
                                    col_count: 5,
                                    empty: registry_empty,
                                    empty_text: i18n.t("api_keys.empty").to_string(),
                                    thead {
                                        tr {
                                            TableHead { {i18n.t("table.name")} }
                                            TableHead { {i18n.t("api_keys.prefix")} }
                                            TableHead { {i18n.t("table.status")} }
                                            TableHead { {i18n.t("table.created_at")} }
                                            TableHead { {i18n.t("table.actions")} }
                                        }
                                    }
                                    tbody {
                                        for key in paged.iter() {
                                            tr { key: "{key.id}",
                                                td { class: "kc-api-key-name", "{key.name}" }
                                                td {
                                                    OwnerKeySecret {
                                                        scope,
                                                        key_id:key.id.clone(),
                                                        preview:key.key_preview.clone(),
                                                        revealable:key.is_active,
                                                    }
                                                }
                                                td {
                                                    Badge { variant: if key.revoked() { BadgeVariant::Error } else if key.expired() { BadgeVariant::Warning } else { BadgeVariant::Success },
                                                        if key.revoked() {
                                                            {i18n.t("api_keys.revoked")}
                                                        } else if key.expired() {
                                                            {i18n.t("api_keys.expired")}
                                                        } else {
                                                            {i18n.t("api_keys.active")}
                                                        }
                                                    }
                                                }
                                                td { {format_time(&key.created_at)} }
                                                td {
                                                    Button {
                                                        variant: ButtonVariant::Danger,
                                                        size: ButtonSize::Small,
                                                        onclick: {
                                                            let id = key.id.to_string();
                                                            let name = key.name.clone();
                                                            move |_| {
                                                                delete_error.set(String::new());
                                                                delete_candidate.set(Some((id.clone(), name.clone())));
                                                                delete_modal_open.set(true);
                                                            }
                                                        },
                                                        {i18n.t("form.delete")}
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            // 分页页脚与面板平级渲染（对齐定价页的页脚结构），避免页脚嵌入面板内部。
                            Pagination {
                                current: page(),
                                total_pages,
                                total: total as u64,
                                page_size: page_size(),
                                summary: i18n.t_with_args(
                                    "common.pagination_summary",
                                    &[
                                        ("total", &total.to_string()),
                                        ("current", &page().to_string()),
                                        ("total_pages", &total_pages.to_string()),
                                    ],
                                ),
                                page_size_label: i18n.t("common.pagination_page_size").to_string(),
                                page_size_suffix: i18n.t("common.items_suffix").to_string(),
                                previous_label: i18n.t("table.previous").to_string(),
                                next_label: i18n.t("table.next").to_string(),
                                on_page_change: move |p| page.set(p),
                                on_page_size_change: move |size| {
                                    page_size.set(size);
                                    page.set(1);
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn OwnerKeySecret(
    scope: WorkspaceScope,
    key_id: String,
    preview: String,
    revealable: bool,
) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut revealed = use_signal(|| false);
    let mut confirming = use_signal(|| false);
    let mut current_password = use_signal(String::new);
    let mut secret = use_signal(|| None::<String>);
    // Expiration, rotation or deletion makes an in-memory secret
    // non-revealable. Never persist it in browser storage.
    let inactive_key_id = key_id.clone();
    use_effect(use_reactive!(|revealable, inactive_key_id, scope| {
        if !revealable {
            let _ = (&inactive_key_id, scope);
            secret.set(None);
            revealed.set(false);
            confirming.set(false);
            current_password.set(String::new());
        }
    }));
    let toggle = move |_| {
        if busy() || !revealable {
            return;
        }
        if revealed() {
            secret.set(None);
            revealed.set(false);
            current_password.set(String::new());
            error.set(String::new());
            return;
        }
        confirming.set(true);
        error.set(String::new());
    };
    let reveal_key_id = key_id.clone();
    let reveal = move |event: Event<FormData>| {
        event.prevent_default();
        if busy() || !revealable {
            return;
        }
        let password = current_password();
        if password.is_empty() {
            error.set(i18n.t("api_keys.password_required").into());
            return;
        }
        let request_key_id = reveal_key_id.clone();
        busy.set(true);
        error.set(String::new());
        spawn(async move {
            let request_id_for_call = request_key_id.clone();
            let result = common::read(auth, users, scope, move |token| {
                let key_id = request_id_for_call.clone();
                let password = password.clone();
                async move { api_key_service::reveal(&key_id, &password, &token).await }
            })
            .await;
            if !scope.is_current(auth, users) {
                return;
            }
            busy.set(false);
            current_password.set(String::new());
            match result {
                Ok(value) => {
                    secret.set(Some(value));
                    revealed.set(true);
                    confirming.set(false);
                }
                Err(e) => {
                    secret.set(None);
                    revealed.set(false);
                    confirming.set(true);
                    error.set(crate::services::api_client::user_error_message(i18n, &e));
                }
            }
        });
    };
    rsx! {div {class:"kc-api-copy-block cached-key-secret",
        code {class:"kc-api-key-preview",if revealable && revealed(){if let Some(value)=secret(){"{value}"}else{"{preview}"}}else{"{preview} · ••••••••••••"}}
        button {class:"btn btn-ghost btn-sm",r#type:"button",disabled:busy() || !revealable,onclick:toggle,{i18n.t(if revealable && revealed(){"api_keys.hide_secret"}else{"api_keys.reveal_secret"})}}
        if confirming() && !revealed() {
            form {class:"kc-api-reveal-confirm",onsubmit:reveal,
                label {class:"sr-only",r#for:"reveal-password-{key_id}",{i18n.t("account_settings.current_password")}}
                input {
                    id:"reveal-password-{key_id}",
                    class:"form-input",
                    r#type:"password",
                    autocomplete:"current-password",
                    maxlength:"4096",
                    placeholder:i18n.t("account_settings.current_password_placeholder"),
                    value:"{current_password}",
                    disabled:busy(),
                    oninput:move|event|current_password.set(event.value()),
                }
                button {class:"btn btn-primary btn-sm",r#type:"submit",disabled:busy() || current_password().is_empty(),{i18n.t("api_keys.confirm_reveal")}}
                button {class:"btn btn-ghost btn-sm",r#type:"button",disabled:busy(),onclick:move|_|{confirming.set(false);current_password.set(String::new());error.set(String::new());},{i18n.t("form.cancel")}}
            }
        }
        if !error().is_empty(){small{class:"text-danger",role:"alert","{error}"}}
    }}
}

#[cfg(test)]
mod tests {
    use super::{KeyedResourceValue, current_keyed_value};
    use dioxus::prelude::UseResourceState;

    #[test]
    fn api_key_rows_are_stale_when_any_list_input_changes() {
        let loaded = KeyedResourceValue::new((false, 1u32, 20u32), vec!["old row"]);

        assert_eq!(
            current_keyed_value(
                &(false, 2u32, 20u32),
                UseResourceState::Ready,
                Some(loaded.clone()),
            ),
            None
        );
        assert_eq!(
            current_keyed_value(
                &(false, 1u32, 50u32),
                UseResourceState::Ready,
                Some(loaded.clone()),
            ),
            None
        );
        assert_eq!(
            current_keyed_value(&(true, 1u32, 20u32), UseResourceState::Ready, Some(loaded),),
            None
        );
    }

    #[test]
    fn empty_and_non_empty_states_share_one_panel_and_pagination_block() {
        let source = include_str!("list.rs");
        let component_source = source.split("#[cfg(test)]").next().unwrap_or(source);

        assert_eq!(
            component_source.matches("kc-api-table-panel").count(),
            1,
            "空态与非空态应共用同一表格面板"
        );
        // 分页页脚脱离面板独立渲染（对齐定价页的页脚结构），且只渲染一次。
        assert_eq!(
            component_source.matches("Pagination {").count(),
            1,
            "分页页脚应只渲染一次"
        );
        assert!(
            !component_source.contains("kc-api-pagination"),
            "分页页脚不应再有额外包裹容器"
        );
    }
}
