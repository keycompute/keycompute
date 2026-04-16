use dioxus::prelude::*;

use crate::hooks::use_i18n::use_i18n;
use crate::services::{api_client::with_auto_refresh, distribution_service};
use crate::stores::auth_store::AuthStore;
use crate::utils::time::format_time;

#[component]
pub fn DistributionOverview() -> Element {
    let i18n = use_i18n();
    let auth_store = use_context::<AuthStore>();

    // 收益数据
    let earnings = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            distribution_service::get_earnings(&token).await
        })
        .await
    });

    // 推荐码
    let referral_code = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            distribution_service::get_referral_code(&token).await
        })
        .await
    });

    // 推荐列表
    let referrals = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            distribution_service::get_referrals(&token).await
        })
        .await
    });

    let total_earnings = match earnings() {
        Some(Ok(ref e)) => format!("¥{}", e.total_earnings),
        Some(Err(_)) => i18n.t("common.load_failed").to_string(),
        None => i18n.t("table.loading").to_string(),
    };
    let available_earnings = match earnings() {
        Some(Ok(ref e)) => format!("¥{}", e.available_earnings),
        _ => "—".to_string(),
    };
    let pending_earnings = match earnings() {
        Some(Ok(ref e)) => format!("¥{}", e.pending_earnings),
        _ => "—".to_string(),
    };
    let referral_count = match earnings() {
        Some(Ok(ref e)) => e.referral_count.to_string(),
        _ => "—".to_string(),
    };
    let code_text = match referral_code() {
        Some(Ok(ref r)) => r.referral_code.clone(),
        Some(Err(_)) => i18n.t("distribution.fetch_failed").to_string(),
        None => i18n.t("table.loading").to_string(),
    };
    let invite_link = match referral_code() {
        Some(Ok(ref r)) => r.referral_link.clone(),
        _ => String::new(),
    };

    rsx! {
        div {
            class: "page-container",
            div {
                class: "page-header",
                h1 { class: "page-title", {i18n.t("distribution.title")} }
                p { class: "page-subtitle", {i18n.t("distribution.subtitle")} }
            }

            // 收益统计
            div {
                class: "stats-grid",
                div { class: "stat-card card",
                    div { class: "card-body",
                        p { class: "stat-label", {i18n.t("distribution.total_earnings")} }
                        p { class: "stat-value", "{total_earnings}" }
                    }
                }
                div { class: "stat-card card",
                    div { class: "card-body",
                        p { class: "stat-label", {i18n.t("distribution.available_balance")} }
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

            // 推荐码
            div { class: "card",
                div { class: "card-header",
                    h3 { class: "card-title", {i18n.t("distribution.my_referral_code")} }
                }
                div { class: "card-body",
                    div { class: "info-grid",
                        div { class: "info-item",
                            span { class: "info-label", {i18n.t("distribution.referral_code")} }
                            span { class: "info-value",
                                code { "{code_text}" }
                            }
                        }
                        if !invite_link.is_empty() {
                            div { class: "info-item",
                                span { class: "info-label", {i18n.t("distribution.invite_link")} }
                                span { class: "info-value",
                                    a { href: "{invite_link}", target: "_blank", "{invite_link}" }
                                }
                            }
                        }
                    }
                }
            }

            // 推荐列表
            div { class: "card",
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
                            match referrals() {
                                Some(Ok(ref list)) if !list.is_empty() => rsx! {
                                    for r in list.iter() {
                                        tr {
                                            td {
                                                div { class: "user-cell",
                                                    span { class: "user-name",
                                                        { r.name.clone().unwrap_or_else(|| r.email.clone()) }
                                                    }
                                                    span { class: "user-email", "{r.email}" }
                                                }
                                            }
                                            td { { format_time(&r.joined_at) } }
                                            td { "¥{r.total_spent}" }
                                            td { "¥{r.earnings_from_referral}" }
                                        }
                                    }
                                },
                                Some(Err(_)) => rsx! {
                                    tr { td { colspan: "4", class: "table-empty", {i18n.t("common.load_failed")} } }
                                },
                                _ => rsx! {
                                    tr { td { colspan: "4", class: "table-empty", {i18n.t("distribution.no_referrals")} } }
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}
