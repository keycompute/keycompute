use client_api::api::distribution::DistributionEarnings;
use client_api::error::ClientError;
use dioxus::prelude::*;

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::{
    api_client::{user_error_message, with_auto_refresh},
    distribution_service,
};
use crate::stores::{
    auth_store::AuthStore, public_settings_store::PublicSettingsStore, ui_store::UiStore,
};
use crate::utils::resource::{KeyedResourceValue, current_keyed_value};
use crate::utils::time::format_time;
use crate::utils::{format_precise_cny_str, on_copy};
use ui::{PageHeader, Pagination, icons::IconCopy};

fn total_earnings_display(
    result: Option<Result<DistributionEarnings, ClientError>>,
    loading: &str,
) -> String {
    match result {
        Some(Ok(earnings)) => format_precise_cny_str(&earnings.total_earnings),
        Some(Err(_)) => "—".to_string(),
        None => loading.to_string(),
    }
}

fn is_distribution_disabled_error<T>(result: &Option<Result<T, ClientError>>) -> bool {
    matches!(
        result,
        Some(Err(ClientError::Forbidden(msg))) if msg.contains("Distribution is disabled")
    )
}

#[component]
pub fn DistributionOverview() -> Element {
    let i18n = use_i18n();
    let public_settings_store = use_context::<PublicSettingsStore>();
    let mut ui_store = use_context::<UiStore>();
    let nav = use_navigator();

    use_effect(move || {
        if public_settings_store.loaded() && !public_settings_store.distribution_is_enabled() {
            ui_store.show_error(i18n.t("distribution.disabled_message"));
            nav.replace(Route::Dashboard {});
        }
    });

    if !public_settings_store.loaded() {
        return rsx! {
            div { class: "page-container",
                div {
                    class: "distribution-loading",
                    style: "display:flex;align-items:center;justify-content:center;padding:64px",
                    div { style: "display:flex;align-items:center;gap:12px;color:var(--text-secondary,#64748b)",
                        div { class: "spinner", style: "width:24px;height:24px" }
                        span { {i18n.t("table.loading")} }
                    }
                }
            }
        };
    }

    if !public_settings_store.distribution_is_enabled() {
        return rsx! {};
    }

    rsx! {
        DistributionOverviewContent {}

    }
}

#[component]
fn DistributionOverviewContent() -> Element {
    let i18n = use_i18n();
    let auth_store = use_context::<AuthStore>();
    let ui_store = use_context::<UiStore>();
    let mut page = use_signal(|| 1u32);
    let mut page_size = use_signal(|| 20u32);
    // One server snapshot supplies earnings, counts and the stable invite link.
    let overview = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            distribution_service::overview(&token).await
        })
        .await
    });
    let earnings = move || overview().map(|result| result.map(|value| value.earnings));
    let referral_code = move || overview().map(|result| result.map(|value| value.referral));
    let as_of = overview()
        .and_then(|result| result.ok())
        .map(|value| value.as_of);

    // Fetch only the selected server page and fence late page completions.
    let referrals = use_resource(move || async move {
        let request_key = (page(), page_size());
        let result = with_auto_refresh(auth_store, |token| async move {
            distribution_service::get_referrals_page(request_key.0, request_key.1, &token).await
        })
        .await;
        KeyedResourceValue::new(request_key, result)
    });
    let referral_result = current_keyed_value(
        &(page(), page_size()),
        referrals.state().cloned(),
        referrals(),
    );

    let total_earnings = total_earnings_display(earnings(), i18n.t("table.loading"));
    let available_earnings = match earnings() {
        Some(Ok(ref e)) => format_precise_cny_str(&e.available_earnings),
        _ => "—".to_string(),
    };
    let pending_earnings = match earnings() {
        Some(Ok(ref e)) => format_precise_cny_str(&e.pending_earnings),
        _ => "—".to_string(),
    };
    let referral_count = match earnings() {
        Some(Ok(ref e)) => e.referral_count.to_string(),
        _ => "—".to_string(),
    };
    let invite_link = match referral_code() {
        Some(Ok(ref r)) => r.referral_link.clone(),
        Some(Err(_)) => "—".to_string(),
        None => i18n.t("table.loading").to_string(),
    };
    let link_ready = matches!(referral_code(), Some(Ok(_)));
    let invite_link_text = invite_link.clone();
    let copied = use_signal(|| false);
    let copied_label = i18n.t("common.copied");
    let copy_label = i18n.t("common.copy");
    let copy_manual_hint = i18n.t("common.copy_manual_hint");
    let distribution_disabled = is_distribution_disabled_error(&earnings())
        || is_distribution_disabled_error(&referral_code())
        || is_distribution_disabled_error(&referral_result);
    let page_error = earnings()
        .and_then(Result::err)
        .or_else(|| referral_code().and_then(Result::err))
        .or_else(|| referral_result.clone().and_then(Result::err));
    let page_error_message = page_error.as_ref().map(|error| {
        if error.is_rate_limited() {
            i18n.t("common.rate_limited_hint").to_string()
        } else {
            user_error_message(error)
        }
    });
    let referral_total = referral_result
        .as_ref()
        .and_then(|value| value.as_ref().ok())
        .map(|value| value.total.max(0) as u64)
        .unwrap_or(0);
    let referral_total_pages = referral_result
        .as_ref()
        .and_then(|value| value.as_ref().ok())
        .map(|value| value.total_pages.clamp(1, u32::MAX as i64) as u32)
        .unwrap_or(1);

    rsx! {
        div { class: "page-container",
            PageHeader {
                title: i18n.t("distribution.title").to_string(),
                description: i18n.t("distribution.subtitle").to_string(),
            }

            if let Some(as_of) = as_of {
                p { class: "text-secondary", {format!("{}: {}", i18n.t("common.display_snapshot"), format_time(&as_of))} }
            }
            if !distribution_disabled {
                if let Some(message) = page_error_message {
                    div { class: "alert alert-error", role: "alert", "{message}" }
                }
            }

            if distribution_disabled {
                div { class: "card",
                    div { class: "card-body",
                        div { class: "empty-state",
                            div { class: "empty-icon", "⛔" }
                            h3 { class: "empty-title", {i18n.t("distribution.disabled_title")} }
                            p { class: "empty-text", {i18n.t("distribution.disabled_desc")} }
                        }
                    }
                }
            } else {
                // 收益统计
                div { class: "stats-grid",
                    div { class: "stat-card card",
                        div { class: "card-body",
                            p { class: "stat-label", {i18n.t("distribution.total_earnings")} }
                            p { class: "stat-value", "{total_earnings}" }
                        }
                    }
                    div { class: "stat-card card",
                        div { class: "card-body",
                            p { class: "stat-label", {i18n.t("distribution.settled_earnings")} }
                            p { class: "stat-value", "{available_earnings}" }
                        }
                    }
                    div { class: "stat-card card",
                        div { class: "card-body",
                            p { class: "stat-label", {i18n.t("distribution.pending")} }
                            p { class: "stat-value", "{pending_earnings}" }
                        }
                    }
                    div { class: "stat-card card",
                        div { class: "card-body",
                            p { class: "stat-label", {i18n.t("distribution.referral_count")} }
                            p { class: "stat-value", "{referral_count}" }
                        }
                    }
                }

                // 我的邀请链接
                div { class: "card",
                    div { class: "card-header",
                        h3 { class: "card-title", {i18n.t("distribution.my_invite_link")} }
                    }
                    div { class: "card-body",
                        // 链接就绪时才渲染复制块，避免加载中/错误文案以邀请链接样式展示误导用户
                        if link_ready {
                            div { class: "distribution-invite-copy-section",
                                div { class: "kc-api-copy-block",
                                    pre {
                                        class: if copied() { "kc-api-example copied" } else { "kc-api-example" },
                                        title: if copied() { copied_label } else { copy_label },
                                        "{invite_link_text}"
                                    }
                                    button {
                                        class: "kc-api-copy-button",
                                        r#type: "button",
                                        onclick: on_copy(invite_link_text.clone(), copy_manual_hint.to_string(), ui_store, copied),
                                        IconCopy { size: 15 }
                                        if copied() {
                                            {copied_label}
                                        } else {
                                            {copy_label}
                                        }
                                    }
                                }
                            }
                        } else {
                            p { class: "empty-text", "{invite_link_text}" }
                        }
                    }
                }

                // 推荐列表
                div { class: "card table-pagination-panel",
                    div { class: "card-header",
                        h3 { class: "card-title", {i18n.t("distribution.referral_users")} }
                    }
                    div { class: "table-container",
                        table { class: "table",
                            thead {
                                tr {
                                    th { {i18n.t("distribution.user")} }
                                    th { {i18n.t("distribution.joined_at")} }
                                    th { {i18n.t("distribution.total_spent")} }
                                    th { {i18n.t("distribution.my_earnings")} }
                                }
                            }
                            tbody {
                                match &referral_result {
                                    Some(Ok(list)) if !list.referrals.is_empty() => rsx! {
                                        for r in &list.referrals {
                                            tr {
                                                td {
                                                    div { class: "user-cell",
                                                        span { class: "user-name", {r.name.clone().unwrap_or_else(|| r.email.clone())} }
                                                        span { class: "user-email", "{r.email}" }
                                                    }
                                                }
                                                td { {format_time(&r.joined_at)} }
                                                td { {format_precise_cny_str(&r.total_spent)} }
                                                td { {format_precise_cny_str(&r.earnings_from_referral)} }
                                            }
                                        }
                                    },
                                    Some(Err(_)) => rsx! {
                                        tr {
                                            td { colspan: "4", class: "table-empty", {i18n.t("common.load_failed")} }
                                        }
                                    },
                                    None => rsx! {
                                        tr {
                                            td { colspan: "4", class: "table-empty", {i18n.t("table.loading")} }
                                        }
                                    },
                                    _ => rsx! {
                                        tr {
                                            td { colspan: "4", class: "table-empty", {i18n.t("distribution.no_referrals")} }
                                        }
                                    },
                                }
                            }
                        }
                    }
                }
                // 分页页脚与面板平级渲染（对齐定价页的页脚结构），避免页脚嵌入面板内部。
                Pagination {
                    current: page(),
                    total_pages: referral_total_pages,
                    total: referral_total,
                    page_size: page_size(),
                    summary: i18n.t_with_args(
                        "common.pagination_summary",
                        &[
                            ("total", &referral_total.to_string()),
                            ("current", &page().to_string()),
                            ("total_pages", &referral_total_pages.to_string()),
                        ],
                    ),
                    page_size_label: i18n.t("common.pagination_page_size").to_string(),
                    page_size_suffix: i18n.t("common.items_suffix").to_string(),
                    previous_label: i18n.t("table.previous").to_string(),
                    next_label: i18n.t("table.next").to_string(),
                    on_page_change: move |value| page.set(value),
                    on_page_size_change: move |value| {
                        page_size.set(value);
                        page.set(1);
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;

    #[test]
    fn quota_failure_is_never_displayed_as_money() {
        let error = ClientError::RateLimited("Rate limit exceeded".to_string().into());
        assert_eq!(total_earnings_display(Some(Err(error)), "Loading"), "—");
        assert_eq!(total_earnings_display(None, "Loading"), "Loading");
    }
}
