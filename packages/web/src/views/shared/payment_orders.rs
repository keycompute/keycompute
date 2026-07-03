use client_api::{AdminApi, api::admin::PaymentQueryParams as AdminPaymentQueryParams};
use dioxus::prelude::*;
use ui::{Badge, BadgeVariant, Pagination, Table, TableHead};

const PAGE_SIZE: usize = 20;

use crate::hooks::use_i18n::use_i18n;
use crate::services::{
    api_client::{get_client, with_auto_refresh},
    payment_service,
};
use crate::stores::auth_store::AuthStore;
use crate::stores::user_store::UserStore;
use crate::utils::time::format_time;

/// 支付订单页面
///
/// - 普通用户：仅查看自己的订单
/// - Admin：查看所有订单
#[component]
pub fn PaymentOrders() -> Element {
    let i18n = use_i18n();
    let user_store = use_context::<UserStore>();
    let auth_store = use_context::<AuthStore>();
    let is_admin = user_store
        .info
        .read()
        .as_ref()
        .map(|u| u.is_admin())
        .unwrap_or(false);

    let mut status_filter = use_signal(|| "all".to_string());
    let mut page = use_signal(|| 1u32);

    // 普通用户订单
    let my_orders = use_resource(move || async move {
        if is_admin {
            return Ok(vec![]);
        }
        let status = status_filter();
        let params = if status == "all" {
            None
        } else {
            // 修复：必须把 status 设入查询参数
            Some(client_api::api::payment::PaymentQueryParams::new().with_status(status.clone()))
        };
        with_auto_refresh(auth_store, |token| {
            let value = params.clone();
            async move { payment_service::list_orders(value, &token).await }
        })
        .await
    });

    // Admin 订单
    let admin_orders = use_resource(move || async move {
        if !is_admin {
            return Ok(vec![]);
        }
        let status = status_filter();
        let params = if status != "all" {
            Some(AdminPaymentQueryParams::new().with_status(status.clone()))
        } else {
            None
        };
        with_auto_refresh(auth_store, |token| {
            let value = params.clone();
            async move {
                let client = get_client();
                AdminApi::new(&client)
                    .list_all_payment_orders(value.as_ref(), &token)
                    .await
            }
        })
        .await
    });

    let filter_labels = [
        ("all", i18n.t("payment_orders.filter_all")),
        ("pending", i18n.t("payment_orders.filter_pending")),
        ("paid", i18n.t("payment_orders.filter_paid")),
        ("failed", i18n.t("payment_orders.filter_failed")),
    ];

    rsx! {
        div { class: "page-header",
            h1 { class: "page-title", {i18n.t("page.payment_orders")} }
            p { class: "page-description",
                if is_admin {
                    {i18n.t("payment_orders.subtitle_admin")}
                } else {
                    {i18n.t("payment_orders.subtitle_user")}
                }
            }
        }

        // 状态筛选
        div { class: "toolbar",
            div { class: "toolbar-left",
                div { class: "filter-tabs",
                    for (val , label) in filter_labels {
                        button {
                            class: if status_filter() == val { "filter-tab active" } else { "filter-tab" },
                            r#type: "button",
                            onclick: {
                                let val = val.to_string();
                                move |_| {
                                    *status_filter.write() = val.clone();
                                    page.set(1);
                                }
                            },
                            "{label}"
                        }
                    }
                }
            }
        }

        div { class: "card",
            if is_admin {
                {
                    let (is_empty, empty_text) = match admin_orders() {
                        None => (true, i18n.t("table.loading")),
                        Some(Err(_)) => (true, i18n.t("common.load_failed")),
                        Some(Ok(ref l)) if l.is_empty() => (true, i18n.t("payment_orders.empty")),
                        _ => (false, ""),
                    };
                    let admin_start = (page() as usize - 1) * PAGE_SIZE;
                    rsx! {
                        Table { empty: is_empty, empty_text: empty_text.to_string(), col_count: 5,
                            thead {
                                tr {
                                    TableHead { {i18n.t("payments.order_no")} }
                                    TableHead { {i18n.t("payment_orders.col_user")} }
                                    TableHead { {i18n.t("common.amount")} }
                                    TableHead { {i18n.t("table.status")} }
                                    TableHead { {i18n.t("table.created_at")} }
                                }
                            }
                            tbody {
                                if let Some(Ok(ref list)) = admin_orders() {
                                    for o in list.iter().skip(admin_start).take(PAGE_SIZE) {
                                        tr {
                                            td {
                                                code { "{o.out_trade_no}" }
                                            }
                                            td {
                                                {
                                                    let uid = o.user_id.clone();
                                                    let short = format!("{}\u{2026}", &uid[..uid.len().min(8)]);
                                                    rsx! {
                                                        span {
                                                            title: "{uid}",
                                                            style: "cursor:help;font-family:monospace;font-size:13px;",
                                                            "{short}"
                                                        }
                                                    }
                                                }
                                            }
                                            td { "¥{o.amount}" }
                                            td {
                                                Badge { variant: status_to_variant(&o.status), "{o.status}" }
                                            }
                                            td { {format_time(&o.created_at)} }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
                {
                    let (is_empty, empty_text) = match my_orders() {
                        None => (true, i18n.t("table.loading")),
                        Some(Err(_)) => (true, i18n.t("common.load_failed")),
                        Some(Ok(ref l)) if l.is_empty() => (true, i18n.t("payment_orders.empty")),
                        _ => (false, ""),
                    };
                    let my_start = (page() as usize - 1) * PAGE_SIZE;
                    rsx! {
                        Table { empty: is_empty, empty_text: empty_text.to_string(), col_count: 5,
                            thead {
                                tr {
                                    TableHead { {i18n.t("payments.order_no")} }
                                    TableHead { {i18n.t("common.amount")} }
                                    TableHead { {i18n.t("payments.subject")} }
                                    TableHead { {i18n.t("table.status")} }
                                    TableHead { {i18n.t("table.created_at")} }
                                }
                            }
                            tbody {
                                if let Some(Ok(ref list)) = my_orders() {
                                    for o in list.iter().skip(my_start).take(PAGE_SIZE) {
                                        tr {
                                            td {
                                                code { "{o.out_trade_no}" }
                                            }
                                            td { "¥{o.amount}" }
                                            td { "{o.subject}" }
                                            td {
                                                Badge { variant: status_to_variant(&o.status), "{o.status}" }
                                            }
                                            td { {format_time(&o.created_at)} }
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
            let total = if is_admin {
                admin_orders().and_then(|r| r.ok()).map(|l| l.len()).unwrap_or(0)
            } else {
                my_orders().and_then(|r| r.ok()).map(|l| l.len()).unwrap_or(0)
            };
            let total_pages = total.div_ceil(PAGE_SIZE).max(1) as u32;
            rsx! {
                div { class: "pagination",
                    span { class: "pagination-info",
                        {i18n.t_with_args("payment_orders.pagination", &[("total", &total.to_string())])}
                    }
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

fn status_to_variant(status: &str) -> BadgeVariant {
    match status {
        "paid" | "success" => BadgeVariant::Success,
        "pending" | "processing" => BadgeVariant::Warning,
        "failed" | "cancelled" => BadgeVariant::Error,
        _ => BadgeVariant::Neutral,
    }
}
