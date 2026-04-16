use dioxus::prelude::*;
use ui::{
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Pagination, Table, TableHead,
    icons::IconPlus,
};

const PAGE_SIZE: usize = 20;

use crate::hooks::use_i18n::use_i18n;
use crate::services::{api_client::with_auto_refresh, api_key_service, model_service};
use crate::stores::auth_store::AuthStore;
use crate::utils::time::format_time;

/// 复制文本到剪贴板（WASM 环境）
fn copy_to_clipboard(text: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = web_sys::window().map(|w| {
            let clipboard = w.navigator().clipboard();
            clipboard.write_text(text)
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = text; // 非 WASM 环境暂不支持
    }
}

#[component]
pub fn ApiKeyList() -> Element {
    let i18n = use_i18n();
    let auth_store = use_context::<AuthStore>();
    let mut show_create = use_signal(|| false);
    let mut new_key_name = use_signal(String::new);
    let mut creating = use_signal(|| false);
    let mut create_error = use_signal(|| Option::<String>::None);
    let mut new_key_value = use_signal(|| Option::<String>::None);
    let mut page = use_signal(|| 1u32);
    // 是否显示已撤销的 Key（默认不显示）
    let mut include_revoked = use_signal(|| false);
    // 复制状态
    let mut copied = use_signal(|| false);
    let create_failed = i18n.t("api_keys.create_failed");

    // 获取模型列表（用于显示用法示例）
    let models = use_resource(move || async move { model_service::list_models().await.ok() });

    // 拉取 key 列表
    let mut keys = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            api_key_service::list(include_revoked(), &token).await
        })
        .await
    });

    let on_create = move |evt: Event<FormData>| {
        evt.prevent_default();
        let name = new_key_name();
        if name.is_empty() {
            return;
        }
        creating.set(true);
        create_error.set(None);
        spawn(async move {
            let token = auth_store.token().unwrap_or_default();
            match api_key_service::create(&name, &token).await {
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
            let token = auth_store.token().unwrap_or_default();
            if api_key_service::delete(&id, &token).await.is_ok() {
                keys.restart();
            }
        });
    };

    rsx! {
        div {
            class: "page-container kc-api-page",
            div {
                class: "page-header kc-api-header",
                div { class: "kc-api-heading",
                    h1 { class: "page-title", {i18n.t("page.api_keys")} }
                    p { class: "page-subtitle", {i18n.t("api_keys.subtitle")} }
                }
                div { class: "kc-api-actions",
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

            // 新建成功后展示完整密钥（仅一次）
            if let Some(key) = new_key_value() {
                {
                    // 使用编译时配置的 API_BASE_URL，移除末尾的 /v1 后缀（如果有）
                    let api_url = crate::services::api_client::get_client()
                        .config()
                        .base_url
                        .trim_end_matches('/')
                        .trim_end_matches("/v1")
                        .to_string();

                    // 获取第一个模型作为示例
                    let sample_model = models()
                        .flatten()
                        .and_then(|m| m.data.first().map(|model| model.id.clone()))
                        .unwrap_or_else(|| "deepseek-chat".to_string());

                    // 生成要复制的文本
                    let example_text = format!(
                        r#"API_URL="{}/v1"
API_KEY="{}"
API_MODEL="{}""#,
                        api_url, key, sample_model
                    );
                    let example_text_for_click = example_text.clone();

                    let copied_label = i18n.t("api_keys.copied");
                    let copy_hint = i18n.t("api_keys.copy_hint");
                    rsx! {
                        div {
                            class: "alert alert-success kc-api-secret-alert",
                            div { class: "kc-api-secret-topline",
                                strong { {i18n.t("api_keys.created_title")} }
                                span { {i18n.t("api_keys.created_once")} }
                            }
                            code { class: "key-display", "{key}" }
                            p { class: "kc-api-secret-label", {i18n.t("api_keys.example")} }
                            div { class: "kc-api-copy-block",
                                pre {
                                    class: if copied() { "kc-api-example copied" } else { "kc-api-example" },
                                    title: if copied() { copied_label } else { copy_hint },
                                    onclick: {
                                        let text = example_text_for_click.clone();
                                        move |_| {
                                            copy_to_clipboard(&text);
                                            copied.set(true);
                                            // 2秒后重置状态
                                            let mut copied_clone = copied.clone();
                                            spawn(async move {
                                                gloo_timers::future::TimeoutFuture::new(2000).await;
                                                copied_clone.set(false);
                                            });
                                        }
                                    },
                                    "{example_text}"
                                }
                                div { class: "kc-api-copy-hint",
                                    if copied() {
                                        {copied_label}
                                    } else {
                                        {copy_hint}
                                    }
                                }
                            }
                            p { class: "kc-api-secret-note", {i18n.t("api_keys.example_note")} }
                            Button {
                                variant: ButtonVariant::Ghost,
                                size: ButtonSize::Small,
                                onclick: move |_| {
                                    new_key_value.set(None);
                                    copied.set(false);
                                },
                                {i18n.t("api_keys.close_saved")}
                            }
                        }
                    }
                }
            }

            // 创建弹窗
            if show_create() {
                div {
                    class: "modal-overlay",
                    div {
                        class: "modal",
                        h2 { class: "modal-title", {i18n.t("api_keys.create_title")} }
                        if let Some(err) = create_error() {
                            div { class: "alert alert-error", "{err}" }
                        }
                        form {
                            onsubmit: on_create,
                            div {
                                class: "form-group",
                                label { class: "form-label", {i18n.t("api_keys.name")} }
                                input {
                                    class: "form-input",
                                    r#type: "text",
                                    placeholder: "{i18n.t(\"api_keys.name_placeholder\")}",
                                    value: "{new_key_name}",
                                    oninput: move |e| new_key_name.set(e.value()),
                                }
                            }
                            div {
                                class: "modal-actions",
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
                                    if creating() { {i18n.t("api_keys.creating")} } else { {i18n.t("form.create")} }
                                }
                            }
                        }
                    }
                }
            }

            match keys() {
                None => rsx! {
                    div { class: "loading-state", {i18n.t("table.loading")} }
                },
                Some(Err(e)) => rsx! {
                    div { class: "alert alert-error", "{i18n.t(\"api_keys.loading_failed\")}：{e}" }
                },
                Some(Ok(list)) => {
                    let total = list.len();
                    let total_pages = total.div_ceil(PAGE_SIZE).max(1) as u32;
                    let start = (page() as usize - 1) * PAGE_SIZE;
                    let paged: Vec<_> = list.iter().skip(start).take(PAGE_SIZE).collect();
                    if paged.is_empty() && total == 0 {
                        rsx! {
                            div { class: "kc-api-table-panel",
                                div { class: "kc-api-table-meta",
                                    div {
                                        span { {i18n.t("api_keys.registry")} }
                                        strong { "0" }
                                    }
                                    p { {i18n.t("api_keys.empty_meta")} }
                                }
                                Table {
                                    class: "kc-api-table".to_string(),
                                    col_count: 5,
                                    empty: true,
                                    empty_text: i18n.t("api_keys.empty").to_string(),
                                    thead { tr { TableHead { "" } } }
                                }
                            }
                        }
                    } else {
                        rsx! {
                            div { class: "kc-api-table-panel",
                                div { class: "kc-api-table-meta",
                                    div {
                                        span { {i18n.t("api_keys.registry")} }
                                        strong { "{total}" }
                                    }
                                    p {
                                        if include_revoked() {
                                            {i18n.t("api_keys.all_meta")}
                                        } else {
                                            {i18n.t("api_keys.active_meta")}
                                        }
                                    }
                                }
                                Table {
                                    class: "kc-api-table".to_string(),
                                    col_count: 5,
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
                                            tr {
                                                key: "{key.id}",
                                                td { class: "kc-api-key-name", "{key.name}" }
                                                td { code { class: "kc-api-key-preview", "{key.key_preview}" } }
                                                td {
                                                    Badge {
                                                        variant: if key.revoked() { BadgeVariant::Error } else { BadgeVariant::Success },
                                                        if key.revoked() { {i18n.t("api_keys.revoked")} } else { {i18n.t("api_keys.active")} }
                                                    }
                                                }
                                                td { { format_time(&key.created_at) } }
                                                td {
                                                    Button {
                                                        variant: ButtonVariant::Danger,
                                                        size: ButtonSize::Small,
                                                        onclick: {
                                                            let id = key.id.to_string();
                                                            move |_| on_delete(id.clone())
                                                        },
                                                        {i18n.t("form.delete")}
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                div { class: "pagination kc-api-pagination",
                                    span { class: "pagination-info", "{i18n.t(\"dashboard.total\")} {total}" }
                                    Pagination {
                                        current: page(),
                                        total_pages,
                                        on_page_change: move |p| page.set(p),
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
