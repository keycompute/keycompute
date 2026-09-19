use dioxus::prelude::*;
use ui::{Badge, BadgeVariant, PageHeader, Pagination, Table, TableHead};

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::{api_client::with_auto_refresh, billing_service, payment_service};
use crate::stores::auth_store::AuthStore;
use crate::utils::display::payment_status_label;
use crate::utils::format_cny_str;
use crate::utils::resource::{KeyedResourceValue, current_keyed_value};
use crate::utils::time::format_time;

const PAGE_SIZE: usize = 20;

/// 支付与账单页面 - /payments
///
/// 包含：账户余额、充值记录和账单统计
#[component]
pub fn PaymentsOverview() -> Element {
    let i18n = use_i18n();
    let auth_store = use_context::<AuthStore>();

    let mut page = use_signal(|| 1u32);
    let mut page_size = use_signal(|| PAGE_SIZE as u32);

    let nav = use_navigator();
    let balance = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            payment_service::get_balance(&token).await
        })
        .await
    });

    let payment_methods = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            payment_service::get_methods(&token).await
        })
        .await
    });

    // 充值记录（服务端分页）
    let orders = use_resource(move || async move {
        let request_key = (page(), page_size());
        let result = with_auto_refresh(auth_store, |token| async move {
            payment_service::list_orders_page(
                Some(
                    client_api::api::payment::PaymentQueryParams::new()
                        .with_page(request_key.0)
                        .with_page_size(request_key.1),
                ),
                &token,
            )
            .await
        })
        .await;
        KeyedResourceValue::new(request_key, result)
    });

    // 用量统计（真实数据，来自 usage_logs 表）
    let usage_stats = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            billing_service::stats(&token).await
        })
        .await
    });

    let orders_request_key = (page(), page_size());
    let orders_result = current_keyed_value(&orders_request_key, orders.state().cloned(), orders());
    // 加载失败单独渲染 alert 并保留具体原因；表格空态只承载加载中与无数据。
    let orders_error = orders_result
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .map(ToString::to_string);
    let orders_rows = orders_result
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .map(|result| result.orders.as_slice())
        .unwrap_or_default();
    let (orders_is_empty, orders_empty_text) = match &orders_result {
        None => (true, i18n.t("table.loading")),
        Some(Ok(result)) if result.orders.is_empty() => {
            (true, i18n.t("payments.no_recharge_records"))
        }
        _ => (false, ""),
    };
    let (orders_total, orders_total_pages) = recharge_page_totals(
        orders_result
            .as_ref()
            .and_then(|result| result.as_ref().ok()),
        page_size(),
    );

    rsx! {
        div { class: "page-container",
            PageHeader {
                title: i18n.t("payments.title").to_string(),
                description: i18n.t("payments.subtitle").to_string(),
                actions: rsx! {
                    if matches!(payment_methods(), Some(Ok(ref methods)) if !methods.methods.is_empty()) {
                        button {
                            class: "btn btn-primary",
                            onclick: move |_| {
                                nav.push(Route::Recharge {});
                            },
                            {i18n.t("payments.recharge_now")}
                        }
                    }
                },
            }

            if let Some(Err(error)) = balance() {
                p { class: "alert alert-error", role: "alert", {crate::services::api_client::user_error_message(&error)} }
            }
            // ─── 账户余额 ───
            div { class: "stats-grid",
                div { class: "stat-card",
                    p { class: "stat-title", {i18n.t("payments.account_balance")} }
                    match balance() {
                        None => rsx! {
                            p { class: "stat-value", {i18n.t("table.loading")} }
                        },
                        Some(Err(_)) => rsx! {
                            p { class: "stat-value", "—" }
                        },
                        Some(Ok(b)) => rsx! {
                            p {
                                class: "stat-value",
                                title: if b.as_of.is_empty() { i18n.t("common.balance_snapshot").to_string() } else { format!("{}: {}", i18n.t("common.balance_snapshot"), b.as_of) },
                                {format_cny_str(&b.available_balance)}
                            }
                        },
                    }
                }
                div { class: "stat-card",
                    p { class: "stat-title", {i18n.t("payments.frozen_amount")} }
                    match balance() {
                        Some(Ok(b)) => rsx! {
                            p { class: "stat-value", {format_cny_str(&b.frozen_balance)} }
                        },
                        _ => rsx! {
                            p { class: "stat-value", "—" }
                        },
                    }
                }
                div { class: "stat-card",
                    p { class: "stat-title", {i18n.t("payments.total_recharge")} }
                    match balance() {
                        Some(Ok(b)) => rsx! {
                            p { class: "stat-value", {format_cny_str(&b.total_recharged)} }
                        },
                        _ => rsx! {
                            p { class: "stat-value", "—" }
                        },
                    }
                }
                div { class: "stat-card",
                    p { class: "stat-title", {i18n.t("payments.total_consumed")} }
                    match balance() {
                        Some(Ok(b)) => rsx! {
                            p { class: "stat-value", {format_cny_str(&b.total_consumed)} }
                        },
                        _ => rsx! {
                            p { class: "stat-value", "—" }
                        },
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
                            p { class: "stat-value", "¥{crate::utils::format_money(s.total_cost)}" }
                        }
                    },
                    _ => rsx! {},
                }
            }

            // ─── 充値记录 ───
            div { class: "section table-pagination-panel",
                h2 { class: "section-title", {i18n.t("payments.recharge_records")} }
                match orders_error.as_deref() {
                    Some(error) => rsx! {
                        div { class: "alert alert-error", "{i18n.t(\"common.load_failed\")}：{error}" }
                    },
                    None => rsx! {
                        Table {
                            empty: orders_is_empty,
                            empty_text: orders_empty_text.to_string(),
                            col_count: 5,
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
                                for order in orders_rows.iter() {
                                    tr { key: "{order.id}",
                                        td {
                                            code { "{order.out_trade_no}" }
                                        }
                                        td { {format_cny_str(&order.amount)} }
                                        td { "{order.subject}" }
                                        td {
                                            Badge { variant: payment_status_variant(&order.status),
                                                {payment_status_label(&order.status, &i18n)}
                                            }
                                        }
                                        td { {format_time(&order.created_at)} }
                                    }
                                }
                            }
                        }
                    },
                }
            }
            // 分页页脚与面板平级渲染（对齐定价页的页脚结构），避免页脚嵌入面板内部。
            if orders_error.is_none() {
                Pagination {
                    current: page(),
                    total_pages: orders_total_pages,
                    total: orders_total as u64,
                    page_size: page_size(),
                    summary: i18n.t_with_args(
                        "common.pagination_summary",
                        &[
                            ("total", &orders_total.to_string()),
                            ("current", &page().to_string()),
                            ("total_pages", &orders_total_pages.to_string()),
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

fn payment_status_variant(status: &str) -> BadgeVariant {
    match status {
        "paid" | "success" => BadgeVariant::Success,
        "pending" | "processing" => BadgeVariant::Warning,
        "failed" | "cancelled" => BadgeVariant::Error,
        _ => BadgeVariant::Neutral,
    }
}

/// 由充值记录分页响应推导分页摘要所需的 (total, total_pages)。
/// 响应缺失时按空列表处理；页大小至少为 1，避免除零。
fn recharge_page_totals(
    result: Option<&client_api::api::payment::PaymentOrderPage>,
    page_size: u32,
) -> (usize, u32) {
    let total = result
        .map(|page_data| page_data.total as usize)
        .unwrap_or(0);
    let total_pages = total.div_ceil(page_size.max(1) as usize).max(1) as u32;
    (total, total_pages)
}

#[cfg(test)]
mod tests {
    use super::{KeyedResourceValue, PAGE_SIZE, current_keyed_value, recharge_page_totals};
    use client_api::api::payment::PaymentOrderPage;
    use dioxus::prelude::UseResourceState;

    type TestResult = Result<PaymentOrderPage, String>;

    fn page_with_total(total: u64) -> PaymentOrderPage {
        PaymentOrderPage {
            orders: Vec::new(),
            total,
        }
    }

    #[test]
    fn recharge_page_totals_derives_total_pages_from_server_total() {
        assert_eq!(recharge_page_totals(None, PAGE_SIZE as u32), (0, 1));
        assert_eq!(
            recharge_page_totals(Some(&page_with_total(0)), PAGE_SIZE as u32),
            (0, 1)
        );
        assert_eq!(
            recharge_page_totals(Some(&page_with_total(20)), PAGE_SIZE as u32),
            (20, 1)
        );
        assert_eq!(
            recharge_page_totals(Some(&page_with_total(21)), PAGE_SIZE as u32),
            (21, 2)
        );
        assert_eq!(
            recharge_page_totals(Some(&page_with_total(45)), PAGE_SIZE as u32),
            (45, 3)
        );
    }

    #[test]
    fn recharge_page_totals_guards_against_zero_page_size() {
        assert_eq!(recharge_page_totals(Some(&page_with_total(5)), 0), (5, 5));
    }

    #[test]
    fn recharge_rows_are_stale_when_the_requested_page_changes() {
        let loaded = |key: (u32, u32)| {
            KeyedResourceValue::<_, TestResult>::new(key, Ok(page_with_total(45)))
        };

        assert!(
            current_keyed_value(
                &(2u32, PAGE_SIZE as u32),
                UseResourceState::Ready,
                Some(loaded((1u32, PAGE_SIZE as u32))),
            )
            .is_none()
        );
        assert!(
            current_keyed_value(
                &(1u32, PAGE_SIZE as u32),
                UseResourceState::Ready,
                Some(loaded((1u32, PAGE_SIZE as u32))),
            )
            .is_some()
        );
    }

    #[test]
    fn recharge_load_errors_keep_the_underlying_reason_visible() {
        let source = include_str!("overview.rs");
        let component_source = source.split("#[cfg(test)]").next().unwrap_or(source);

        assert!(
            component_source.contains("alert alert-error"),
            "充值记录加载失败应使用独立的错误提示样式"
        );
        assert!(
            component_source.contains("：{error}"),
            "充值记录加载失败时必须展示具体错误原因"
        );
    }
}
