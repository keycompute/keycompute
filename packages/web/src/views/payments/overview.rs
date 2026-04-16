use dioxus::prelude::*;
use ui::{Badge, BadgeVariant, Pagination, Table, TableHead};

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::{api_client::with_auto_refresh, billing_service, payment_service};
use crate::stores::auth_store::AuthStore;
use crate::utils::time::format_time;

const PAGE_SIZE: usize = 20;

/// 支付与账单页面 - /payments
///
/// 包含：账户余额、充値记录、账单统计、账单明细
#[component]
pub fn PaymentsOverview() -> Element {
    let i18n = use_i18n();
    let auth_store = use_context::<AuthStore>();

    let nav = use_navigator();
    let mut page = use_signal(|| 1u32);

    let balance = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            payment_service::get_balance(&token).await
        })
        .await
    });

    let orders = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            payment_service::list_orders(None, &token).await
        })
        .await
    });

    // 用量统计（真实数据，来自 usage_logs 表）
    let usage_stats = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            billing_service::stats(&token).await
        })
        .await
    });

    // 用量明细（真实数据，来自 usage_logs 表）
    let usage_records = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            billing_service::list(&token).await
        })
        .await
    });

    rsx! {
        div {
            class: "page-container",
            div {
                class: "page-header",
                h1 { class: "page-title", {i18n.t("payments.title")} }
                p { class: "page-subtitle", {i18n.t("payments.subtitle")} }
                button {
                    class: "btn btn-primary",
                    onclick: move |_| { nav.push(Route::Recharge {}); },
                    {i18n.t("payments.recharge_now")}
                }
            }

            // ─── 账户余额 ───
            div { class: "stats-grid",
                div {
                    class: "stat-card",
                    p { class: "stat-title", {i18n.t("payments.account_balance")} }
                    match balance() {
                        None => rsx! { p { class: "stat-value", {i18n.t("table.loading")} } },
                        Some(Err(e)) => rsx! { p { class: "stat-value text-error", "{i18n.t(\"common.error\")}: {e}" } },
                        Some(Ok(b)) => rsx! {
                            p { class: "stat-value", "¥ {b.available_balance}" }
                        },
                    }
                }
                div {
                    class: "stat-card",
                    p { class: "stat-title", {i18n.t("payments.frozen_amount")} }
                    match balance() {
                        Some(Ok(b)) => rsx! { p { class: "stat-value", "¥ {b.frozen_balance}" } },
                        _ => rsx! { p { class: "stat-value", "—" } },
                    }
                }
                div {
                    class: "stat-card",
                    p { class: "stat-title", {i18n.t("payments.total_recharge")} }
                    match balance() {
                        Some(Ok(b)) => rsx! { p { class: "stat-value", "¥ {b.total_recharged}" } },
                        _ => rsx! { p { class: "stat-value", "—" } },
                    }
                }
                div {
                    class: "stat-card",
                    p { class: "stat-title", {i18n.t("payments.total_consumed")} }
                    match balance() {
                        Some(Ok(b)) => rsx! { p { class: "stat-value", "¥ {b.total_consumed}" } },
                        _ => rsx! { p { class: "stat-value", "—" } },
                    }
                }
                match usage_stats() {
                    Some(Ok(s)) => rsx! {
                        div { class: "stat-card",
                            p { class: "stat-title", {i18n.t("payments.usage_requests")} }
                            p { class: "stat-value", "{s.total_requests}" }
                        }
                        div { class: "stat-card",
                            p { class: "stat-title", {i18n.t("payments.input_tokens")} }
                            p { class: "stat-value", "{s.input_tokens}" }
                        }
                        div { class: "stat-card",
                            p { class: "stat-title", {i18n.t("payments.output_tokens")} }
                            p { class: "stat-value", "{s.output_tokens}" }
                        }
                        div { class: "stat-card",
                            p { class: "stat-title", {i18n.t("payments.total_cost")} }
                            p { class: "stat-value", "¥{s.total_cost:.2}" }
                        }
                    },
                    _ => rsx! {},
                }
            }

            // ─── 充値记录 ───
            div { class: "section",
                h2 { class: "section-title", {i18n.t("payments.recharge_records")} }
                match orders() {
                    None => rsx! { div { class: "loading-state", {i18n.t("table.loading")} } },
                    Some(Err(e)) => rsx! { div { class: "alert alert-error", "{i18n.t(\"common.load_failed\")}：{e}" } },
                    Some(Ok(list)) => {
                        if list.is_empty() {
                            rsx! { div { class: "empty-state", p { {i18n.t("payments.no_recharge_records")} } } }
                        } else {
                            rsx! {
                                Table {
                                    col_count: 4,
                                    thead {
                                        tr {
                                            TableHead { {i18n.t("payments.order_no")} }
                                            TableHead { {i18n.t("common.amount")} }
                                            TableHead { {i18n.t("payments.subject")} }
                                            TableHead { {i18n.t("table.status")} }
                                            TableHead { {i18n.t("common.time")} }
                                        }
                                    }
                                    tbody {
                                        for order in list.iter() {
                                            tr {
                                                key: "{order.id}",
                                                td { code { "{order.out_trade_no}" } }
                                                td { "¥ {order.amount}" }
                                                td { "{order.subject}" }
                                                td {
                                                    Badge {
                                                        variant: payment_status_variant(&order.status),
                                                        "{order.status}"
                                                    }
                                                }
                                                td { { format_time(&order.created_at) } }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ─── 用量明细 ───
            div { class: "section",
                h2 { class: "section-title", {i18n.t("payments.usage_details")} }
                match usage_records() {
                    None => rsx! { p { class: "loading-text", {i18n.t("table.loading")} } },
                    Some(Err(e)) => rsx! { p { class: "error-text", "{i18n.t(\"common.load_failed\")}：{e}" } },
                    Some(Ok(recs)) if recs.is_empty() => rsx! {
                        p { class: "empty-text", {i18n.t("payments.no_usage_records")} }
                    },
                    Some(Ok(recs)) => rsx! {
                        div { class: "table-container",
                            table { class: "data-table",
                                thead {
                                    tr {
                                        th { {i18n.t("common.time")} }
                                        th { {i18n.t("usage.model")} }
                                        th { {i18n.t("payments.input_tokens")} }
                                        th { {i18n.t("payments.output_tokens")} }
                                        th { {i18n.t("payments.total_tokens")} }
                                        th { {i18n.t("common.cost")} }
                                        th { {i18n.t("table.status")} }
                                    }
                                }
                                tbody {
                                    {
                                        let start = (page() as usize - 1) * PAGE_SIZE;
                                        rsx! {
                                            for r in recs.iter().skip(start).take(PAGE_SIZE) {
                                                tr {
                                                    td { { format_time(&r.created_at) } }
                                                    td { "{r.model}" }
                                                    td { "{r.prompt_tokens}" }
                                                    td { "{r.completion_tokens}" }
                                                    td { "{r.total_tokens}" }
                                                    td { "¥{r.cost:.2}" }
                                                    td {
                                                        span {
                                                            class: if r.status == "success" { "badge badge-success" } else { "badge badge-warning" },
                                                            "{r.status}"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        {
                            let total = recs.len();
                            let total_pages = total.div_ceil(PAGE_SIZE).max(1) as u32;
                            rsx! {
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
                    },
                }
            }
        }
    }
}

fn payment_status_variant(status: &str) -> BadgeVariant {
    match status {
        "paid" | "success" => BadgeVariant::Success,
        "pending" | "processing" => BadgeVariant::Warning,
        "failed" | "cancelled" => BadgeVariant::Error,
        _ => BadgeVariant::Neutral,
    }
}
