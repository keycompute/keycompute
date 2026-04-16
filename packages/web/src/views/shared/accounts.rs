use dioxus::prelude::*;
use ui::{Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Pagination, Table, TableHead};

const PAGE_SIZE: usize = 20;

use crate::hooks::use_i18n::use_i18n;
use crate::services::{account_service, api_client::with_auto_refresh, debug_service};
use crate::stores::auth_store::AuthStore;
use crate::stores::ui_store::UiStore;
use crate::stores::user_store::UserStore;
use crate::utils::time::format_time;

/// Provider 选项列表（后端支持的 provider）
const PROVIDERS: &[(&str, &str)] = &[
    ("openai", "OpenAI"),
    ("claude", "Anthropic Claude"),
    ("deepseek", "DeepSeek"),
    ("gemini", "Google Gemini"),
    ("vllm", "vLLM"),
    ("ollama", "Ollama"),
];

fn provider_label(provider: &str) -> &str {
    PROVIDERS
        .iter()
        .find(|(value, _)| *value == provider)
        .map(|(_, label)| *label)
        .unwrap_or(provider)
}

/// 根据 provider 获取默认模型列表
fn default_models_for(provider: &str) -> Vec<String> {
    match provider {
        "openai" => vec!["gpt-4o".into(), "gpt-4o-mini".into(), "gpt-4-turbo".into()],
        "claude" => vec![
            "claude-3-5-sonnet-latest".into(),
            "claude-3-opus-latest".into(),
        ],
        "deepseek" => vec!["deepseek-chat".into(), "deepseek-coder".into()],
        "gemini" => vec!["gemini-1.5-pro".into(), "gemini-1.5-flash".into()],
        "vllm" => vec![], // 用户需自行配置
        "ollama" => vec![
            // GPT-OSS
            "gpt-oss".into(),
            "gpt-oss:latest".into(),
            "gpt-oss:20b".into(),
            "gpt-oss:120b".into(),
            "gpt-oss:20b-cloud".into(),
            "gpt-oss:120b-cloud".into(),
            // Qwen 3.5
            "qwen3.5".into(),
            "qwen3.5:latest".into(),
            "qwen3.5:cloud".into(),
            "qwen3.5:0.8b".into(),
            "qwen3.5:2b".into(),
            "qwen3.5:4b".into(),
            "qwen3.5:9b".into(),
            "qwen3.5:27b".into(),
            "qwen3.5:35b".into(),
            "qwen3.5:122b".into(),
            "qwen3.5:397b-cloud".into(),
            // Qwen3-VL
            "qwen3-vl".into(),
            "qwen3-vl:latest".into(),
            "qwen3-vl:2b".into(),
            "qwen3-vl:4b".into(),
            "qwen3-vl:8b".into(),
            "qwen3-vl:30b".into(),
            "qwen3-vl:32b".into(),
            "qwen3-vl:235b".into(),
            "qwen3-vl:235b-cloud".into(),
            "qwen3-vl:235b-instruct-cloud".into(),
            // Qwen3-Coder
            "qwen3-coder".into(),
            "qwen3-coder:latest".into(),
            "qwen3-coder:30b".into(),
            "qwen3-coder:480b".into(),
            "qwen3-coder:480b-cloud".into(),
            // Qwen3-Next
            "qwen3-next".into(),
            "qwen3-next:latest".into(),
            "qwen3-next:80b".into(),
            "qwen3-next:80b-cloud".into(),
            // Qwen3-Coder-Next
            "qwen3-coder-next".into(),
            "qwen3-coder-next:latest".into(),
            "qwen3-coder-next:cloud".into(),
            "qwen3-coder-next:q4_K_M".into(),
            "qwen3-coder-next:q8_0".into(),
            // MiniMax
            "minimax-m2:cloud".into(),
            "minimax-m2.1:cloud".into(),
            "minimax-m2.5:cloud".into(),
            "minimax-m2.7:cloud".into(),
            // DeepSeek V3
            "deepseek-v3.1".into(),
            "deepseek-v3.1:latest".into(),
            "deepseek-v3.1:671b".into(),
            "deepseek-v3.1:671b-cloud".into(),
            "deepseek-v3.2:cloud".into(),
            // GLM
            "glm-4.6:cloud".into(),
            "glm-4.7:cloud".into(),
            "glm-5:cloud".into(),
            // Kimi K2
            "kimi-k2:1t-cloud".into(),
            "kimi-k2-thinking:cloud".into(),
            "kimi-k2.5:cloud".into(),
            // Gemma3
            "gemma3".into(),
            "gemma3:latest".into(),
            "gemma3:270m".into(),
            "gemma3:1b".into(),
            "gemma3:4b".into(),
            "gemma3:12b".into(),
            "gemma3:27b".into(),
            "gemma3:4b-cloud".into(),
            "gemma3:12b-cloud".into(),
            "gemma3:27b-cloud".into(),
            // Gemma4
            "gemma4".into(),
            "gemma4:latest".into(),
            "gemma4:e2b".into(),
            "gemma4:e4b".into(),
            "gemma4:26b".into(),
            "gemma4:31b".into(),
            "gemma4:31b-cloud".into(),
            // Mistral Large 3
            "mistral-large-3:675b-cloud".into(),
            // Devstral
            "devstral-small-2".into(),
            "devstral-small-2:latest".into(),
            "devstral-small-2:24b".into(),
            "devstral-small-2:24b-cloud".into(),
            "devstral-2".into(),
            "devstral-2:latest".into(),
            "devstral-2:123b".into(),
            "devstral-2:123b-cloud".into(),
            // Ministral 3
            "ministral-3".into(),
            "ministral-3:latest".into(),
            "ministral-3:3b".into(),
            "ministral-3:8b".into(),
            "ministral-3:14b".into(),
            "ministral-3:3b-cloud".into(),
            "ministral-3:8b-cloud".into(),
            "ministral-3:14b-cloud".into(),
            // NVIDIA Nemotron
            "nemotron-3-super".into(),
            "nemotron-3-super:latest".into(),
            "nemotron-3-super:120b".into(),
            "nemotron-3-super:cloud".into(),
            "nemotron-3-nano".into(),
            "nemotron-3-nano:latest".into(),
            "nemotron-3-nano:4b".into(),
            "nemotron-3-nano:30b".into(),
            // Cogito 2.1
            "cogito-2.1".into(),
            "cogito-2.1:latest".into(),
            "cogito-2.1:671b".into(),
            "cogito-2.1:671b-cloud".into(),
            // Gemini 3 Flash
            "gemini-3-flash-preview".into(),
            "gemini-3-flash-preview:latest".into(),
            "gemini-3-flash-preview:cloud".into(),
            // RNJ-1
            "rnj-1".into(),
            "rnj-1:latest".into(),
            "rnj-1:8b".into(),
            "rnj-1:8b-cloud".into(),
        ],
        _ => vec![],
    }
}

/// 根据 provider 获取模型提示文本
fn models_placeholder_key_for(provider: &str) -> &'static str {
    match provider {
        "openai" => "accounts.models_placeholder_openai",
        "claude" => "accounts.models_placeholder_claude",
        "deepseek" => "accounts.models_placeholder_deepseek",
        "gemini" => "accounts.models_placeholder_gemini",
        "vllm" => "accounts.models_placeholder_vllm",
        "ollama" => "accounts.models_placeholder_ollama",
        _ => "accounts.models_placeholder_default",
    }
}

/// 账号管理页面（LLM 渠道配置）
///
/// - 普通用户：无权限提示
/// - Admin：管理 LLM Provider 渠道，支持测试连接、刷新状态
#[component]
pub fn Accounts() -> Element {
    let i18n = use_i18n();
    let user_store = use_context::<UserStore>();
    let is_admin = user_store
        .info
        .read()
        .as_ref()
        .map(|u| u.is_admin())
        .unwrap_or(false);

    if is_admin {
        rsx! { AdminAccountsView {} }
    } else {
        rsx! { NoPermissionView { resource: i18n.t("page.accounts").to_string() } }
    }
}

// ── Admin 视图 ────────────────────────────────────────────────────────

#[component]
fn AdminAccountsView() -> Element {
    let i18n = use_i18n();
    let auth_store = use_context::<AuthStore>();
    let mut ui_store = use_context::<UiStore>();
    let mut show_create = use_signal(|| false);
    let mut create_name = use_signal(String::new);
    let mut create_provider = use_signal(|| "openai".to_string());
    let mut create_api_key = use_signal(String::new);
    let mut create_api_base = use_signal(String::new);
    let mut create_models_input = use_signal(String::new); // 逗号分隔的模型列表
    let mut saving = use_signal(|| false);
    let mut error_msg = use_signal(String::new);
    let mut page = use_signal(|| 1u32);

    // 全局重置健康状态
    let mut resetting = use_signal(|| false);

    // 编辑弹窗状态
    let mut edit_id = use_signal(String::new);
    let mut edit_name = use_signal(String::new);
    let mut edit_api_key = use_signal(String::new);
    let mut edit_api_base = use_signal(String::new);
    let mut edit_is_active = use_signal(|| true);
    let mut show_edit = use_signal(|| false);
    let mut edit_saving = use_signal(|| false);
    let mut edit_error = use_signal(String::new);

    // 删除确认弹窗状态
    let mut delete_id = use_signal(String::new);
    let mut delete_name = use_signal(String::new);
    let mut show_delete = use_signal(|| false);
    let mut deleting = use_signal(|| false);

    let mut accounts = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            account_service::list(None, &token).await
        })
        .await
    });

    // 全局重置健康状态处理函数
    let on_reset_health = move |_| {
        let auth = auth_store.clone();
        let mut ui = ui_store.clone();
        resetting.set(true);
        spawn(async move {
            let token = auth.token().unwrap_or_default();
            match debug_service::reset_health(&token).await {
                Ok(resp) => {
                    ui.show_success(&resp.message);
                }
                Err(e) => {
                    ui.show_error(format!("{}: {}", i18n.t("accounts.reset_failed"), e));
                }
            }
            resetting.set(false);
        });
    };

    let on_submit = move |_| {
        let name = create_name();
        let provider = create_provider();
        let api_key_val = create_api_key();
        let api_base = create_api_base();
        let models_str = create_models_input();
        if name.is_empty() || provider.is_empty() || api_key_val.is_empty() {
            *error_msg.write() = i18n.t("accounts.fill_required").to_string();
            return;
        }
        // 解析模型列表（逗号分隔，去空格，去空项）
        let models: Vec<String> = if models_str.trim().is_empty() {
            // 没有填写时使用 provider 默认模型
            default_models_for(&provider)
        } else {
            models_str
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        let token = auth_store.token().unwrap_or_default();
        *saving.write() = true;
        *error_msg.write() = String::new();
        spawn(async move {
            use client_api::api::admin::CreateAccountRequest;
            let mut req =
                CreateAccountRequest::new(name, provider, api_key_val).with_models(models);
            if !api_base.is_empty() {
                req = req.with_api_base(api_base);
            }
            match account_service::create(req, &token).await {
                Ok(_) => {
                    *show_create.write() = false;
                    create_name.write().clear();
                    *create_provider.write() = "openai".to_string();
                    create_api_key.write().clear();
                    create_api_base.write().clear();
                    create_models_input.write().clear();
                    page.set(1);
                    accounts.restart();
                    ui_store.show_success(i18n.t("accounts.created"));
                }
                Err(e) => {
                    *error_msg.write() = format!("{}: {}", i18n.t("accounts.create_failed"), e);
                }
            }
            *saving.write() = false;
        });
    };

    // 提交编辑
    let on_edit_save = move |_| {
        let id = edit_id();
        let name_val = edit_name();
        let key_val = edit_api_key();
        let base_val = edit_api_base();
        let active = edit_is_active();
        if name_val.trim().is_empty() {
            *edit_error.write() = i18n.t("accounts.name_required").to_string();
            return;
        }
        let token = auth_store.token().unwrap_or_default();
        edit_saving.set(true);
        *edit_error.write() = String::new();
        spawn(async move {
            use client_api::api::admin::UpdateAccountRequest;
            let mut req = UpdateAccountRequest::new()
                .with_name(name_val)
                .with_is_active(active);
            if !key_val.trim().is_empty() {
                req = req.with_api_key(key_val);
            }
            if !base_val.trim().is_empty() {
                req.api_base = Some(base_val);
            }
            match account_service::update(&id, req, &token).await {
                Ok(_) => {
                    show_edit.set(false);
                    accounts.restart();
                    ui_store.show_success(i18n.t("accounts.updated"));
                }
                Err(e) => {
                    *edit_error.write() = format!("{}: {}", i18n.t("accounts.update_failed"), e);
                }
            }
            edit_saving.set(false);
        });
    };

    let reset_health_label = if resetting() {
        i18n.t("accounts.resetting")
    } else {
        i18n.t("accounts.reset_health")
    };
    let create_save_label = if saving() {
        i18n.t("form.saving")
    } else {
        i18n.t("form.save")
    };
    let edit_save_label = if edit_saving() {
        i18n.t("form.saving")
    } else {
        i18n.t("form.save")
    };
    let delete_confirm_label = if deleting() {
        i18n.t("accounts.deleting")
    } else {
        i18n.t("accounts.confirm_delete")
    };

    rsx! {
        div { class: "page-header",
            h1 { class: "page-title", {i18n.t("page.accounts")} }
            p { class: "page-description", {i18n.t("accounts.subtitle")} }
        }

        div { class: "toolbar",
            div { class: "toolbar-left",
                Button {
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::Small,
                    loading: resetting(),
                    onclick: on_reset_health,
                    "{reset_health_label}"
                }
            }
            div { class: "toolbar-right",
                Button {
                    variant: ButtonVariant::Primary,
                    size: ButtonSize::Small,
                    onclick: move |_| *show_create.write() = true,
                    {i18n.t("accounts.add_channel")}
                }
            }
        }

        {
            let (is_empty, empty_text) = match accounts() {
                None => (true, i18n.t("table.loading")),
                Some(Err(_)) => (true, i18n.t("common.load_failed")),
                Some(Ok(ref l)) if l.is_empty() => (true, i18n.t("accounts.empty")),
                _ => (false, ""),
            };
            let total = accounts().and_then(|r| r.ok()).map(|l| l.len()).unwrap_or(0);
            let total_pages = total.div_ceil(PAGE_SIZE).max(1) as u32;
            let start = (page() as usize - 1) * PAGE_SIZE;
            let paged_list: Vec<_> = accounts()
                .and_then(|r| r.ok())
                .map(|l| l.into_iter().skip(start).take(PAGE_SIZE).collect())
                .unwrap_or_default();
            rsx! {
                div { class: "accounts-table-shell",
                    div { class: "accounts-table-intro",
                        div {
                            h2 { class: "accounts-table-title", {i18n.t("accounts.table_title")} }
                            p { class: "accounts-table-subtitle", {i18n.t("accounts.table_subtitle")} }
                        }
                        div { class: "accounts-table-meta", "{i18n.t(\"common.total_items\")} {total} {i18n.t(\"accounts.channels_suffix\")}" }
                    }
                    Table {
                        class: "accounts-table".to_string(),
                        empty: is_empty,
                        empty_text: empty_text.to_string(),
                        col_count: 6,
                        thead {
                            tr {
                                TableHead { {i18n.t("accounts.channel")} }
                                TableHead { {i18n.t("accounts.provider_model")} }
                                TableHead { {i18n.t("accounts.runtime_status")} }
                                TableHead { {i18n.t("accounts.rate_quota")} }
                                TableHead { {i18n.t("common.time")} }
                                TableHead { {i18n.t("table.actions")} }
                            }
                        }
                        tbody {
                            if accounts().and_then(|r| r.ok()).is_some() {
                                for acc in paged_list.iter() {
                                    tr {
                                        td {
                                            div { class: "account-cell-main",
                                                div { class: "account-name-row",
                                                    span { class: "account-name", "{acc.name}" }
                                                    span { class: "account-id", "#{acc.id.chars().take(8).collect::<String>()}" }
                                                }
                                                div { class: "account-subline",
                                                    span { class: "account-secret-label", {i18n.t("accounts.key_preview")} }
                                                    code { class: "account-key-preview", "{acc.api_key_preview}" }
                                                }
                                                if let Some(api_base) = &acc.api_base {
                                                    p { class: "account-endpoint", "{api_base}" }
                                                } else {
                                                    p { class: "account-endpoint account-endpoint-muted", {i18n.t("accounts.default_endpoint")} }
                                                }
                                            }
                                        }
                                        td {
                                            div { class: "account-provider-cell",
                                                span {
                                                    class: "account-provider-badge account-provider-{acc.provider}",
                                                    "{provider_label(&acc.provider)}"
                                                }
                                                p { class: "account-provider-code", "{acc.provider}" }
                                                div { class: "account-models",
                                                    if acc.models.is_empty() {
                                                        span { class: "account-model-chip account-model-chip-muted", {i18n.t("accounts.no_models")} }
                                                    } else {
                                                        for model in acc.models.iter().take(2) {
                                                            span { class: "account-model-chip", "{model}" }
                                                        }
                                                        if acc.models.len() > 2 {
                                                            span { class: "account-model-chip account-model-chip-muted", "+{acc.models.len() - 2}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        td {
                                            div { class: "account-status-stack",
                                                div { class: "account-status-row",
                                                    if acc.is_active {
                                                        Badge { variant: BadgeVariant::Success, {i18n.t("common.enabled")} }
                                                    } else {
                                                        Badge { variant: BadgeVariant::Neutral, {i18n.t("common.disabled")} }
                                                    }
                                                    if acc.is_healthy {
                                                        Badge { variant: BadgeVariant::Success, {i18n.t("system.healthy")} }
                                                    } else {
                                                        Badge { variant: BadgeVariant::Warning, {i18n.t("dashboard.pending_check")} }
                                                    }
                                                }
                                                p { class: "account-status-note",
                                                    if acc.is_active && acc.is_healthy {
                                                        {i18n.t("accounts.route_ready")}
                                                    } else if acc.is_active {
                                                        {i18n.t("accounts.enabled_but_unhealthy")}
                                                    } else {
                                                        {i18n.t("accounts.not_routed")}
                                                    }
                                                }
                                            }
                                        }
                                        td {
                                            div { class: "account-rpm-cell",
                                                div { class: "account-rpm-metric",
                                                    span { class: "account-rpm-current", "{acc.current_rpm}" }
                                                    span { class: "account-rpm-divider", "/" }
                                                    span { class: "account-rpm-limit", "{acc.rpm_limit}" }
                                                }
                                                p { class: "account-rpm-label", {i18n.t("accounts.rpm_label")} }
                                            }
                                        }
                                        td {
                                            div { class: "account-time-cell",
                                                div { class: "account-time-block",
                                                    span { class: "account-time-label", {i18n.t("common.created_at_label")} }
                                                    span { class: "account-time-value", { format_time(&acc.created_at) } }
                                                }
                                                div { class: "account-time-block",
                                                    span { class: "account-time-label", {i18n.t("accounts.last_used")} }
                                                    span { class: "account-time-value",
                                                        {
                                                            if let Some(last_used) = &acc.last_used_at {
                                                                format_time(last_used)
                                                            } else {
                                                                i18n.t("accounts.no_usage_record").to_string()
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        td {
                                            div { class: "accounts-actions",
                                                Button {
                                                    variant: ButtonVariant::Ghost,
                                                    size: ButtonSize::Small,
                                                    onclick: {
                                                        let id = acc.id.clone();
                                                        let name = acc.name.clone();
                                                        let active = acc.is_active;
                                                        move |_| {
                                                            edit_id.set(id.clone());
                                                            edit_name.set(name.clone());
                                                            edit_api_key.set(String::new());
                                                            edit_api_base.set(String::new());
                                                            edit_is_active.set(active);
                                                            *edit_error.write() = String::new();
                                                            show_edit.set(true);
                                                        }
                                                    },
                                                    {i18n.t("form.edit")}
                                                }
                                                Button {
                                                    variant: ButtonVariant::Ghost,
                                                    size: ButtonSize::Small,
                                                    onclick: {
                                                        let id = acc.id.clone();
                                                        move |_| {
                                                            let token = auth_store.token().unwrap_or_default();
                                                            let id = id.clone();
                                                            spawn(async move {
                                                                match account_service::test(&id, &token).await {
                                                                    Ok(_) => ui_store.show_success(i18n.t("accounts.test_success")),
                                                                    Err(e) => ui_store.show_error(format!("{}: {}", i18n.t("accounts.test_failed"), e)),
                                                                }
                                                                accounts.restart();
                                                            });
                                                        }
                                                    },
                                                    {i18n.t("accounts.test")}
                                                }
                                                Button {
                                                    variant: ButtonVariant::Danger,
                                                    size: ButtonSize::Small,
                                                    onclick: {
                                                        let id = acc.id.clone();
                                                        let name = acc.name.clone();
                                                        move |_| {
                                                            delete_id.set(id.clone());
                                                            delete_name.set(name.clone());
                                                            show_delete.set(true);
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
                    }
                    div { class: "pagination",
                        span { class: "pagination-info", "{i18n.t(\"common.total_items\")} {total} {i18n.t(\"pricing.items_suffix\")}" }
                        Pagination {
                            current: page(),
                            total_pages,
                            on_page_change: move |p| page.set(p),
                        }
                    }
                }
            }
        }

        // 新增渠道弹窗
        if show_create() {
            div { class: "modal-backdrop",
                onclick: move |_| *show_create.write() = false,
                div {
                    class: "modal",
                    onclick: move |e| e.stop_propagation(),
                    div { class: "modal-header",
                        h2 { class: "modal-title", {i18n.t("accounts.create_title")} }
                        button {
                            class: "btn btn-ghost btn-sm",
                            r#type: "button",
                            onclick: move |_| *show_create.write() = false,
                            "✕"
                        }
                    }
                    div { class: "modal-body",
                        if !error_msg().is_empty() {
                            div { class: "alert alert-error",
                                span { "{error_msg}" }
                            }
                        }
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("accounts.channel_name")} }
                            input {
                                class: "input-field",
                                placeholder: "{i18n.t(\"accounts.channel_name_placeholder\")}",
                                value: "{create_name}",
                                oninput: move |e| *create_name.write() = e.value(),
                            }
                        }
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("accounts.provider")} }
                            select {
                                class: "input-field",
                                value: "{create_provider}",
                                onchange: move |e| {
                                    *create_provider.write() = e.value();
                                    // 切换 provider 时自动填充默认模型
                                    *create_models_input.write() = default_models_for(&e.value()).join(", ");
                                },
                                for (value, label) in PROVIDERS {
                                    option {
                                        value: "{value}",
                                        selected: *value == create_provider(),
                                        "{label}"
                                    }
                                }
                            }
                        }
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("accounts.supported_models")} }
                            input {
                                class: "input-field",
                                placeholder: "{i18n.t(models_placeholder_key_for(&create_provider()))}",
                                value: "{create_models_input}",
                                oninput: move |e| *create_models_input.write() = e.value(),
                            }
                            small { class: "form-hint", {i18n.t("accounts.models_hint")} }
                        }
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("accounts.api_key")} }
                            input {
                                class: "input-field",
                                r#type: "password",
                                placeholder: "sk-...",
                                value: "{create_api_key}",
                                oninput: move |e| *create_api_key.write() = e.value(),
                            }
                        }
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("accounts.custom_base_url")} }
                            input {
                                class: "input-field",
                                placeholder: "https://api.openai.com/v1",
                                value: "{create_api_base}",
                                oninput: move |e| *create_api_base.write() = e.value(),
                            }
                        }
                    }
                    div { class: "modal-footer",
                        Button {
                            variant: ButtonVariant::Ghost,
                            onclick: move |_| *show_create.write() = false,
                            {i18n.t("form.cancel")}
                        }
                        Button {
                            variant: ButtonVariant::Primary,
                            loading: saving(),
                            onclick: on_submit,
                            "{create_save_label}"
                        }
                    }
                }
            }
        }
        // 编辑渠道弹窗
        if show_edit() {
            div { class: "modal-backdrop",
                onclick: move |_| show_edit.set(false),
                div {
                    class: "modal",
                    onclick: move |e| e.stop_propagation(),
                    div { class: "modal-header",
                        h2 { class: "modal-title", {i18n.t("accounts.edit_title")} }
                        button {
                            class: "btn btn-ghost btn-sm",
                            r#type: "button",
                            onclick: move |_| show_edit.set(false),
                            "✕"
                        }
                    }
                    div { class: "modal-body",
                        if !edit_error().is_empty() {
                            div { class: "alert alert-error",
                                span { "{edit_error}" }
                            }
                        }
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("accounts.channel_name")} }
                            input {
                                class: "input-field",
                                value: "{edit_name}",
                                oninput: move |e| *edit_name.write() = e.value(),
                            }
                        }
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("accounts.new_api_key")} }
                            input {
                                class: "input-field",
                                r#type: "password",
                                placeholder: "{i18n.t(\"accounts.new_api_key_placeholder\")}",
                                value: "{edit_api_key}",
                                oninput: move |e| *edit_api_key.write() = e.value(),
                            }
                        }
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("accounts.custom_base_url_optional")} }
                            input {
                                class: "input-field",
                                placeholder: "https://api.openai.com/v1",
                                value: "{edit_api_base}",
                                oninput: move |e| *edit_api_base.write() = e.value(),
                            }
                        }
                        div { class: "form-group",
                            label { class: "form-label",
                                input {
                                    r#type: "checkbox",
                                    checked: edit_is_active(),
                                    onchange: move |e| edit_is_active.set(e.checked()),
                                    style: "margin-right:6px",
                                }
                                {i18n.t("accounts.enable_channel")}
                            }
                        }
                    }
                    div { class: "modal-footer",
                        Button {
                            variant: ButtonVariant::Ghost,
                            onclick: move |_| show_edit.set(false),
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

        // 删除确认弹窗
        if show_delete() {
            div { class: "modal-backdrop",
                onclick: move |_| show_delete.set(false),
                div {
                    class: "modal",
                    onclick: move |e| e.stop_propagation(),
                    div { class: "modal-header",
                        h2 { class: "modal-title", {i18n.t("accounts.delete_confirm_title")} }
                        button {
                            class: "btn btn-ghost btn-sm",
                            r#type: "button",
                            onclick: move |_| show_delete.set(false),
                            "✕"
                        }
                    }
                    div { class: "modal-body",
                        p {
                            "{i18n.t(\"accounts.delete_confirm_prefix\")}"
                            strong { "{delete_name}" }
                            "{i18n.t(\"accounts.delete_confirm_suffix\")}"
                        }
                    }
                    div { class: "modal-footer",
                        Button {
                            variant: ButtonVariant::Ghost,
                            onclick: move |_| show_delete.set(false),
                            {i18n.t("form.cancel")}
                        }
                        Button {
                            variant: ButtonVariant::Danger,
                            loading: deleting(),
                            onclick: move |_| {
                                let id = delete_id();
                                let token = auth_store.token().unwrap_or_default();
                                deleting.set(true);
                                spawn(async move {
                                    match account_service::delete(&id, &token).await {
                                        Ok(_) => {
                                            ui_store.show_success(i18n.t("accounts.deleted"));
                                            accounts.restart();
                                        }
                                        Err(e) => {
                                            ui_store.show_error(format!("{}: {}", i18n.t("accounts.delete_failed"), e));
                                        }
                                    }
                                    deleting.set(false);
                                    show_delete.set(false);
                                });
                            },
                            "{delete_confirm_label}"
                        }
                    }
                }
            }
        }
    }
}

// ── 无权限视图（共用组件）──────────────────────────────────────────────

#[component]
pub fn NoPermissionView(resource: String) -> Element {
    let i18n = use_i18n();
    let no_permission_desc = i18n
        .t("accounts.no_permission_desc")
        .replace("{resource}", &resource);
    rsx! {
        div { class: "page-header",
            h1 { class: "page-title", "{resource}" }
        }
        div { class: "empty-state",
            div { class: "empty-icon", "🔒" }
            h3 { class: "empty-title", {i18n.t("accounts.no_permission_title")} }
            p { class: "empty-description",
                "{no_permission_desc}"
            }
        }
    }
}
