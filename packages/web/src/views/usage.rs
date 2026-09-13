use dioxus::prelude::*;
use ui::{LineChart, LineSeriesData, PageHeader, Pagination, Table, TableHead};

const PAGE_SIZE: usize = 20;

use crate::hooks::use_i18n::use_i18n;
use crate::services::{api_client::with_auto_refresh, usage_service};
use crate::stores::auth_store::AuthStore;
use crate::utils::resource::{KeyedResourceValue, current_keyed_value};
use crate::utils::time::format_time;
use std::collections::HashMap;

/// 用量统计页面 - /usage
#[component]
pub fn Usage() -> Element {
    let i18n = use_i18n();
    let auth_store = use_context::<AuthStore>();
    let mut page = use_signal(|| 1u32);
    let mut page_size = use_signal(|| PAGE_SIZE as u32);

    // 汇总统计
    let stats = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            usage_service::stats(&token).await
        })
        .await
    });

    // 明细记录（服务端分页）
    let records = use_resource(move || async move {
        let request_key = (page(), page_size());
        let result = with_auto_refresh(auth_store, |token| async move {
            usage_service::list_page(
                &client_api::api::usage::UsageQueryParams::new()
                    .with_page(request_key.0 as i32)
                    .with_page_size(request_key.1 as i32),
                &token,
            )
            .await
        })
        .await;
        KeyedResourceValue::new(request_key, result)
    });

    // 趋势图保持展示最近一段窗口，不随表格翻页而改变。
    let trend_records = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            usage_service::list_page(
                &client_api::api::usage::UsageQueryParams::new()
                    .with_page(1)
                    .with_page_size(100),
                &token,
            )
            .await
        })
        .await
    });

    // 折线图：按日期聚合调用次数
    let (chart_x, chart_series) = match trend_records() {
        Some(Ok(ref result)) => {
            let mut by_date: HashMap<String, f64> = HashMap::new();
            for r in &result.records {
                let date = r.created_at.get(..10).unwrap_or("").to_string();
                *by_date.entry(date).or_default() += 1.0;
            }
            let mut pairs: Vec<(String, f64)> = by_date.into_iter().collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            let x: Vec<String> = pairs.iter().map(|(d, _)| d.clone()).collect();
            let y: Vec<f64> = pairs.iter().map(|(_, v)| *v).collect();
            (
                x,
                vec![LineSeriesData {
                    name: i18n.t("usage.calls").to_string(),
                    data: y,
                }],
            )
        }
        _ => (vec![], vec![]),
    };

    rsx! {
        div {
            class: "page-container",
            PageHeader {
                title: i18n.t("page.usage").to_string(),
                description: i18n.t("usage.subtitle").to_string(),
            }

            // 汇总卡片
            div { class: "stats-grid",
                match stats() {
                    None => rsx! { p { {i18n.t("table.loading")} } },
                    Some(Err(e)) => rsx! { p { "{i18n.t(\"common.load_failed\")}：{e}" } },
                    Some(Ok(s)) => rsx! {
                        div { class: "stat-card",
                            div { class: "stat-body",
                                p { class: "stat-title", {i18n.t("usage.total_calls")} }
                                p { class: "stat-value", "{s.total_requests}" }
                                p { class: "stat-label", "{i18n.t(\"usage.period\")}：{s.period}" }
                            }
                        }
                        div { class: "stat-card",
                            div { class: "stat-body",
                                p { class: "stat-title", {i18n.t("usage.total_tokens")} }
                                p { class: "stat-value", "{s.total_tokens}" }
                                p { class: "stat-label",
                                    "{i18n.t(\"usage.prompt_tokens\")}：{s.total_prompt_tokens} / {i18n.t(\"usage.completion_tokens\")}：{s.total_completion_tokens}"
                                }
                            }
                        }
                        div { class: "stat-card",
                            div { class: "stat-body",
                                p { class: "stat-title", {i18n.t("usage.total_cost")} }
                                p { class: "stat-value", "¥{s.total_cost:.4}" }
                                p { class: "stat-label", {i18n.t("usage.usage_billed")} }
                            }
                        }
                    },
                }
            }

            // 调用趋势折线图
            if !chart_x.is_empty() {
                div { class: "section",
                    h2 { class: "section-title", {i18n.t("usage.trend")} }
                    div { class: "chart-container",
                        LineChart {
                            id: "usage-line-chart",
                            title: "",
                            x_data: chart_x,
                            series: chart_series,
                            width: 800,
                            height: 300,
                        }
                    }
                }
            }

            // 明细记录表格
            div { class: "section",
                h2 { class: "section-title", {i18n.t("usage.records")} }
                {
                    let request_key = (page(), page_size());
                    let current_records = current_keyed_value(
                        &request_key,
                        records.state().cloned(),
                        records(),
                    );
                    match current_records {
                    None => rsx! { p { class: "loading-text", {i18n.t("table.loading")} } },
                    Some(Err(e)) => rsx! { p { class: "error-text", "{i18n.t(\"common.load_failed\")}：{e}" } },
                    Some(Ok(result)) if result.records.is_empty() => rsx! {
                        p { class: "empty-text", {i18n.t("usage.no_records")} }
                        Pagination {
                            current: page(),
                            total_pages: result.total_pages.max(1) as u32,
                            total: result.total.max(0) as u64,
                            page_size: page_size(),
                            summary: i18n.t_with_args(
                                "common.pagination_summary",
                                &[
                                    ("total", &result.total.max(0).to_string()),
                                    ("current", &page().to_string()),
                                    ("total_pages", &result.total_pages.max(1).to_string()),
                                ],
                            ),
                            page_size_label: i18n.t("common.pagination_page_size").to_string(),
                            page_size_suffix: i18n.t("pricing.items_suffix").to_string(),
                            previous_label: i18n.t("table.previous").to_string(),
                            next_label: i18n.t("table.next").to_string(),
                            on_page_change: move |p| page.set(p),
                            on_page_size_change: move |size| { page_size.set(size); page.set(1); },
                        }
                    },
                    Some(Ok(result)) => rsx! {
                        Table {
                            class: "data-table".to_string(),
                            col_count: 6,
                                thead {
                                    tr {
                                        TableHead { {i18n.t("common.time")} }
                                        TableHead { {i18n.t("usage.model")} }
                                        TableHead { {i18n.t("usage.prompt_tokens")} }
                                        TableHead { {i18n.t("usage.completion_tokens")} }
                                        TableHead { {i18n.t("usage.total_token")} }
                                        TableHead { {i18n.t("common.cost")} }
                                    }
                                }
                                tbody {
                                    for r in result.records.iter() {
                                                tr {
                                                    td { { format_time(&r.created_at) } }
                                                    td { "{r.model}" }
                                                    td { "{r.prompt_tokens}" }
                                                    td { "{r.completion_tokens}" }
                                                    td { "{r.total_tokens}" }
                                                    td {
                                                        {
                                                            if r.cost > 0.0 {
                                                                format!("¥{:.6}", r.cost)
                                                            } else {
                                                                "—".to_string()
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                }
                        }
                        {
                            rsx! {
                                Pagination {
                                    current: page(),
                                    total_pages: result.total_pages.max(1) as u32,
                                    total: result.total.max(0) as u64,
                                    page_size: page_size(),
                                    summary: i18n.t_with_args(
                                        "common.pagination_summary",
                                        &[
                                            ("total", &result.total.max(0).to_string()),
                                            ("current", &page().to_string()),
                                            ("total_pages", &result.total_pages.max(1).to_string()),
                                        ],
                                    ),
                                    page_size_label: i18n.t("common.pagination_page_size").to_string(),
                                    page_size_suffix: i18n.t("pricing.items_suffix").to_string(),
                                    previous_label: i18n.t("table.previous").to_string(),
                                    next_label: i18n.t("table.next").to_string(),
                                    on_page_change: move |p| page.set(p),
                                    on_page_size_change: move |size| { page_size.set(size); page.set(1); },
                                }
                            }
                        }
                    },
                    }
                }
            }
        }
    }
}
