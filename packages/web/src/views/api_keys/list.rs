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
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, ConfirmModal, Pagination, Table,
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
    let mut delete_candidate = use_signal(|| Option::<(String, String)>::None);
    let mut delete_modal_open = use_signal(|| false);
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
        let name = new_key_name();
        if name.is_empty() {
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
                    new_key_value.set(Some(resp.api_key));
                    show_create.set(false);
                    new_key_name.set(String::new());
                    creating.set(false);
                    page.set(1);
                    // 重新拉取列表
                    keys.restart();
                }
                Err(e) => {
                    create_error.set(Some(format!("{create_failed}：{e}")));
                    creating.set(false);
                }
            }
        });
    };

    let on_delete = move |id: String| {
        spawn(async move {
            let result = common::command(auth_store, user_store, scope, move |token| async move {
                api_key_service::delete(&id, &token).await
            })
            .await;
            if !scope.is_current(auth_store, user_store) {
                return;
            }
            if result.is_ok() {
                keys.restart();
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
                            new_key_value.set(None);
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
                            {i18n.t("api_keys.active")}
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

            // Keep the one-time secret separate from reusable invocation guidance.
            if let Some(key) = new_key_value() {
                section {class:"kc-api-success-panel",
                    h2 {{i18n.t("api_keys.created_title")}}
                    p {{i18n.t("api_keys.created_once")}}
                    div {class:"kc-api-copy-block",pre {class:"kc-api-example","{key}"}
                        button {class:"btn btn-secondary",r#type:"button",onclick:on_copy(key.clone(),i18n.t("common.copy_manual_hint").to_string(),ui_store,copied),
                            IconCopy {size:15} {i18n.t("api_keys.copy")}
                        }
                    }
                    Button {variant:ButtonVariant::Ghost,size:ButtonSize::Small,onclick:move|_|{new_key_value.set(None);copied.set(false);},{i18n.t("api_keys.close_saved")}}
                }
            }
            ModelUsageGuide {key:"{new_key_value().is_some()}",api_key:new_key_value()}

            // 创建弹窗
            if show_create() {
                div { class: "modal-overlay",
                    div {
                        class: "modal",
                        role: "dialog",
                        aria_modal: "true",
                        aria_label: i18n.t("api_keys.create_title"),
                        h2 { class: "modal-title", {i18n.t("api_keys.create_title")} }
                        if let Some(err) = create_error() {
                            div { class: "alert alert-error", "{err}" }
                        }
                        form { onsubmit: on_create,
                            div { class: "form-group",
                                label { class: "form-label", {i18n.t("api_keys.name")} }
                                input {
                                    class: "form-input",
                                    r#type: "text",
                                    placeholder: "{i18n.t(\"api_keys.name_placeholder\")}",
                                    value: "{new_key_name}",
                                    oninput: move |e| new_key_name.set(e.value()),
                                }
                            }
                            div { class: "modal-actions",
                                Button {
                                    variant: ButtonVariant::Ghost,
                                    r#type: "button".to_string(),
                                    onclick: move |_| show_create.set(false),
                                    {i18n.t("form.cancel")}
                                }
                                Button {
                                    variant: ButtonVariant::Primary,
                                    r#type: "submit".to_string(),
                                    loading: creating(),
                                    if creating() {
                                        {i18n.t("api_keys.creating")}
                                    } else {
                                        {i18n.t("form.create")}
                                    }
                                }
                            }
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
                confirm_text: i18n.t("form.delete").to_string(),
                cancel_text: i18n.t("form.cancel").to_string(),
                close_label: i18n.t("common.close").to_string(),
                danger: true,
                onconfirm: move |_| {
                    if let Some((id, _)) = delete_candidate() {
                        on_delete(id);
                    }
                    delete_candidate.set(None);
                    delete_modal_open.set(false);
                },
                oncancel: move |_| {
                    delete_candidate.set(None);
                    delete_modal_open.set(false);
                },
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
                        div { class: "alert alert-error", "{i18n.t(\"api_keys.loading_failed\")}：{e}" }
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
                                                    code { class: "kc-api-key-preview", "{key.key_preview}" }
                                                }
                                                td {
                                                    Badge { variant: if key.revoked() { BadgeVariant::Error } else { BadgeVariant::Success },
                                                        if key.revoked() {
                                                            {i18n.t("api_keys.revoked")}
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
