use dioxus::prelude::*;

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::{
    api_client::{user_error_message, with_auto_refresh},
    console_service, payment_service,
};
use crate::stores::auth_store::AuthStore;
use crate::stores::user_store::UserStore;
use crate::utils::display::usage_status_label;
use crate::utils::time::format_time;
use crate::views::tenant::common::TenantRequiredPage;

#[derive(Clone)]
struct TrendPoint {
    label: String,
    requests: i64,
}

#[component]
pub fn Dashboard() -> Element {
    rsx! { TenantRequiredPage { DashboardContent {} } }
}

#[component]
fn DashboardContent() -> Element {
    let i18n = use_i18n();
    let user_store = use_context::<UserStore>();
    let auth_store = use_context::<AuthStore>();
    let user_info = user_store.info.read().clone();

    let overview = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            console_service::dashboard(&token).await
        })
        .await
    });
    let usage_stats = move || overview().map(|result| result.map(|value| value.stats));

    let balance = use_resource(move || {
        let auth = auth_store.clone();
        async move {
            with_auto_refresh(auth, |token| async move {
                payment_service::get_balance(&token).await
            })
            .await
        }
    });

    let greeting = if let Some(ref u) = user_info {
        format!(
            "{}，{}",
            i18n.t("dashboard.greeting"),
            u.name.as_deref().unwrap_or(&u.email)
        )
    } else {
        i18n.t("dashboard.greeting").to_string()
    };

    let api_call_value = usage_stats()
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .map(|stats| crate::utils::format_count(stats.total_requests))
        .unwrap_or_else(|| "—".to_string());

    let total_cost_value = usage_stats()
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .map(|stats| {
            format!(
                "¥{}",
                crate::utils::format_precise_money_str(&stats.total_cost)
            )
        })
        .unwrap_or_else(|| "—".to_string());

    let balance_value = balance()
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .map(|balance| {
            format!(
                "¥{}",
                crate::utils::format_money_str(&balance.available_balance)
            )
        })
        .unwrap_or_else(|| "—".to_string());

    let summary = overview().and_then(|result| result.ok());
    let summary_ready = summary.is_some();
    let summary_error = overview()
        .and_then(Result::err)
        .map(|error| user_error_message(i18n, &error));
    let snapshot_time = summary.as_ref().map(|value| format_time(&value.as_of));
    let active_key_value = summary
        .as_ref()
        .map(|value| value.active_key_count.to_string())
        .unwrap_or_else(|| "—".into());
    let active_keys = summary
        .as_ref()
        .map(|value| value.active_keys.clone())
        .unwrap_or_default();
    let recent_usage = summary
        .as_ref()
        .map(|value| value.recent_usage.clone())
        .unwrap_or_default();
    let recent_trend = summary
        .as_ref()
        .map(|value| {
            value
                .trend
                .buckets
                .iter()
                .map(|bucket| TrendPoint {
                    label: bucket.start.get(5..10).unwrap_or(&bucket.start).to_string(),
                    requests: bucket.requests,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let trend_svg = build_trend_svg(&recent_trend);

    rsx! {
        div { class: "page-container dashboard-console",
            div { class: "page-header dashboard-console-header",
                div {
                    h1 { class: "page-title dashboard-console-title", "{greeting}" }
                    p { class: "page-subtitle dashboard-console-subtitle", {i18n.t("dashboard.subtitle_long")} }
                }
            }

            if let Some(error) = summary_error {
                p { class: "alert alert-error", role: "alert", "{error}" }
            }
            if let Some(time) = snapshot_time {
                p { class: "text-secondary", {format!("{}: {}", i18n.t("common.display_snapshot"), time)} }
            }
            div { class: "dashboard-stats-row",
                DashboardStatCard {
                    tone: "blue",
                    icon: "◎",
                    title: i18n.t("dashboard.api_calls").to_string(),
                    value: api_call_value,
                    meta: i18n.t("dashboard.meta_usage").to_string()
                }
                DashboardStatCard {
                    tone: "green",
                    icon: "¥",
                    title: i18n.t("dashboard.balance_available").to_string(),
                    value: balance_value,
                    meta: i18n.t("dashboard.meta_balance").to_string()
                }
                DashboardStatCard {
                    tone: "orange",
                    icon: "K",
                    title: i18n.t("dashboard.active_keys").to_string(),
                    value: active_key_value,
                    meta: i18n.t("dashboard.meta_keys").to_string()
                }
                DashboardStatCard {
                    tone: "violet",
                    icon: "↗",
                    title: i18n.t("dashboard.total_cost").to_string(),
                    value: total_cost_value,
                    meta: i18n.t("dashboard.meta_cost").to_string()
                }
            }

            div { class: "dashboard-panel dashboard-quick-links-panel",
                div { class: "dashboard-panel-head",
                    div {
                        h2 { class: "dashboard-panel-title", {i18n.t("dashboard.quick_links")} }
                        p { class: "dashboard-panel-copy", {i18n.t("dashboard.quick_links_desc")} }
                    }
                }
                div { class: "dashboard-panel-body dashboard-quick-links",
                    Link { class: "dashboard-quick-link dashboard-quick-link-blue", to: Route::ApiKeyList {},
                        div { class: "dashboard-quick-link-main",
                            div { class: "dashboard-quick-link-title", {i18n.t("dashboard.manage_api_keys")} }
                            div { class: "dashboard-quick-link-desc", {i18n.t("dashboard.manage_api_keys_desc")} }
                        }
                        span { class: "dashboard-quick-link-arrow", aria_hidden: "true", "→" }
                    }
                    Link { class: "dashboard-quick-link dashboard-quick-link-green", to: Route::PaymentsOverview {},
                        div { class: "dashboard-quick-link-main",
                            div { class: "dashboard-quick-link-title", {i18n.t("dashboard.payments")} }
                            div { class: "dashboard-quick-link-desc", {i18n.t("dashboard.payments_desc")} }
                        }
                        span { class: "dashboard-quick-link-arrow", aria_hidden: "true", "→" }
                    }
                    Link { class: "dashboard-quick-link dashboard-quick-link-orange", to: Route::Usage {},
                        div { class: "dashboard-quick-link-main",
                            div { class: "dashboard-quick-link-title", {i18n.t("dashboard.usage_details")} }
                            div { class: "dashboard-quick-link-desc", {i18n.t("dashboard.usage_details_desc")} }
                        }
                        span { class: "dashboard-quick-link-arrow", aria_hidden: "true", "→" }
                    }
                }
            }

            div { class: "dashboard-two-col",
                div { class: "dashboard-panel dashboard-panel-primary",
                    div { class: "dashboard-panel-head",
                        div {
                            h2 { class: "dashboard-panel-title", {i18n.t("dashboard.recent_active_days")} }
                            p { class: "dashboard-panel-copy", {i18n.t("dashboard.recent_active_days_desc")} }
                        }
                        span { class: "dashboard-panel-tag", {i18n.t("dashboard.live_data")} }
                    }
                    div { class: "dashboard-panel-body" ,
                        div { class: "dashboard-trend-chart",
                            svg {
                                class: "dashboard-trend-svg",
                                role: "img",
                                view_box: "0 0 640 220",
                                preserve_aspect_ratio: "none",
                                title { {i18n.t("dashboard.recent_active_days_desc")} }
                                defs {
                                    linearGradient { id: "dashboardTrendFill", x1: "0", y1: "0", x2: "0", y2: "1",
                                        stop { offset: "0%", stop_color: "rgba(71, 132, 255, 0.30)" }
                                        stop { offset: "100%", stop_color: "rgba(71, 132, 255, 0.02)" }
                                    }
                                }
                                rect { class: "dashboard-trend-plot", x: "12", y: "10", width: "616", height: "184", rx: "18" }
                                for guide in trend_svg.guides.iter() {
                                    line {
                                        class: "dashboard-trend-guide",
                                        x1: "24",
                                        y1: "{guide}",
                                        x2: "616",
                                        y2: "{guide}"
                                    }
                                }
                                path { class: "dashboard-trend-area", d: "{trend_svg.area_path}" }
                                path { class: "dashboard-trend-line", d: "{trend_svg.line_path}" }
                                for (index, point) in recent_trend.iter().enumerate() {
                                    circle {
                                        class: if point.requests > 0 {
                                            "dashboard-trend-point dashboard-trend-point-active"
                                        } else {
                                            "dashboard-trend-point"
                                        },
                                        cx: "{trend_svg.x_points[index]}",
                                        cy: "{trend_svg.y_points[index]}",
                                        r: if point.requests > 0 { "4.5" } else { "3.2" }
                                    }
                                    if point.requests > 0 {
                                        text {
                                            class: "dashboard-trend-peak",
                                            x: "{trend_svg.x_points[index]}",
                                            y: "{trend_svg.y_points[index] - 12.0}",
                                            text_anchor: "middle",
                                            "{point.requests}"
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "dashboard-trend-labels",
                            for point in recent_trend.iter() {
                                div {
                                    class: if point.requests > 0 {
                                        "dashboard-trend-label-item dashboard-trend-label-item-active"
                                    } else {
                                        "dashboard-trend-label-item"
                                    },
                                    span { class: "dashboard-trend-label", "{point.label}" }
                                    span { class: "dashboard-trend-value", "{point.requests}" }
                                }
                            }
                        }
                    }
                }

            }

            div { class: "dashboard-bottom-row",
                div { class: "dashboard-panel",
                    div { class: "dashboard-panel-head",
                        div {
                            h2 { class: "dashboard-panel-title", {i18n.t("dashboard.recent_calls")} }
                            p { class: "dashboard-panel-copy", {i18n.t("dashboard.recent_calls_desc")} }
                        }
                    }
                    div { class: "dashboard-panel-body" ,
                        if !summary_ready {
                            p { class: "dashboard-empty-copy", {i18n.t("common.loading")} }
                        } else if recent_usage.is_empty() {
                            p { class: "dashboard-empty-copy", {i18n.t("dashboard.no_recent_calls")} }
                        } else {
                            div { class: "dashboard-activity-table",
                                div { class: "dashboard-activity-row dashboard-activity-row-head",
                                    div { {i18n.t("usage.model")} }
                                    div { {i18n.t("table.status")} }
                                    div { {i18n.t("common.time")} }
                                    div { {i18n.t("common.cost")} }
                                }
                                for record in recent_usage.iter() {
                                    div { class: "dashboard-activity-row",
                                        div {
                                            div { class: "dashboard-activity-main", "{record.model}" }
                                            div { class: "dashboard-activity-sub", "#{record.request_id.chars().take(8).collect::<String>()}" }
                                        }
                                        div {
                                            span {
                                                class: if record.status == "success" {
                                                    "dashboard-inline-status dashboard-inline-status-ok"
                                                } else {
                                                    "dashboard-inline-status dashboard-inline-status-warn"
                                                },
                                                {usage_status_label(&record.status, &i18n)}
                                            }
                                        }
                                        div { class: "dashboard-activity-time", { format_time(&record.created_at) } }
                                        div { class: "dashboard-activity-time", "¥{crate::utils::format_precise_money_str(&record.cost)}" }
                                    }
                                }
                            }
                        }
                    }
                }

                div { class: "dashboard-panel",
                    div { class: "dashboard-panel-head",
                        div {
                            h2 { class: "dashboard-panel-title", {i18n.t("dashboard.active_keys_panel")} }
                            p { class: "dashboard-panel-copy", {i18n.t("dashboard.active_keys_panel_desc")} }
                        }
                    }
                    div { class: "dashboard-panel-body" ,
                        if !summary_ready {
                            p { class: "dashboard-empty-copy", {i18n.t("common.loading")} }
                        } else if active_keys.is_empty() {
                            p { class: "dashboard-empty-copy", {i18n.t("dashboard.no_active_keys")} }
                        } else {
                            div { class: "dashboard-key-list",
                                for key in active_keys.iter() {
                                    div { class: "dashboard-key-item",
                                        span { class: "dashboard-key-dot" }
                                        div { class: "dashboard-key-main",
                                            div { class: "dashboard-key-name", "{key.name}" }
                                            div { class: "dashboard-key-meta",
                                                code { class: "dashboard-key-code", "{key.key_preview}" }
                                                span { class: "dashboard-key-time",
                                                    { format_active_key_time(
                                                        key.last_used_at.as_deref(),
                                                        i18n.t("dashboard.last_used_prefix"),
                                                        i18n.t("dashboard.no_usage_record"),
                                                    ) }
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
}

struct TrendSvg {
    line_path: String,
    area_path: String,
    x_points: Vec<f32>,
    y_points: Vec<f32>,
    guides: Vec<f32>,
}

fn build_trend_svg(points: &[TrendPoint]) -> TrendSvg {
    let width = 640.0_f32;
    let height = 220.0_f32;
    let left = 24.0_f32;
    let right = 24.0_f32;
    let top = 24.0_f32;
    let bottom = 34.0_f32;
    let plot_width = width - left - right;
    let baseline_y = height - bottom;
    let plot_height = baseline_y - top;
    let max_value = points
        .iter()
        .map(|point| point.requests)
        .max()
        .unwrap_or(0)
        .max(4) as f32;
    let step = if points.len() > 1 {
        plot_width / (points.len() - 1) as f32
    } else {
        plot_width
    };

    let x_points: Vec<f32> = points
        .iter()
        .enumerate()
        .map(|(index, _)| left + step * index as f32)
        .collect();
    let y_points: Vec<f32> = points
        .iter()
        .map(|point| baseline_y - ((point.requests as f32 / max_value) * plot_height))
        .collect();

    let line_path = x_points
        .iter()
        .zip(y_points.iter())
        .enumerate()
        .map(|(index, (x, y))| {
            if index == 0 {
                format!("M{:.2},{:.2}", x, y)
            } else {
                format!(" L{:.2},{:.2}", x, y)
            }
        })
        .collect::<String>();

    let first_x = x_points.first().copied().unwrap_or(left);
    let last_x = x_points.last().copied().unwrap_or(width - right);
    let area_path = format!(
        "{} L{:.2},{:.2} L{:.2},{:.2} Z",
        line_path, last_x, baseline_y, first_x, baseline_y
    );

    let guides = (0..4)
        .map(|index| top + plot_height * (index as f32 / 3.0))
        .collect();

    TrendSvg {
        line_path,
        area_path,
        x_points,
        y_points,
        guides,
    }
}

fn format_active_key_time(
    last_used_at: Option<&str>,
    last_used_prefix: &str,
    empty_text: &str,
) -> String {
    last_used_at
        .map(|last_used| format!("{last_used_prefix} {}", format_time(last_used)))
        .unwrap_or_else(|| empty_text.to_string())
}

#[component]
fn DashboardStatCard(
    tone: String,
    icon: String,
    title: String,
    value: String,
    meta: String,
) -> Element {
    rsx! {
        div { class: "dashboard-stat-card dashboard-stat-card-{tone}",
            div { class: "dashboard-stat-top",
                div { class: "dashboard-stat-icon", "{icon}" }
                span { class: "dashboard-stat-kicker", "{title}" }
            }
            div { class: "dashboard-stat-value", "{value}" }
            div { class: "dashboard-stat-meta", "{meta}" }
        }
    }
}
