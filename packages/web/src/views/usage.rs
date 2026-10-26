use dioxus::prelude::*;
use ui::{LineChart, LineSeriesData, PageHeader, Pagination, Table, TableHead};

const PAGE_SIZE: usize = 20;

use crate::hooks::use_i18n::use_i18n;
use crate::services::{api_client::with_auto_refresh, usage_service};
use crate::stores::auth_store::AuthStore;
use crate::utils::resource::{KeyedResourceValue, current_keyed_value};
use crate::utils::time::format_time;
use crate::views::tenant::common::TenantRequiredPage;

/// 用量统计页面 - /usage
#[component]
pub fn Usage() -> Element {
    rsx! { TenantRequiredPage { UsageContent {} } }
}

#[component]
fn UsageContent() -> Element {
    let i18n = use_i18n();
    let auth_store = use_context::<AuthStore>();
    let mut page = use_signal(|| 1u32);
    let mut page_size = use_signal(|| PAGE_SIZE as u32);
    let mut start_date = use_signal(String::new);
    let mut end_date = use_signal(String::new);
    let mut model = use_signal(String::new);
    let mut applied_filters = use_signal(|| (String::new(), String::new(), String::new()));

    // 汇总统计
    let stats = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            usage_service::stats(&token).await
        })
        .await
    });

    // 明细记录（服务端分页）
    let records = use_resource(move || async move {
        let filters = applied_filters();
        let request_key = (page(), page_size(), filters.0, filters.1, filters.2);
        let query_key = request_key.clone();
        let result = with_auto_refresh(auth_store, move |token| {
            let query_key = query_key.clone();
            async move {
                let mut query = client_api::api::usage::UsageQueryParams::new()
                    .with_page(query_key.0 as i32)
                    .with_page_size(query_key.1 as i32);
                if !query_key.2.is_empty() {
                    query = query.with_start_date(query_key.2);
                }
                if !query_key.3.is_empty() {
                    query = query.with_end_date(query_key.3);
                }
                if !query_key.4.is_empty() {
                    query = query.with_model(query_key.4);
                }
                usage_service::list_page(&query, &token).await
            }
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
    let trend_summary = trend().and_then(|result| result.ok()).map(|value| {
        let total = value
            .buckets
            .iter()
            .map(|bucket| bucket.requests)
            .sum::<i64>();
        let peak = value.buckets.iter().max_by_key(|bucket| bucket.requests);
        let (peak_day, peak_calls) = peak
            .map(|bucket| {
                (
                    bucket.start.get(..10).unwrap_or(&bucket.start).to_string(),
                    bucket.requests,
                )
            })
            .unwrap_or_else(|| ("—".to_string(), 0));
        i18n.t_with_args(
            "usage.trend_summary",
            &[
                ("total", &total.to_string()),
                ("day", &peak_day),
                ("calls", &peak_calls.to_string()),
            ],
        )
    });

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
                        p { {format!("{}：{}", i18n.t("common.load_failed"), crate::services::api_client::user_error_message(i18n, &e))} }
                    },
                    Some(Ok(s)) => rsx! {
                        div { class: "stat-card",
                            div { class: "stat-body",
                                p { class: "stat-title", {i18n.t("usage.total_calls")} }
                                p { class: "stat-value", {crate::utils::format_count(s.total_requests)} }
                                p { class: "stat-label", "{i18n.t(\"usage.period\")}：{s.period}" }
                            }
                        }
                        div { class: "stat-card",
                            div { class: "stat-body",
                                p { class: "stat-title", {i18n.t("usage.total_tokens")} }
                                p { class: "stat-value", {crate::utils::format_count(s.total_tokens)} }
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
                p { class: "alert alert-error", role: "alert", {format!("{}: {}", i18n.t("common.load_failed"), crate::services::api_client::user_error_message(i18n, &error))} }
            }
            if let Some(Ok(value)) = trend() {
                p { class: "text-secondary", {format!("{}: {}", i18n.t("common.display_snapshot"), format_time(&value.as_of))} }
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
                    if let Some(summary) = trend_summary.as_ref() {
                        p { class: "text-secondary", {summary.clone()} }
                    }
                }
            }

            section { class: "section", aria_label: i18n.t("usage.filters"),
                form {
                    class: "filter-bar",
                    onsubmit: move |event| {
                        event.prevent_default();
                        applied_filters.set((
                            start_date().trim().to_string(),
                            end_date().trim().to_string(),
                            model().trim().to_string(),
                        ));
                        page.set(1);
                    },
                    label { class: "form-field",
                        span { {i18n.t("usage.start_date")} }
                        input {
                            class: "input",
                            r#type: "date",
                            value: start_date(),
                            oninput: move |event| start_date.set(event.value()),
                        }
                    }
                    label { class: "form-field",
                        span { {i18n.t("usage.end_date")} }
                        input {
                            class: "input",
                            r#type: "date",
                            value: end_date(),
                            oninput: move |event| end_date.set(event.value()),
                        }
                    }
                    label { class: "form-field",
                        span { {i18n.t("usage.model")} }
                        input {
                            class: "input",
                            value: model(),
                            placeholder: i18n.t("usage.model_placeholder"),
                            oninput: move |event| model.set(event.value()),
                        }
                    }
                    div { class: "filter-actions",
                        button { class: "btn btn-primary", r#type: "submit", {i18n.t("common.search")} }
                        button {
                            class: "btn btn-secondary",
                            r#type: "button",
                            onclick: move |_| {
                                start_date.set(String::new());
                                end_date.set(String::new());
                                model.set(String::new());
                                applied_filters.set((String::new(), String::new(), String::new()));
                                page.set(1);
                            },
                            {i18n.t("common.clear")}
                        }
                    }
                }
                p { class: "text-secondary", {i18n.t("usage.timezone_hint")} }
            }

            // 明细记录表格
            {
                let filters = applied_filters();
                let request_key = (page(), page_size(), filters.0, filters.1, filters.2);
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
                                p { class: "error-text", {format!("{}：{}", i18n.t("common.load_failed"), crate::services::api_client::user_error_message(i18n, &e))} }
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
                                                td { {crate::utils::format_count(r.prompt_tokens)} }
                                                td { {crate::utils::format_count(r.completion_tokens)} }
                                                td { {crate::utils::format_count(r.total_tokens)} }
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
