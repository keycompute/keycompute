use dioxus::prelude::*;
use ui::{LineChart, LineSeriesData, PageHeader, Pagination, Table, TableHead};

const PAGE_SIZE: usize = 20;

use crate::hooks::use_i18n::use_i18n;
use crate::services::{api_client::with_auto_refresh, usage_service};
use crate::stores::auth_store::AuthStore;
use crate::utils::resource::{KeyedResourceValue, current_keyed_value};
use crate::utils::time::format_time;

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

    // Aggregate the complete seven-day UTC window, independent of pagination.
    let trend = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            crate::services::console_service::trend(&token).await
        })
        .await
    });
    let (chart_x, chart_series) = match trend() {
        Some(Ok(value)) => (
            value
                .buckets
                .iter()
                .map(|b| b.start.get(..10).unwrap_or(&b.start).to_string())
                .collect::<Vec<_>>(),
            vec![LineSeriesData {
                name: i18n.t("usage.calls").to_string(),
                data: value.buckets.iter().map(|b| b.requests as f64).collect(),
            }],
        ),
        _ => (vec![], vec![]),
    };

    rsx! {
        div { class: "page-container",
            PageHeader {
                title: i18n.t("page.usage").to_string(),
                description: i18n.t("usage.subtitle").to_string(),
            }

            // 汇总卡片
            div { class: "stats-grid",
                match stats() {
                    None => rsx! {
                        p { {i18n.t("table.loading")} }
                    },
                    Some(Err(e)) => rsx! {
                        p { "{i18n.t(\"common.load_failed\")}：{e}" }
                    },
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

            if let Some(Err(error)) = trend() {
                p { class: "alert alert-error", role: "alert", {format!("{}: {}", i18n.t("common.load_failed"), crate::services::api_client::user_error_message(&error))} }
            }
            if let Some(Ok(value)) = trend() {
                p { class: "text-secondary", {format!("UTC · {}: {}", i18n.t("common.display_snapshot"), format_time(&value.as_of))} }
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
            {
                let request_key = (page(), page_size());
                let current_records = current_keyed_value(
                    &request_key,
                    records.state().cloned(),
                    records(),
                );
                // 分页页脚与面板平级渲染（对齐定价页的页脚结构），避免页脚嵌入面板内部。
                let footer_totals = current_records
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .map(|result| (
                        result.total.max(0) as u64,
                        result.total_pages.max(1) as u32,
                    ));
                rsx! {
                    div { class: "section table-pagination-panel",
                        h2 { class: "section-title", {i18n.t("usage.records")} }
                        match current_records {
                            None => rsx! {
                                p { class: "loading-text", {i18n.t("table.loading")} }
                            },
                            Some(Err(e)) => rsx! {
                                p { class: "error-text", "{i18n.t(\"common.load_failed\")}：{e}" }
                            },
                            Some(Ok(result)) if result.records.is_empty() => rsx! {
                                p { class: "empty-text", {i18n.t("usage.no_records")} }
                            },
                            Some(Ok(result)) => rsx! {
                                Table { class: "data-table".to_string(), col_count: 6,
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
                                                td { {format_time(&r.created_at)} }
                                                td { "{r.model}" }
                                                td { "{r.prompt_tokens}" }
                                                td { "{r.completion_tokens}" }
                                                td { "{r.total_tokens}" }
                                                td { {if r.cost > 0.0 { format!("¥{:.6}", r.cost) } else { "—".to_string() }} }
                                            }
                                        }
                                    }
                                }
                            },
                        }
                    }
                    if let Some((total, total_pages)) = footer_totals {
                        Pagination {
                            current: page(),
                            total_pages,
                            total,
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
