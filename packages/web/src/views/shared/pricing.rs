mod authority;
use authority::PricingIdentity;

#[dioxus::prelude::component]
pub fn Pricing() -> dioxus::prelude::Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let identity = PricingIdentity::from_stores(auth, users);
    rsx! {if let Some(identity)=identity {for key in [format!("{identity:?}")]{PricingWorkspace{key:"{key}",identity}}}
    else {p {role:"alert",{i18n.t("common.admin_only_page")}}}}
}
#[component]
fn PricingWorkspace(identity: PricingIdentity) -> Element {
    let i18n = use_i18n();
    let mut target = use_signal(|| PricingTarget::Platform);
    let mut mode = use_signal(|| "platform".to_string());
    let mut tenant = use_signal(String::new);
    let mut error = use_signal(String::new);
    rsx! {div {class:"platform-pricing-workspace",
        section {class:"page-container",aria_label:i18n.t("platform_pricing.target"),
            label {class:"form-label",r#for:"platform-pricing-scope",{i18n.t("platform_pricing.target")}}
            select {id:"platform-pricing-scope",class:"input-field",value:"{mode}",onchange:move|e|mode.set(e.value()),
                option {value:"platform",{i18n.t("platform_pricing.global")}} option {value:"tenant",{i18n.t("platform_pricing.tenant")}}
            }
            if mode()=="tenant" {label {class:"form-label",r#for:"platform-pricing-tenant",{i18n.t("pricing.tenant_id")}}
                input {id:"platform-pricing-tenant",class:"input-field",value:"{tenant}",maxlength:"36",oninput:move|e|tenant.set(e.value())}}
            button {class:"btn btn-secondary",onclick:move |_|match authority::parse_target(&mode(),&tenant()) {
                Ok(next)=>{target.set(next);error.set(String::new());},Err(_)=>error.set(i18n.t("platform_pricing.invalid_target").into())
            },{i18n.t("platform_pricing.apply")}}
            if !error().is_empty(){p {class:"alert alert-error",role:"alert","{error}"}}
            p {class:"text-secondary",{i18n.t("platform_pricing.explicit_scope")}}
        }
        for key in [format!("{:?}",target())]{PricingRecords{key:"{key}",identity,target:target()}}
    }}
}

use client_api::api::admin::{CreatePricingRequest, PricingQueryParams, PricingTarget};
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use ui::{Badge, BadgeVariant, ConfirmModal, PageHeader, Pagination, Table, TableHead};

const PAGE_SIZE: usize = 20;
const SEARCH_DEBOUNCE_MS: u32 = 300;

#[derive(Clone, Debug, Eq, PartialEq)]
struct PricingListQuery {
    search: String,
    page: u32,
    page_size: u32,
}

impl Default for PricingListQuery {
    fn default() -> Self {
        Self {
            search: String::new(),
            page: 1,
            page_size: PAGE_SIZE as u32,
        }
    }
}

impl PricingListQuery {
    fn reset_page(&mut self) {
        self.page = 1;
    }

    fn commit_search(&mut self, search: String) {
        self.search = search;
        self.reset_page();
    }

    fn set_page_size(&mut self, page_size: u32) {
        self.page_size = page_size;
        self.reset_page();
    }
}

use crate::hooks::use_i18n::use_i18n;
use crate::i18n::I18n;
use crate::services::{
    api_client::with_auto_refresh,
    pricing_service::{self, is_platform_price},
};
use crate::stores::auth_store::AuthStore;
use crate::stores::user_store::UserStore;
use crate::utils::resource::{KeyedResourceValue, current_keyed_value};
use crate::utils::time::format_time;

fn pricing_provider_label<'a>(dimension: &'a str, i18n: &I18n) -> &'a str {
    match dimension {
        "provideraccount" => i18n.t("pricing.label_provider_account"),
        "node" => i18n.t("pricing.label_node"),
        _ => dimension,
    }
}

/// 获取计费维度的 CSS 类名（短格式）
fn pricing_provider_class(dimension: &str) -> &'static str {
    match dimension {
        "provideraccount" => "provider-pa",
        "node" => "provider-node",
        _ => "provider-unknown",
    }
}

fn pricing_col_count(can_manage_platform: bool) -> u32 {
    if can_manage_platform { 7 } else { 6 }
}

/// Explicit root-managed pricing records for one chosen target.
#[component]
fn PricingRecords(target: PricingTarget, identity: authority::PricingIdentity) -> Element {
    let i18n = use_i18n();
    let user_store = use_context::<UserStore>();
    let auth_store = use_context::<AuthStore>();
    let can_manage_platform = user_store
        .info
        .read()
        .as_ref()
        .map(|u| u.can_manage_platform())
        .unwrap_or(false);

    // 控制创建弹窗
    let mut show_create = use_signal(|| false);
    let mut mutating = use_signal(|| false);
    let mut editing_pricing = use_signal(|| None as Option<client_api::api::admin::PricingInfo>);
    let mut delete_candidate = use_signal(|| Option::<(String, String)>::None);
    let mut delete_modal_open = use_signal(|| false);
    // 操作结果提示
    let mut op_ok = use_signal(String::new);
    let mut op_err = use_signal(String::new);
    // 刷新触发器
    let mut refresh_tick = use_signal(|| 0u32);
    let mut search = use_signal(String::new);
    let mut query = use_signal(PricingListQuery::default);

    use_effect(move || {
        let next_search = search();
        spawn(async move {
            TimeoutFuture::new(SEARCH_DEBOUNCE_MS).await;
            if search() == next_search && query.read().search != next_search {
                query.write().commit_search(next_search);
            }
        });
    });

    let pricing_list = use_resource(move || {
        let refresh_revision = refresh_tick();
        let current_query = query();
        async move {
            let request_key = (current_query.clone(), refresh_revision);
            let mut params = PricingQueryParams::new(target)
                .with_page(current_query.page as u64)
                .with_page_size(current_query.page_size as u64);
            if !current_query.search.is_empty() {
                params = params.with_search(current_query.search);
            }
            let result = with_auto_refresh(auth_store, move |token| {
                let params = params.clone();
                async move { pricing_service::list_page(&params, &token).await }
            })
            .await;
            let result = if identity.is_current(auth_store, user_store) {
                result
            } else {
                Err(client_api::ClientError::Other(
                    "Pricing workspace changed".into(),
                ))
            };
            KeyedResourceValue::new(request_key, result)
        }
    });
    let page_description = if can_manage_platform {
        i18n.t("pricing.admin_desc")
    } else {
        i18n.t("pricing.user_desc")
    };

    let col_count = pricing_col_count(can_manage_platform);

    rsx! {
        div { class: "page-container platform-pricing-records",
            p {class:"platform-pricing-current",{i18n.t("platform_pricing.current")} " " {authority::target_label(target,&i18n)}}
            p {class:"text-secondary",{i18n.t("platform_pricing.command_hint")}}
            PageHeader {
                title: i18n.t("page.pricing").to_string(),
                description: page_description.to_string(),
                actions: rsx! {
                    if can_manage_platform {
                        button {
                            class: "btn btn-primary",
                            onclick: move |_| show_create.set(true),
                            {i18n.t("pricing.create")}
                        }
                    }
                },
            }

            // 操作结果提示
            if !op_ok().is_empty() {
                div { class: "alert alert-success", "{op_ok}" }
            }
            if !op_err().is_empty() {
                div { class: "alert alert-error", "{op_err}" }
            }

            div { class: "toolbar",
                div { class: "toolbar-left",
                    div { class: "input-wrapper",
                        input {
                            class: "input-field",
                            r#type: "search",
                            placeholder: "{i18n.t(\"pricing.search_placeholder\")}",
                            value: "{search}",
                            oninput: move |e| search.set(e.value()),
                        }
                    }
                }
            }

            {
                let current_query = query();
                let current_key = (current_query.clone(), refresh_tick());
                let result = current_keyed_value(
                    &current_key,
                    pricing_list.state().cloned(),
                    pricing_list(),
                );
                let load_error = result.as_ref().and_then(|r|r.as_ref().err()).map(crate::services::api_client::user_error_message);
                let (is_empty, empty_text) = match &result {
                    None => (true, i18n.t("table.loading")),
                    Some(Err(_)) => (true, i18n.t("common.load_failed")),
                    Some(Ok(result)) if result.pricing.is_empty() => (true, i18n.t("pricing.empty")),
                    _ => (false, ""),
                };
                let total = result
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .map(|result| result.total)
                    .unwrap_or(0);
                let total_pages = result
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .map(|result| result.total_pages.max(1) as u32)
                    .unwrap_or(1);
                let paged_list = result
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .map(|result| result.pricing.as_slice())
                    .unwrap_or_default();
                rsx! {
                    if let Some(error)=load_error {p {class:"alert alert-error",role:"alert","{error}"}}
                    div { class: "pricing-table-shell table-pagination-panel",
                        div { class: "pricing-table-intro",
                            div {
                                h2 { class: "pricing-table-title", {i18n.t("pricing.table_title")} }
                                p { class: "pricing-table-subtitle", {i18n.t("pricing.table_subtitle")} }
                            }
                            div { class: "pricing-table-meta",
                                "{i18n.t(\"common.total_items\")} {total} {i18n.t(\"pricing.items_suffix\")}"
                            }
                        }
                        Table {
                            class: "pricing-table".to_string(),
                            empty: is_empty,
                            empty_text: empty_text.to_string(),
                            col_count,
                            thead {
                                tr {
                                    TableHead { {i18n.t("pricing.model_provider")} }
                                    TableHead { {i18n.t("pricing.tenant_id")} }
                                    TableHead { {i18n.t("pricing.input_price")} }
                                    TableHead { {i18n.t("pricing.output_price")} }
                                    TableHead { {i18n.t("pricing.billing_status")} }
                                    TableHead { {i18n.t("common.time")} }
                                    if can_manage_platform {
                                        TableHead { {i18n.t("table.actions")} }
                                    }
                                }
                            }
                            tbody {

                                for p in paged_list.iter() {
                                        tr { key: "{p.id}",
                                            td {
                                                div { class: "pricing-model-cell",
                                                    div { class: "pricing-model-row",
                                                        span { class: "pricing-model-name", "{p.model_name}" }
                                                        span { class: "pricing-model-id",
                                                            "#{p.id.chars().take(8).collect::<String>()}"
                                                        }
                                                    }
                                                    div { class: "pricing-provider-row",
                                                        span { class: "pricing-provider-badge {pricing_provider_class(&p.billing_dimension)}",
                                                            "{pricing_provider_label(&p.billing_dimension, &i18n)}"
                                                        }
                                                        span { class: "pricing-provider-code", "{p.billing_dimension}" }
                                                    }
                                                }
                                            }
                                            td {
                                                if is_platform_price(p) {
                                                    span {class:"pricing-tenant-global",{i18n.t("platform_pricing.global")}}
                                                } else if let Some(tenant_id)=&p.tenant_id {
                                                    span {class:"pricing-tenant-code",title:tenant_id.clone(),"{tenant_id}"}
                                                }

                                            }
                                            td {
                                                div { class: "pricing-amount-cell",
                                                    div { class: "pricing-amount-value", "{p.input_price_per_1k}" }
                                                    div { class: "pricing-amount-meta",
                                                        "{p.currency} / 1K {i18n.t(\"pricing.input_tokens\")}"
                                                    }
                                                }
                                            }
                                            td {
                                                div { class: "pricing-amount-cell",
                                                    div { class: "pricing-amount-value", "{p.output_price_per_1k}" }
                                                    div { class: "pricing-amount-meta",
                                                        "{p.currency} / 1K {i18n.t(\"pricing.output_tokens\")}"
                                                    }
                                                }
                                            }
                                            td {
                                                div { class: "pricing-status-cell", // 全局默认定价不允许删除，仅保留编辑按钮
                                                    if p.is_default {
                                                        Badge { variant: BadgeVariant::Success, {i18n.t("pricing.default")} }
                                                    } else {
                                                        Badge { variant: BadgeVariant::Neutral,
                                                            {i18n.t("pricing.alternative")}
                                                        }
                                                    }
                                                    p { class: "pricing-status-note",
                                                        if p.is_default {
                                                            {i18n.t("pricing.default_note")}
                                                        } else {
                                                            {i18n.t("pricing.alternative_note")}
                                                        }
                                                    }
                                                }
                                            }
                                            td {
                                                div { class: "pricing-time-cell",
                                                    span { class: "pricing-time-label",
                                                        {i18n.t("common.created_at_label")}
                                                    }
                                                    span { class: "pricing-time-value", {format_time(&p.created_at)} }
                                                }
                                            }
                                            if can_manage_platform {
                                                td {
                                                    div { class: "action-buttons pricing-actions",
                                                        if !p.is_default {
                                                            {
                                                                let pid = p.id.clone();
                                                                rsx! {
                                                                    button {
                                                                        class: "btn btn-sm btn-secondary",
                                                                        onclick: move |_| {
                                                                            if mutating() || !identity.is_current(auth_store,user_store){return;}
                                                                            mutating.set(true);
                                                                            let id = pid.clone();
                                                                            let token = auth_store.token().unwrap_or_default();
                                                                            spawn(async move {
                                                                                let result=pricing_service::make_default(target,&id,&token).await;
                                                                                if !identity.is_current(auth_store,user_store){return;}
                                                                                mutating.set(false);
                                                                                match result {
                                                                                    Ok(_) => {
                                                                                        op_ok.set(i18n.t("pricing.set_default_ok").to_string());
                                                                                        op_err.set(String::new());
                                                                                        *refresh_tick.write() += 1;
                                                                                        spawn(async move {
                                                                                            gloo_timers::future::TimeoutFuture::new(3_000).await;
                                                                                            op_ok.set(String::new());
                                                                                        });
                                                                                    }
                                                                                    Err(e) => {
                                                                                        op_err
                                                                                            .set(format!("{}：{e}", i18n.t("pricing.set_default_failed")));
                                                                                        spawn(async move {
                                                                                            gloo_timers::future::TimeoutFuture::new(3_000).await;
                                                                                            op_err.set(String::new());
                                                                                        });
                                                                                    }
                                                                                }
                                                                            });
                                                                        },
                                                                        {i18n.t("pricing.set_default")}
                                                                    }
                                                                }
                                                            }
                                                        }
                                                        // 编辑按钮：所有定价都可以编辑
                                                        {
                                                            let pricing = p.clone();
                                                            rsx! {
                                                                button {
                                                                    class: "btn btn-sm btn-secondary",
                                                                    onclick: move |_| editing_pricing.set(Some(pricing.clone())),
                                                                    {i18n.t("form.edit")}
                                                                }
                                                            }
                                                        }
                                                        // 全局默认定价不允许删除，仅保留编辑按钮
                                                        if !is_platform_price(p) {
                                                            {
                                                                let pid = p.id.clone();
                                                                let model_name = p.model_name.clone();
                                                                rsx! {
                                                                    button {
                                                                        class: "btn btn-sm btn-danger",
                                                                        onclick: move |_| {
                                                                            delete_candidate.set(Some((pid.clone(), model_name.clone())));
                                                                            delete_modal_open.set(true);
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
                    }
                    Pagination {
                        current: current_query.page,
                        total_pages,
                        total,
                        page_size: current_query.page_size,
                        summary: i18n.t_with_args(
                            "common.pagination_summary",
                            &[
                                ("total", &total.to_string()),
                                ("current", &current_query.page.to_string()),
                                ("total_pages", &total_pages.to_string()),
                            ],
                        ),
                        page_size_label: i18n.t("common.pagination_page_size").to_string(),
                        page_size_suffix: i18n.t("common.items_suffix").to_string(),
                        previous_label: i18n.t("table.previous").to_string(),
                        next_label: i18n.t("table.next").to_string(),
                        on_page_change: move |p| query.write().page = p,
                        on_page_size_change: move |size| query.write().set_page_size(size),
                    }
                }
            }

            // 创建定价弹窗
            if show_create() {
                CreatePricingModal {
                    auth_store, target, identity,
                    on_close: move |_| show_create.set(false),
                    on_created: move |_| {
                        show_create.set(false);
                        op_ok.set(i18n.t("pricing.created").to_string());
                        op_err.set(String::new());
                        query.write().reset_page();
                        *refresh_tick.write() += 1;
                        spawn(async move {
                            gloo_timers::future::TimeoutFuture::new(3_000).await;
                            op_ok.set(String::new());
                        });
                    },
                }
            }

            if let Some(pricing) = editing_pricing() {
                for key in [format!("{}:{}",pricing.id,pricing.version)] { EditPricingModal {
                    key:"{key}",auth_store, target, identity,
                    pricing_id: pricing.id.clone(),
                    pricing_model: pricing.model_name.clone(),
                    pricing_provider: pricing.billing_dimension.clone(),
                    pricing_currency: pricing.currency.clone(),
                    initial_input_price: pricing.input_price_per_1k.clone(),
                    initial_output_price: pricing.output_price_per_1k.clone(),
                    pricing_version: pricing.version,
                    on_close: move |_| editing_pricing.set(None),
                    on_updated: move |_| {
                        editing_pricing.set(None);
                        op_ok.set(i18n.t("pricing.updated").to_string());
                        op_err.set(String::new());
                        query.write().reset_page();
                        *refresh_tick.write() += 1;
                        spawn(async move {
                            gloo_timers::future::TimeoutFuture::new(3_000).await;
                            op_ok.set(String::new());
                        });
                    },
                }
            }}

            ConfirmModal {
                open: delete_modal_open,
                title: i18n.t("pricing.delete_confirm_title").to_string(),
                message: delete_candidate()
                    .as_ref()
                    .map(|(_, model)| i18n.t_with_args("pricing.delete_confirm_message", &[("model", model)]))
                    .unwrap_or_default(),
                confirm_text: i18n.t("form.delete").to_string(),
                cancel_text: i18n.t("form.cancel").to_string(),
                close_label: i18n.t("common.close").to_string(),
                danger: true,
                onconfirm: move |_| {
                    if mutating() || !identity.is_current(auth_store,user_store){return;}
                    let candidate = delete_candidate();
                    delete_candidate.set(None);
                    delete_modal_open.set(false);
                    if let Some((id, _)) = candidate {
                        mutating.set(true);
                        let token = auth_store.token().unwrap_or_default();
                        spawn(async move {
                            let result=pricing_service::delete(target,&id,&token).await;
                            if !identity.is_current(auth_store,user_store){return;}
                            mutating.set(false);
                            match result {
                                Ok(_) => {
                                    op_ok.set(i18n.t("pricing.deleted").to_string());
                                    op_err.set(String::new());
                                    query.write().reset_page();
                                    *refresh_tick.write() += 1;
                                    spawn(async move {
                                        gloo_timers::future::TimeoutFuture::new(3_000).await;
                                        op_ok.set(String::new());
                                    });
                                }
                                Err(e) => {
                                    op_err.set(format!("{}：{e}", i18n.t("pricing.delete_failed")));
                                }
                            }
                        });
                    }
                },
                oncancel: move |_| {
                    delete_candidate.set(None);
                    delete_modal_open.set(false);
                },
            }
        }
    }
}

/// 创建定价弹窗
#[component]
fn CreatePricingModal(
    auth_store: AuthStore,
    target: PricingTarget,
    identity: authority::PricingIdentity,
    on_close: EventHandler,
    on_created: EventHandler,
) -> Element {
    let i18n = use_i18n();
    let mut model = use_signal(String::new);
    let mut provider = use_signal(|| "provideraccount".to_string());
    let user_store = use_context::<UserStore>();
    let mut input_price = use_signal(String::new);
    let mut output_price = use_signal(String::new);
    let mut currency = use_signal(|| "CNY".to_string());
    let mut saving = use_signal(|| false);
    let mut form_err = use_signal(String::new);

    let on_submit = move |_| {
        if saving() || !identity.is_current(auth_store, user_store) {
            return;
        }
        let m = model().trim().to_owned();
        let p = provider();
        let ip_str = input_price().trim().to_owned();
        let op_str = output_price().trim().to_owned();
        let cur = currency();
        if m.is_empty() || p.is_empty() || ip_str.is_empty() || op_str.is_empty() {
            form_err.set(i18n.t("pricing.fill_all").to_string());
            return;
        }
        // Keep the exact model ID. The billing dimension, not a name prefix,
        // selects which execution mode this price belongs to.
        if client_api::api::tenant_pricing::validate_decimal(&ip_str).is_err()
            || client_api::api::tenant_pricing::validate_decimal(&op_str).is_err()
        {
            form_err.set(i18n.t("platform_pricing.invalid_amount").into());
            return;
        }
        saving.set(true);
        form_err.set(String::new());
        let token = auth_store.token().unwrap_or_default();
        spawn(async move {
            let req = CreatePricingRequest {
                model_name: m,
                billing_dimension: p,
                target,
                input_price_per_1k: ip_str,
                output_price_per_1k: op_str,
                currency: cur,
                is_default: false,
                effective_from: None,
                effective_until: None,
            };
            let result = pricing_service::create(req, &token).await;
            if !identity.is_current(auth_store, user_store) {
                return;
            }
            match result {
                Ok(_) => {
                    saving.set(false);
                    on_created.call(());
                }
                Err(e) => {
                    form_err.set(format!("{}：{e}", i18n.t("pricing.create_failed")));
                    saving.set(false);
                }
            }
        });
    };

    rsx! {
        div { class: "modal-backdrop", onclick: move |_| on_close.call(()),
            div { class: "modal", role: "dialog", aria_modal: "true", aria_label: i18n.t("pricing.create_title"), onclick: move |e| e.stop_propagation(),
                div { class: "modal-header",
                    h2 { class: "modal-title", {i18n.t("pricing.create_title")} }
                    button {
                        class: "btn btn-ghost btn-sm",
                        r#type: "button",
                        aria_label: i18n.t("common.close"),
                        onclick: move |_| on_close.call(()),
                        "✕"
                    }
                }
                div { class: "modal-body",
                    if !form_err().is_empty() {
                        div { class: "alert alert-error", "{form_err}" }
                    }
                    div { class: "form-group",
                        label { class: "form-label", {i18n.t("pricing.model_name")} }
                        input {
                            class: "input-field",
                            r#type: "text",
                            placeholder: "{i18n.t(\"pricing.model_placeholder\")}",
                            id: "platform-price-model",
                            value: "{model}",
                            oninput: move |e| model.set(e.value()),
                        }
                    }
                    div { class: "form-group",
                        label { class: "form-label", {i18n.t("pricing.provider_type")} }
                        select {
                            class: "input-field",
                            value: "{provider}",
                            onchange: move |e| provider.set(e.value()),
                            option { value: "provideraccount",
                                {i18n.t("pricing.label_provider_account")}
                            }
                            option { value: "node", {i18n.t("pricing.label_node")} }
                        }
                    }
                    div { class: "form-group",
                        label { class: "form-label", {i18n.t("pricing.tenant_id")} }
                        p {class:"platform-pricing-target",{authority::target_label(target,&i18n)}}
                    }
                    div { class: "form-group",
                        label { class: "form-label", {i18n.t("pricing.input_price_label")} }
                        input {
                            class: "input-field",
                            r#type: "text",
                            inputmode: "decimal",
                            placeholder: "{i18n.t(\"pricing.input_placeholder\")}",
                            id: "platform-price-input",
                            maxlength: "64",
                            value: "{input_price}",
                            oninput: move |e| input_price.set(e.value()),
                        }
                    }
                    div { class: "form-group",
                        label { class: "form-label", {i18n.t("pricing.output_price_label")} }
                        input {
                            class: "input-field",
                            r#type: "text",
                            inputmode: "decimal",
                            placeholder: "{i18n.t(\"pricing.output_placeholder\")}",
                            id: "platform-price-output",
                            maxlength: "64",
                            value: "{output_price}",
                            oninput: move |e| output_price.set(e.value()),
                        }
                    }
                    div { class: "form-group",
                        label { class: "form-label", {i18n.t("common.currency")} }
                        select {
                            class: "input-field",
                            value: "{currency}",
                            onchange: move |e| currency.set(e.value()),
                            option { value: "CNY", {i18n.t("pricing.currency_cny")} }
                            option { value: "USD", {i18n.t("pricing.currency_usd")} }
                        }
                    }
                }
                div { class: "modal-footer",
                    button {
                        class: "btn btn-ghost",
                        r#type: "button",
                        onclick: move |_| on_close.call(()),
                        {i18n.t("form.cancel")}
                    }
                    button {
                        class: "btn btn-primary",
                        r#type: "button",
                        disabled: saving(),
                        onclick: on_submit,
                        if saving() {
                            {i18n.t("pricing.creating")}
                        } else {
                            {i18n.t("form.create")}
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn EditPricingModal(
    auth_store: AuthStore,
    target: PricingTarget,
    identity: authority::PricingIdentity,
    pricing_id: String,
    pricing_model: String,
    pricing_provider: String,
    pricing_currency: String,
    initial_input_price: String,
    initial_output_price: String,
    pricing_version: i64,
    on_close: EventHandler,
    on_updated: EventHandler,
) -> Element {
    let i18n = use_i18n();
    let user_store = use_context::<UserStore>();
    let mut input_price = use_signal(|| authority::display_decimal(&initial_input_price));
    let mut output_price = use_signal(|| authority::display_decimal(&initial_output_price));
    let mut saving = use_signal(|| false);
    let mut form_err = use_signal(String::new);

    let on_submit = move |_| {
        if saving() || !identity.is_current(auth_store, user_store) {
            return;
        }
        let ip_str = input_price().trim().to_owned();
        let op_str = output_price().trim().to_owned();

        if ip_str.is_empty() || op_str.is_empty() {
            form_err.set(i18n.t("pricing.fill_all").to_string());
            return;
        }
        if client_api::api::tenant_pricing::validate_decimal(&ip_str).is_err()
            || client_api::api::tenant_pricing::validate_decimal(&op_str).is_err()
        {
            form_err.set(i18n.t("platform_pricing.invalid_amount").into());
            return;
        }
        saving.set(true);
        form_err.set(String::new());
        let token = auth_store.token().unwrap_or_default();
        let id = pricing_id.clone();
        spawn(async move {
            let req = client_api::api::admin::UpdatePricingRequest::new()
                .with_input_price_per_1k(ip_str)
                .with_output_price_per_1k(op_str)
                .with_expected_version(pricing_version);
            let result = pricing_service::update(target, &id, req, &token).await;
            if !identity.is_current(auth_store, user_store) {
                return;
            }
            match result {
                Ok(_) => {
                    saving.set(false);
                    on_updated.call(());
                }
                Err(e) => {
                    form_err.set(format!("{}：{e}", i18n.t("pricing.update_failed")));
                    saving.set(false);
                }
            }
        });
    };

    rsx! {
        div { class: "modal-backdrop", onclick: move |_| on_close.call(()),
            div { class: "modal", role: "dialog", aria_modal: "true", aria_label: i18n.t("pricing.edit_title"), onclick: move |e| e.stop_propagation(),
                div { class: "modal-header",
                    h2 { class: "modal-title", {i18n.t("pricing.edit_title")} }
                    button {
                        class: "btn btn-ghost btn-sm",
                        r#type: "button",
                        aria_label: i18n.t("common.close"),
                        onclick: move |_| on_close.call(()),
                        "✕"
                    }
                }
                div { class: "modal-body",
                    if !form_err().is_empty() {
                        div { class: "alert alert-error", "{form_err}" }
                    }
                    div { class: "form-group",
                        label { class: "form-label", {i18n.t("pricing.model_name")} }
                        input {
                            class: "input-field",
                            r#type: "text",
                            value: "{pricing_model}",
                            disabled: true,
                        }
                    }
                    div { class: "form-group",
                        label { class: "form-label", {i18n.t("pricing.provider_type")} }
                        input {
                            class: "input-field",
                            r#type: "text",
                            value: "{pricing_provider}",
                            disabled: true,
                        }
                    }
                    div { class: "form-group",
                        label { class: "form-label", {i18n.t("common.currency")} }
                        input {
                            class: "input-field",
                            r#type: "text",
                            value: "{pricing_currency}",
                            disabled: true,
                        }
                    }
                    div { class: "form-group",
                        label { class: "form-label", {i18n.t("pricing.input_price_label")} }
                        input {
                            class: "input-field",
                            r#type: "text",
                            inputmode: "decimal",
                            id: "platform-price-input",
                            maxlength: "64",
                            value: "{input_price}",
                            oninput: move |e| input_price.set(e.value()),
                        }
                    }
                    div { class: "form-group",
                        label { class: "form-label", {i18n.t("pricing.output_price_label")} }
                        input {
                            class: "input-field",
                            r#type: "text",
                            inputmode: "decimal",
                            id: "platform-price-output",
                            maxlength: "64",
                            value: "{output_price}",
                            oninput: move |e| output_price.set(e.value()),
                        }
                    }
                }
                div { class: "modal-footer",
                    button {
                        class: "btn btn-ghost",
                        r#type: "button",
                        onclick: move |_| on_close.call(()),
                        {i18n.t("form.cancel")}
                    }
                    button {
                        class: "btn btn-primary",
                        r#type: "button",
                        disabled: saving(),
                        onclick: on_submit,
                        if saving() {
                            {i18n.t("form.saving")}
                        } else {
                            {i18n.t("form.save_changes")}
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PAGE_SIZE, PricingListQuery, pricing_col_count};

    #[test]
    fn empty_table_colspan_matches_visible_pricing_columns() {
        assert_eq!(pricing_col_count(true), 7);
        assert_eq!(pricing_col_count(false), 6);
    }

    #[test]
    fn committing_search_resets_pricing_page() {
        let mut query = PricingListQuery {
            search: String::new(),
            page: 4,
            page_size: PAGE_SIZE as u32,
        };
        query.commit_search("gpt".to_string());
        assert_eq!(query.search, "gpt");
        assert_eq!(query.page, 1);
    }

    #[test]
    fn pricing_mutations_can_reset_an_orphaned_last_page() {
        let mut query = PricingListQuery {
            search: "matched before editing".to_string(),
            page: 2,
            page_size: PAGE_SIZE as u32,
        };

        query.reset_page();

        assert_eq!(query.page, 1);
        assert_eq!(query.search, "matched before editing");
    }
}
