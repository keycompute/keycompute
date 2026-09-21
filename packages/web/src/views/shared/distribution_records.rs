use dioxus::prelude::*;
use ui::{Badge, BadgeVariant, PageHeader, Pagination, Table, TableHead};

const PAGE_SIZE: usize = 20;

use crate::hooks::use_i18n::use_i18n;
use crate::services::{
    api_client::{user_error_message, with_auto_refresh},
    distribution_service,
};
use crate::stores::auth_store::AuthStore;
use crate::stores::user_store::UserStore;
use crate::utils::display::{distribution_status_label, short_id};
use crate::utils::format_precise_cny_str;
use crate::utils::resource::{KeyedResourceValue, current_keyed_value};
use crate::utils::time::format_time;

/// 分销记录页面
///
/// - 普通用户：查看自己的推荐用户明细（真实表格数据）
/// - Admin：查看全平台分销记录，展示分销规则（只读，当前由后端硬编码）
#[component]
pub fn DistributionRecords() -> Element {
    let i18n = use_i18n();
    let user_store = use_context::<UserStore>();
    let auth_store = use_context::<AuthStore>();
    let can_manage_console = user_store
        .info
        .read()
        .as_ref()
        .map(|u| u.can_manage_console())
        .unwrap_or(false);

    let mut page = use_signal(|| 1u32);
    let mut page_size = use_signal(|| PAGE_SIZE as u32);

    // 收益数据（普通用户）
    let earnings = use_resource(move || async move {
        if can_manage_console {
            return Ok(None);
        }
        with_auto_refresh(auth_store, |token| async move {
            distribution_service::get_earnings(&token).await.map(Some)
        })
        .await
    });

    // 普通用户：推荐明细列表
    let referrals = use_resource(move || async move {
        if can_manage_console {
            return Ok(vec![]);
        }
        with_auto_refresh(auth_store, |token| async move {
            distribution_service::get_referrals(&token).await
        })
        .await
    });

    // Admin：全平台分销记录
    let admin_records = use_resource(move || async move {
        let request_key = (page(), page_size());
        if !can_manage_console {
            return KeyedResourceValue::new(
                request_key,
                Ok(client_api::api::distribution::DistributionRecordPage {
                    records: vec![],
                    total: 0,
                    page: 1,
                    page_size: PAGE_SIZE as i64,
                    total_pages: 0,
                }),
            );
        }
        let result = with_auto_refresh(auth_store, |token| {
            let params = client_api::api::distribution::DistributionQueryParams::new()
                .with_page(request_key.0 as i32)
                .with_page_size(request_key.1 as i32);
            async move { distribution_service::list_records_page(&params, &token).await }
        })
        .await;
        KeyedResourceValue::new(request_key, result)
    });

    // Admin：分销规则列表（只读展示，后端硬编码）
    let rules = use_resource(move || async move {
        if !can_manage_console {
            return Ok(vec![]);
        }
        with_auto_refresh(auth_store, |token| async move {
            use crate::services::api_client::get_client;
            use client_api::DistributionApi;
            let client = get_client();
            DistributionApi::new(&client)
                .list_distribution_rules(&token)
                .await
        })
        .await
    });

    let total_earnings = match earnings() {
        Some(Ok(Some(ref e))) => format_precise_cny_str(&e.total_earnings),
        _ => "¥0.00".to_string(),
    };
    let available = match earnings() {
        Some(Ok(Some(ref e))) => format_precise_cny_str(&e.available_earnings),
        _ => "¥0.00".to_string(),
    };
    let pending = match earnings() {
        Some(Ok(Some(ref e))) => format_precise_cny_str(&e.pending_earnings),
        _ => "¥0.00".to_string(),
    };

    let page_desc = if can_manage_console {
        i18n.t("distribution_records.admin_desc")
    } else {
        i18n.t("distribution_records.user_desc")
    };

    rsx! {
        div { class: "page-container distribution-records-page",
            PageHeader {
                title: i18n.t("page.distribution_records").to_string(),
                description: page_desc.to_string(),
            }

            // 收益统计只属于个人视图；管理员页不混入当前管理员的个人收益。
            if !can_manage_console {
                div { class: "stats-grid",
                    div { class: "stat-card card",
                        div { class: "card-body",
                            p { class: "stat-label", {i18n.t("distribution.total_earnings")} }
                            p { class: "stat-value", "{total_earnings}" }
                        }
                    }
                    div { class: "stat-card card",
                        div { class: "card-body",
                            p { class: "stat-label", {i18n.t("distribution.available_balance")} }
                            p { class: "stat-value", "{available}" }
                        }
                    }
                    div { class: "stat-card card",
                        div { class: "card-body",
                            p { class: "stat-label", {i18n.t("distribution.pending")} }
                            p { class: "stat-value", "{pending}" }
                        }
                    }
                }
            }

            // 分销规则只读展示（Admin 可见）
            if can_manage_console {
                div { class: "section",
                    h2 { class: "section-title", {i18n.t("distribution_records.rules_title")} }
                    div {
                        class: "alert alert-info",
                        style: "margin-bottom: 12px",
                        span { class: "alert-icon", "ℹ" }
                        div { class: "alert-content",
                            p { class: "alert-body", {i18n.t("distribution_records.rules_hint")} }
                        }
                    }
                    match rules() {
                        None => rsx! {
                            p { class: "text-secondary", {i18n.t("table.loading")} }
                        },
                        Some(Err(ref e)) => rsx! {
                            p { class: "text-secondary", {user_error_message(e)} }
                        },
                        Some(Ok(ref list)) if list.is_empty() => rsx! {
                            p { class: "text-secondary", {i18n.t("distribution_records.no_rules")} }
                        },
                        Some(Ok(ref list)) => rsx! {
                            Table { col_count: 4,
                                thead {
                                    tr {
                                        TableHead { {i18n.t("distribution_records.rule_name")} }
                                        TableHead { {i18n.t("distribution_records.commission_rate")} }
                                        TableHead { {i18n.t("table.status")} }
                                        TableHead { {i18n.t("table.created_at")} }
                                    }
                                }
                                tbody {
                                    for r in list.iter() {
                                        tr {
                                            td { "{r.name}" }
                                            td { {format!("{:.1}%", r.commission_rate * 100.0)} }
                                            td {
                                                if r.is_active {
                                                    Badge { variant: BadgeVariant::Success, {i18n.t("common.enabled")} }
                                                } else {
                                                    Badge { variant: BadgeVariant::Neutral, {i18n.t("common.disabled")} }
                                                }
                                            }
                                            td { {format_time(&r.created_at)} }
                                        }
                                    }
                                }
                            }
                        },
                    }
                }
            }

            // 表格：admin 视图 / 普通用户视图分别渲染
            div { class: "table-pagination-panel table-pagination-frame",
                if can_manage_console {
                    {
                        let request_key = (page(), page_size());
                        let current_admin_records = current_keyed_value(
                            &request_key,
                            admin_records.state().cloned(),
                            admin_records(),
                        );
                        let (is_empty, empty_text) = match &current_admin_records {
                            None => (true, i18n.t("table.loading").to_string()),
                            Some(Err(e)) => (true, user_error_message(e)),
                            Some(Ok(result)) if result.records.is_empty() => {
                                (true, i18n.t("distribution_records.empty_admin").to_string())
                            }
                            _ => (false, String::new()),
                        };
                        rsx! {
                            Table { empty: is_empty, empty_text, col_count: 7u32,
                                thead {
                                    tr {
                                        TableHead { {i18n.t("distribution_records.record_id")} }
                                        TableHead { {i18n.t("distribution_records.source_user_id")} }
                                        TableHead { {i18n.t("distribution_records.amount_spent")} }
                                        TableHead { {i18n.t("distribution_records.commission_amount")} }
                                        TableHead { {i18n.t("table.status")} }
                                        TableHead { {i18n.t("table.created_at")} }
                                        TableHead { {i18n.t("distribution_records.referrer_id")} }
                                    }
                                }
                                tbody {
                                    if let Some(Ok(ref result)) = current_admin_records {
                                        for rec in result.records.iter() {
                                            tr {
                                                td {
                                                    code { title: "{rec.id}", {short_id(&rec.id)} } // 截取 UUID 前 8 位＋全量 tooltip
                                                }
                                                td {
                                                    // 截取 UUID 前 8 位＋全量 tooltip
                                                    span {
                                                        title: "{rec.referred_id}",
                                                        style: "cursor: help; font-family: monospace; font-size: 13px;",
                                                        {short_id(&rec.referred_id)}
                                                    }
                                                }
                                                td { {format_precise_cny_str(&rec.amount)} }
                                                td { {format_precise_cny_str(&rec.commission)} }
                                                td {
                                                    Badge { variant: dist_status_variant(&rec.status),
                                                        {distribution_status_label(&rec.status, &i18n)}
                                                    }
                                                }
                                                td { {format_time(&rec.created_at)} }
                                                td {
                                                    span {
                                                        title: "{rec.referrer_id}",
                                                        style: "cursor: help; font-family: monospace; font-size: 13px;",
                                                        {short_id(&rec.referrer_id)}
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    {
                        let (is_empty, empty_text) = match referrals() {
                            None => (true, i18n.t("table.loading").to_string()),
                            Some(Err(ref e)) => (true, user_error_message(e)),
                            Some(Ok(ref l)) if l.is_empty() => {
                                (true, i18n.t("distribution_records.empty_user").to_string())
                            }
                            _ => (false, String::new()),
                        };
                        let ref_start = (page() as usize - 1) * page_size() as usize;
                        rsx! {
                            Table { empty: is_empty, empty_text, col_count: 4u32,
                                thead {
                                    tr {
                                        TableHead { {i18n.t("distribution_records.referred_user")} }
                                        TableHead { {i18n.t("distribution.joined_at")} }
                                        TableHead { {i18n.t("distribution.total_spent")} }
                                        TableHead { {i18n.t("distribution.my_earnings")} }
                                    }
                                }
                                tbody {
                                    if let Some(Ok(ref list)) = referrals() {
                                        for r in list.iter().skip(ref_start).take(page_size() as usize) {
                                            tr {
                                                td {
                                                    div { class: "user-cell",
                                                        span { class: "user-name",
                                                            {r.name.clone().unwrap_or_else(|| r.email.clone())}
                                                        }
                                                        span { class: "user-email text-secondary", "{r.email}" }
                                                    }
                                                }
                                                td { {format_time(&r.joined_at)} }
                                                td { {format_precise_cny_str(&r.total_spent)} }
                                                td { {format_precise_cny_str(&r.earnings_from_referral)} }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

            }

            // 分页页脚与面板平级渲染（对齐定价页的页脚结构），避免页脚嵌入面板内部。
            {
                let (total, total_pages) = if can_manage_console {
                    let request_key = (page(), page_size());
                    current_keyed_value(
                            &request_key,
                            admin_records.state().cloned(),
                            admin_records(),
                        )
                        .and_then(|r| r.ok())
                        .map(|result| (
                            result.total.max(0) as usize,
                            result.total_pages.max(1) as u32,
                        ))
                        .unwrap_or((0, 1))
                } else {
                    let total = referrals().and_then(|r| r.ok()).map(|l| l.len()).unwrap_or(0);
                    (total, total.div_ceil(page_size() as usize).max(1) as u32)
                };
                rsx! {
                    Pagination {
                        current: page(),
                        total_pages,
                        total: total as u64,
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

fn dist_status_variant(status: &str) -> BadgeVariant {
    match status {
        "settled" | "paid" => BadgeVariant::Success,
        "pending" => BadgeVariant::Warning,
        "cancelled" | "failed" => BadgeVariant::Error,
        _ => BadgeVariant::Neutral,
    }
}
