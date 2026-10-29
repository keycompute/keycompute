use dioxus::prelude::*;
use ui::{
    Alert, AlertVariant, Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Modal, PageHeader,
    Pagination, Table, TableHead,
};

use crate::hooks::use_i18n::use_i18n;
use crate::router::{NodeEarningsQuery, Route};
use crate::services::node_tips_service;
use crate::stores::{auth_store::AuthStore, ui_store::UiStore};
use crate::utils::resource::{KeyedResourceValue, current_keyed_value};
use crate::utils::{display::short_id, focus_element, format_precise_cny_str, time::format_time};
use crate::views::tenant::common::TenantRequiredPage;

const HISTORY_PAGE_SIZE: u32 = 20;

// Token refresh does not change the financial owner of an accepted command.
// Tenant switches, logout and a different login must still discard its result.
fn withdrawal_completion_matches(
    current: &crate::stores::auth_store::AuthState,
    observed: &crate::stores::auth_store::AuthState,
) -> bool {
    current.is_authenticated
        && current.session_id == observed.session_id
        && current.selected_tenant_id == observed.selected_tenant_id
}

/// 计算简单分页当前页可见的记录范围（1-based，含首尾）。
/// `offset` 为已跳过的记录数；结束值截断到 total；total 为 0 时返回 (0, 0)。
#[allow(dead_code)]
fn visible_range(offset: usize, page_size: usize, total: usize) -> (usize, usize) {
    if total == 0 {
        return (0, 0);
    }
    let start = offset + 1;
    let end = (offset + page_size).min(total);
    (start, end)
}

/// 提现方式
#[derive(Clone, PartialEq)]
enum WithdrawMethod {
    Alipay,
    Balance,
}

impl WithdrawMethod {
    fn value(&self) -> &'static str {
        match self {
            WithdrawMethod::Alipay => "alipay",
            WithdrawMethod::Balance => "balance",
        }
    }
    fn label(&self) -> String {
        let i18n = use_i18n();
        match self {
            WithdrawMethod::Alipay => i18n.t("node_earnings.method_alipay").to_string(),
            WithdrawMethod::Balance => i18n.t("node_earnings.method_balance").to_string(),
        }
    }
}

/// 提现记录状态枚举
#[derive(Clone, PartialEq)]
enum WithdrawalStatus {
    Pending,
    Approved,
    Completed,
    Rejected,
}

impl WithdrawalStatus {
    fn variant(&self) -> BadgeVariant {
        match self {
            WithdrawalStatus::Pending => BadgeVariant::Warning,
            WithdrawalStatus::Approved => BadgeVariant::Info,
            WithdrawalStatus::Completed => BadgeVariant::Success,
            WithdrawalStatus::Rejected => BadgeVariant::Error,
        }
    }
    fn label(&self) -> String {
        let i18n = use_i18n();
        match self {
            WithdrawalStatus::Pending => i18n.t("node_earnings.status_pending").to_string(),
            WithdrawalStatus::Approved => i18n.t("node_earnings.status_approved").to_string(),
            WithdrawalStatus::Completed => i18n.t("node_earnings.status_completed").to_string(),
            WithdrawalStatus::Rejected => i18n.t("node_earnings.status_rejected").to_string(),
        }
    }
}

#[component]
pub fn NodeEarnings(query: NodeEarningsQuery) -> Element {
    rsx! { TenantRequiredPage { NodeEarningsContent { query } } }
}

#[component]
fn NodeEarningsContent(query: NodeEarningsQuery) -> Element {
    let i18n = use_i18n();
    let auth_store = use_context::<AuthStore>();
    let _ui_store = use_context::<UiStore>();

    // 汇总数据 resource
    let mut summary_resource = use_resource(move || {
        let auth = auth_store.clone();
        async move {
            let token = auth.token().unwrap_or_default();
            node_tips_service::get_my_tips_summary(&token).await
        }
    });

    // 小费历史
    let mut history_offset = use_signal(|| 0u32);
    let mut history_page_size = use_signal(|| HISTORY_PAGE_SIZE);
    let history_resource = use_resource(move || {
        let auth = auth_store.clone();
        let offset = *history_offset.read();
        let page_size = history_page_size();
        async move {
            let token = auth.token().unwrap_or_default();
            let result = node_tips_service::get_my_tips_history(&token, page_size, offset).await;
            KeyedResourceValue::new((offset, page_size), result)
        }
    });

    let history_request_key = (history_offset(), history_page_size());
    let history_result = current_keyed_value(
        &history_request_key,
        history_resource.state().cloned(),
        history_resource(),
    );

    // 当前页可见记录范围（供小费历史简单分页展示）；total 为 0 时返回 (0, 0)
    let history_total = history_result
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .map(|resp| resp.total.max(0) as usize)
        .unwrap_or(0);
    let current_history_page = (*history_offset.read() / history_page_size().max(1)) + 1;
    let history_total_pages = history_total
        .div_ceil(history_page_size().max(1) as usize)
        .max(1) as u32;

    // 提现记录
    let mut withdrawals_resource = use_resource(move || {
        let auth = auth_store.clone();
        async move {
            let token = auth.token().unwrap_or_default();
            node_tips_service::get_my_withdrawals(&token).await
        }
    });

    // 提现弹窗状态
    #[allow(unused_mut)]
    let mut withdraw_modal = use_signal(|| false);
    #[allow(unused_mut)]
    let mut withdraw_method = use_signal(|| WithdrawMethod::Balance);
    #[allow(unused_mut)]
    let mut alipay_account = use_signal(String::new);
    #[allow(unused_mut)]
    let mut real_name = use_signal(String::new);
    #[allow(unused_mut)]
    let mut withdraw_loading = use_signal(|| false);
    #[allow(unused_mut)]
    let mut withdraw_error = use_signal(|| None::<String>);
    let show_tip_history = query.tab.as_deref() != Some("withdrawals");
    let nav = use_navigator();

    // 小费历史分页页脚仅在数据加载成功后渲染（与面板内表格的数据来源保持一致）
    let show_history_pagination = history_result
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .is_some();

    rsx! {
        div { class: "page-container node-earnings-page",
            PageHeader {
                title: i18n.t("page.node_earnings").to_string(),
                description: i18n.t("node_earnings.subtitle").to_string(),
                actions: rsx! {
                    Button {
                        variant: ButtonVariant::Primary,
                        size: ButtonSize::Medium,
                        onclick: move |_| {
                            withdraw_error.set(None);
                            withdraw_modal.set(true);
                        },
                        {i18n.t("node_earnings.withdraw_btn")}
                    }
                },
            }

            // 错误提示
            if let Some(Err(e)) = summary_resource().as_ref().map(|r| r.as_ref()) {
                Alert { variant: AlertVariant::Error, {format!("{}：{}", i18n.t("common.load_failed"), crate::services::api_client::user_error_message(i18n, e))} }
            }

            // 汇总卡片
            div { class: "earnings-summary-grid",
                if let Some(Ok(summary)) = summary_resource().as_ref().map(|r| r.as_ref()) {
                    EarningsCard {
                        label: i18n.t("node_earnings.pending_amount"),
                        value: format_precise_cny_str(&summary.pending_amount),
                        meta: i18n.t_with_args(
                            "node_earnings.pending_count",
                            &[("count", &summary.pending_count.to_string())],
                        ),
                        variant: "warning",
                    }
                    EarningsCard {
                        label: i18n.t("node_earnings.withdrawn_amount"),
                        value: format_precise_cny_str(&summary.withdrawn_amount),
                        meta: i18n.t("node_earnings.withdrawn_meta"),
                        variant: "success",
                    }
                    EarningsCard {
                        label: i18n.t("node_earnings.total_amount"),
                        value: format_precise_cny_str(&summary.total_amount),
                        meta: i18n.t("node_earnings.total_meta"),
                        variant: "info",
                    }
                } else if summary_resource().is_none() {
                    div { class: "earnings-card-skeleton" }
                    div { class: "earnings-card-skeleton" }
                    div { class: "earnings-card-skeleton" }
                }
            }

            div { class: "tabs", role: "tablist", aria_label: i18n.t("node_earnings.records_tabs"),
                button {
                    id: "node-earnings-history-tab",
                    class: if show_tip_history { "tab active" } else { "tab" },
                    role: "tab",
                    aria_selected: show_tip_history,
                    aria_controls: "node-earnings-history-panel",
                    tabindex: if show_tip_history { "0" } else { "-1" },
                    onclick: move |_| { nav.replace(Route::NodeEarnings { query: NodeEarningsQuery::for_tab("history") }); },
                    onkeydown: move |event| match event.key() {
                        Key::ArrowRight | Key::ArrowLeft | Key::End => {
                            event.prevent_default();
                            nav.replace(Route::NodeEarnings { query: NodeEarningsQuery::for_tab("withdrawals") });
                            focus_element("node-earnings-withdrawals-tab");
                        }
                        Key::Home => event.prevent_default(),
                        _ => {}
                    },
                    {i18n.t("node_earnings.history_title")}
                }
                button {
                    id: "node-earnings-withdrawals-tab",
                    class: if !show_tip_history { "tab active" } else { "tab" },
                    role: "tab",
                    aria_selected: !show_tip_history,
                    aria_controls: "node-earnings-withdrawals-panel",
                    tabindex: if !show_tip_history { "0" } else { "-1" },
                    onclick: move |_| { nav.replace(Route::NodeEarnings { query: NodeEarningsQuery::for_tab("withdrawals") }); },
                    onkeydown: move |event| match event.key() {
                        Key::ArrowLeft | Key::ArrowRight | Key::Home => {
                            event.prevent_default();
                            nav.replace(Route::NodeEarnings { query: NodeEarningsQuery::for_tab("history") });
                            focus_element("node-earnings-history-tab");
                        }
                        Key::End => event.prevent_default(),
                        _ => {}
                    },
                    {i18n.t("node_earnings.withdrawals_title")}
                }
            }

            // 小费历史
            if show_tip_history {
            section {
                id: "node-earnings-history-panel",
                role: "tabpanel",
                aria_labelledby: "node-earnings-history-tab",
                tabindex: "0",
            div { class: "card table-pagination-panel",
                div { class: "card-header",
                    h3 { class: "card-title", {i18n.t("node_earnings.history_title")} }
                }
                div { class: "card-body",
                    match history_result.as_ref().map(|r| r.as_ref()) {
                        None => rsx! {
                            div { class: "text-secondary", {i18n.t("table.loading")} }
                        },
                        Some(Err(e)) => rsx! {
                            Alert { variant: AlertVariant::Error, {crate::services::api_client::user_error_message(i18n, e)} }
                        },
                        Some(Ok(resp)) => rsx! {
                            Table {
                                empty: resp.items.is_empty(),
                                empty_text: i18n.t("node_earnings.no_history"),
                                col_count: 5,
                                thead {
                                    tr {
                                        TableHead { {i18n.t("node_earnings.col_time")} }
                                        TableHead { {i18n.t("node_earnings.col_bill_amount")} }
                                        TableHead { {i18n.t("node_earnings.col_tip_amount")} }
                                        TableHead { {i18n.t("node_earnings.col_tip_ratio")} }
                                        TableHead { "ID" }
                                    }
                                }
                                tbody {
                                    for item in resp.items.iter() {
                                        tr {
                                            td { {format_time(&item.created_at)} }
                                            td { {format_precise_cny_str(&item.bill_amount)} }
                                            td { {format_precise_cny_str(&item.tip_amount)} }
                                            td { "{item.tip_ratio}" }
                                            td { class: "text-muted", title: "{item.id}", {short_id(&item.id)} }
                                        }
                                    }
                                }
                            }
                        },
                    }
                }
            }

            // 分页页脚与面板平级渲染（对齐定价页的页脚结构），避免页脚嵌入面板内部。
            if show_history_pagination {
                Pagination {
                    current: current_history_page,
                    total_pages: history_total_pages,
                    total: history_total as u64,
                    page_size: history_page_size(),
                    summary: i18n.t_with_args(
                        "common.pagination_summary",
                        &[
                            ("total", &history_total.to_string()),
                            ("current", &current_history_page.to_string()),
                            ("total_pages", &history_total_pages.to_string()),
                        ],
                    ),
                    page_size_label: i18n.t("common.pagination_page_size").to_string(),
                    page_size_suffix: i18n.t("common.items_suffix").to_string(),
                    previous_label: i18n.t("table.previous").to_string(),
                    next_label: i18n.t("table.next").to_string(),
                    on_page_change: move |p: u32| {
                        *history_offset.write() = (p.saturating_sub(1)) * history_page_size();
                    },
                    on_page_size_change: move |size| {
                        history_page_size.set(size);
                        history_offset.set(0);
                    },
                }
            }
            }
            }

            // 提现记录
            if !show_tip_history {
            section {
                id: "node-earnings-withdrawals-panel",
                role: "tabpanel",
                aria_labelledby: "node-earnings-withdrawals-tab",
                tabindex: "0",
            div { class: "card node-earnings-withdrawals",
                div { class: "card-header",
                    h3 { class: "card-title", {i18n.t("node_earnings.withdrawals_title")} }
                }
                div { class: "card-body",
                    match withdrawals_resource().as_ref().map(|r| r.as_ref()) {
                        None => rsx! {
                            div { class: "text-secondary", {i18n.t("table.loading")} }
                        },
                        Some(Err(e)) => rsx! {
                            Alert { variant: AlertVariant::Error, {crate::services::api_client::user_error_message(i18n, e)} }
                        },
                        Some(Ok(records)) => rsx! {
                            Table {
                                empty: records.is_empty(),
                                empty_text: i18n.t("node_earnings.no_withdrawals"),
                                col_count: 6,
                                thead {
                                    tr {
                                        TableHead { {i18n.t("node_earnings.col_time")} }
                                        TableHead { {i18n.t("node_earnings.col_amount")} }
                                        TableHead { {i18n.t("node_earnings.col_method")} }
                                        TableHead { {i18n.t("node_earnings.col_status")} }
                                        TableHead { {i18n.t("node_earnings.col_remark")} }
                                        TableHead { "ID" }
                                    }
                                }
                                tbody {
                                    for w in records.iter() {
                                        tr {
                                            td { {format_time(&w.created_at)} }
                                            td { {format_precise_cny_str(&w.total_amount)} }
                                            td {
                                                match w.withdrawal_type.as_str() {
                                                    "alipay" => i18n.t("node_earnings.method_alipay"),
                                                    "balance" => i18n.t("node_earnings.method_balance"),
                                                    _ => w.withdrawal_type.as_str(),
                                                }
                                            }
                                            td {
                                                WithdrawalStatusBadge { status: w.status.clone() }
                                            }
                                            td { class: "text-secondary",
                                                if let Some(ref remark) = w.admin_remark {
                                                    "{remark}"
                                                } else {
                                                    "—"
                                                }
                                            }
                                            td { class: "text-muted", title: "{w.id}", {short_id(&w.id)} }
                                        }
                                    }
                                }
                            }
                        },
                    }
                }
            }
            }
            }

            // 提现弹窗
            WithdrawModal {
                withdraw_modal,
                withdraw_method,
                alipay_account,
                real_name,
                withdraw_loading,
                withdraw_error,
                withdrawable_amount: summary_resource()
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .map(|summary| format_precise_cny_str(&summary.pending_amount))
                    .unwrap_or_else(|| "—".to_string()),
                on_success: move |_| {
                    summary_resource.restart();
                    withdrawals_resource.restart();
                },
            }
        }
    }
}

/// 汇总卡片
#[component]
fn EarningsCard(label: String, value: String, meta: String, variant: &'static str) -> Element {
    rsx! {
        div { class: "earnings-card earnings-card-{variant}",
            p { class: "earnings-card-label", "{label}" }
            p { class: "earnings-card-value", "{value}" }
            p { class: "earnings-card-meta", "{meta}" }
        }
    }
}

/// 提现状态 Badge
#[component]
fn WithdrawalStatusBadge(status: String) -> Element {
    let s = match status.as_str() {
        "pending" => WithdrawalStatus::Pending,
        "approved" => WithdrawalStatus::Approved,
        "completed" => WithdrawalStatus::Completed,
        "rejected" => WithdrawalStatus::Rejected,
        _ => WithdrawalStatus::Pending,
    };
    rsx! {
        Badge { variant: s.variant(), "{s.label()}" }
    }
}

/// 提现弹窗
///
/// 提交逻辑直接内联在弹窗的提交按钮中，避免复杂闭包无法转换为 EventHandler<()> 的问题。
#[component]
fn WithdrawModal(
    mut withdraw_modal: Signal<bool>,
    mut withdraw_method: Signal<WithdrawMethod>,
    mut alipay_account: Signal<String>,
    mut real_name: Signal<String>,
    withdraw_loading: Signal<bool>,
    withdraw_error: Signal<Option<String>>,
    withdrawable_amount: String,
    on_success: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let auth_store = use_context::<AuthStore>();
    let ui_store = use_context::<UiStore>();
    let opened_identity = use_hook(|| {
        let state = auth_store.state.peek();
        (state.session_id, state.selected_tenant_id.clone())
    });

    let mut submitted = use_signal(|| None::<client_api::api::node_tips::CreateWithdrawalRequest>);

    rsx! {
    Modal {
        open: withdraw_modal,
        title: i18n.t("node_earnings.withdraw_title").to_string(),
        close_label: i18n.t("common.close").to_string(),
        onclose: move |_| {
            if !withdraw_loading() {
                withdraw_modal.set(false);
            }
        },
        footer: rsx! {
            Button {
                variant: ButtonVariant::Ghost,
                size: ButtonSize::Medium,
                disabled: withdraw_loading(),
                onclick: move |_| withdraw_modal.set(false),
                {i18n.t("form.cancel")}
            }
            Button {
                variant: ButtonVariant::Primary,
                size: ButtonSize::Medium,
                disabled: withdraw_loading(),
                onclick: move |_| {
                    let current=auth_store.state.peek().clone();
                    if current.session_id!=opened_identity.0 || current.selected_tenant_id!=opened_identity.1 {
                        withdraw_modal.set(false);
                        return;
                    }
                    let needs_alipay = withdraw_method() == WithdrawMethod::Alipay;
                    if needs_alipay
                        && (alipay_account().trim().is_empty() || real_name().trim().is_empty())
                    {
                        withdraw_error.set(Some(i18n.t("node_earnings.fill_alipay").to_string()));
                        return;
                    }
                    withdraw_loading.set(true);
                    withdraw_error.set(None);

                    let auth = auth_store.clone();
                    let mut ui = ui_store.clone();
                    let i18n_clone = i18n.clone();
                    let method_val = withdraw_method().value().to_string();
                    let alipay_opt = if needs_alipay {
                        Some(alipay_account().to_string())
                    } else {
                        None
                    };
                    let name_opt = if needs_alipay { Some(real_name().to_string()) } else { None };
                    let observed = auth.state.peek().clone();
                    let mut intent = client_api::api::node_tips::CreateWithdrawalRequest {
                        request_id: uuid::Uuid::new_v4(), currency: "CNY".into(),
                        withdrawal_type: method_val.clone(),alipay_account:alipay_opt.clone(),real_name:name_opt.clone(),
                    };
                    if let Some(previous) = submitted.peek().as_ref()
                        && previous.withdrawal_type==intent.withdrawal_type
                        && previous.alipay_account==intent.alipay_account
                        && previous.real_name==intent.real_name {
                        intent.request_id=previous.request_id;
                    }
                    let request_id = intent.request_id;
                    submitted.set(Some(intent));
                    let mut wm = withdraw_modal.clone();
                    let mut aa = alipay_account.clone();
                    let mut rn = real_name.clone();
                    let mut wl = withdraw_loading.clone();
                    spawn(async move {
                        let token = observed.access_token.clone().unwrap_or_default();
                        let alipay_str = alipay_opt.as_deref();
                        let name_str = name_opt.as_deref();
                        match node_tips_service::create_withdrawal(
                                &token,
                                request_id,
                                &method_val,
                                alipay_str,
                                name_str,
                            )
                            .await
                        {
                            Ok(_) => {
                                if !withdrawal_completion_matches(&auth.state.peek(), &observed) { return; }
                                wl.set(false);
                                wm.set(false);
                                aa.set(String::new());
                                rn.set(String::new());
                                on_success.call(());
                                let msg = if needs_alipay {
                                    i18n_clone.t("node_earnings.withdraw_alipay_success")
                                } else {
                                    i18n_clone.t("node_earnings.withdraw_balance_success")
                                };
                                ui.show_success(msg);
                            }
                            Err(e) => {
                                if !withdrawal_completion_matches(&auth.state.peek(), &observed) { return; }
                                wl.set(false);
                                withdraw_error
                                    .set(
                                        Some(
                                            format!(
                                                "{}: {}",
                                                i18n_clone.t("node_earnings.withdraw_failed"),
                                                e,
                                            ),
                                        ),
                                    );
                            }
                        }
                    });
                },
                if withdraw_loading() {
                    span { class: "spinner spinner-sm" }
                    " {i18n.t(\"form.submit\")}"
                } else {
                    {i18n.t("form.submit")}
                }
            }
        },
        div { class: "withdraw-amount-summary",
            span { class: "form-label", {i18n.t("node_earnings.withdraw_amount")} }
            strong { class: "withdraw-amount-value", "{withdrawable_amount}" }
            p { class: "form-hint", {i18n.t("node_earnings.withdraw_amount_hint")} }
        }
        div {
                if let Some(ref err) = withdraw_error() {
                    Alert { variant: AlertVariant::Error, "{err}" }
                }

                // 选择提现方式
                div { class: "form-group",
                    label { class: "form-label", {i18n.t("node_earnings.withdraw_method")} }
                    div { class: "withdraw-method-grid",
                        for method in [WithdrawMethod::Balance, WithdrawMethod::Alipay] {
                            {
                                let m = method.clone();
                                let is_active = *withdraw_method.read() == method;
                                rsx! {
                                    button {
                                        class: if is_active { "withdraw-method-card active" } else { "withdraw-method-card" },
                                        r#type: "button",
                                        aria_pressed: is_active,
                                        onclick: move |_| {
                                            withdraw_method.set(m.clone());
                                            withdraw_error.set(None);
                                        },
                                        strong { "{m.label()}" }
                                        p { class: "text-secondary",
                                            if m == WithdrawMethod::Balance {
                                                {i18n.t("node_earnings.method_balance_desc")}
                                            } else {
                                                {i18n.t("node_earnings.method_alipay_desc")}
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // 支付宝方式需填写账号和姓名
                if matches!(*withdraw_method.read(), WithdrawMethod::Alipay) {
                    div { class: "form-group",
                        label { class: "form-label", r#for: "withdraw-alipay-account", {i18n.t("node_earnings.alipay_account")} }
                        input {
                            id: "withdraw-alipay-account",
                            class: "form-input",
                            r#type: "text",
                            placeholder: i18n.t("node_earnings.alipay_placeholder"),
                            value: "{alipay_account}",
                            oninput: move |e| alipay_account.set(e.value()),
                        }
                    }
                    div { class: "form-group",
                        label { class: "form-label", r#for: "withdraw-real-name", {i18n.t("node_earnings.real_name")} }
                        input {
                            id: "withdraw-real-name",
                            class: "form-input",
                            r#type: "text",
                            placeholder: i18n.t("node_earnings.real_name_placeholder"),
                            value: "{real_name}",
                            oninput: move |e| real_name.set(e.value()),
                        }
                    }
                }

                ul { class: "withdraw-policy-list",
                    li { {i18n.t("node_earnings.withdraw_minimum")} }
                    li { {i18n.t("node_earnings.withdraw_fee")} }
                    li {
                        if matches!(*withdraw_method.read(), WithdrawMethod::Balance) {
                            {i18n.t("node_earnings.withdraw_time_balance")}
                        } else {
                            {i18n.t("node_earnings.withdraw_time_alipay")}
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::visible_range;

    #[test]
    fn visible_range_first_page_shows_page_size_items() {
        assert_eq!(visible_range(0, 20, 50), (1, 20));
    }

    #[test]
    fn visible_range_last_page_truncates_to_total() {
        assert_eq!(visible_range(40, 20, 50), (41, 50));
        assert_eq!(visible_range(45, 20, 50), (46, 50));
    }

    #[test]
    fn visible_range_exact_multiples() {
        assert_eq!(visible_range(0, 20, 20), (1, 20));
        assert_eq!(visible_range(20, 20, 40), (21, 40));
    }

    #[test]
    fn visible_range_empty_total_returns_zero() {
        assert_eq!(visible_range(0, 20, 0), (0, 0));
        assert_eq!(visible_range(40, 20, 0), (0, 0));
    }
}

#[cfg(test)]
mod withdrawal_completion_tests {
    use super::withdrawal_completion_matches;
    use crate::stores::auth_store::AuthState;
    #[test]
    fn refreshed_same_tenant_session_completes_the_original_command() {
        let mut observed = AuthState::logged_in("fixture-original".into());
        observed.selected_tenant_id = Some("tenant-a".into());
        let mut refreshed = observed.clone();
        refreshed.access_token = Some("fixture-refreshed".into());
        refreshed.token_revision += 1;
        assert!(withdrawal_completion_matches(&refreshed, &observed));
    }
    #[test]
    fn another_tenant_login_or_logout_cannot_receive_a_late_completion() {
        let mut observed = AuthState::logged_in("fixture-original".into());
        observed.selected_tenant_id = Some("tenant-a".into());
        let mut current = observed.clone();
        current.selected_tenant_id = Some("tenant-b".into());
        assert!(!withdrawal_completion_matches(&current, &observed));
        current = observed.clone();
        current.session_id = uuid::Uuid::new_v4();
        assert!(!withdrawal_completion_matches(&current, &observed));
        current = observed.clone();
        current.is_authenticated = false;
        assert!(!withdrawal_completion_matches(&current, &observed));
    }
}
