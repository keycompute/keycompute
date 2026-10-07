use client_api::api::{
    distribution_policy::{BeneficiaryScope, DistributionPolicyApi},
    distribution_reporting::DistributionReportingApi,
};
use dioxus::prelude::*;
use ui::{Badge, BadgeVariant, PageHeader, Pagination, Table, TableHead};
use uuid::Uuid;

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::{
    api_client::{get_client, user_error_message, with_auto_refresh},
    distribution_service, tenant_service,
};
use crate::stores::auth_store::AuthStore;
use crate::stores::user_store::UserStore;
use crate::utils::display::{distribution_status_label, short_id};
use crate::utils::resource::{KeyedResourceValue, current_keyed_value};
use crate::utils::time::format_time;
use crate::utils::{format_precise_cny_str, format_precise_money_str};

const PAGE_SIZE: usize = 20;

/// Distribution reporting is personal for regular users and explicitly
/// tenant-targeted for platform administrators. A platform role never relies
/// on a hidden "current tenant" selection.
#[component]
pub fn DistributionRecords() -> Element {
    let i18n = use_i18n();
    let user_store = use_context::<UserStore>();
    let auth_store = use_context::<AuthStore>();
    let can_manage_platform = user_store
        .info
        .read()
        .as_ref()
        .is_some_and(|user| user.can_manage_platform());

    let mut page = use_signal(|| 1u32);
    let mut page_size = use_signal(|| PAGE_SIZE as u32);
    let mut rule_page = use_signal(|| 1u32);
    let mut rule_page_size = use_signal(|| PAGE_SIZE as u32);
    let mut selected_tenant = use_signal(String::new);

    let earnings = use_resource(move || async move {
        if can_manage_platform {
            return Ok(None);
        }
        with_auto_refresh(auth_store, |token| async move {
            distribution_service::get_earnings(&token).await.map(Some)
        })
        .await
    });

    let referrals = use_resource(move || async move {
        if can_manage_platform {
            return Ok(vec![]);
        }
        with_auto_refresh(auth_store, |token| async move {
            distribution_service::get_referrals(&token).await
        })
        .await
    });

    let tenants = use_resource(move || async move {
        if !can_manage_platform {
            return Ok(vec![]);
        }
        with_auto_refresh(auth_store, |token| async move {
            tenant_service::list_active(&token).await
        })
        .await
    });

    let admin_records = use_resource(move || {
        let request_key = (selected_tenant(), page(), page_size());
        async move {
            let tenant_id = Uuid::parse_str(request_key.0.trim())
                .ok()
                .filter(|id| !id.is_nil());
            let result = if can_manage_platform {
                if let Some(tenant_id) = tenant_id {
                    with_auto_refresh(auth_store, |token| async move {
                        DistributionReportingApi::platform_tenant(&get_client(), tenant_id)?
                            .records(request_key.1, request_key.2, &token)
                            .await
                            .map(Some)
                    })
                    .await
                } else {
                    Ok(None)
                }
            } else {
                Ok(None)
            };
            KeyedResourceValue::new(request_key, result)
        }
    });

    let rules = use_resource(move || {
        let request_key = (selected_tenant(), rule_page(), rule_page_size());
        async move {
            let tenant_id = Uuid::parse_str(request_key.0.trim())
                .ok()
                .filter(|id| !id.is_nil());
            let result = if can_manage_platform {
                if let Some(tenant_id) = tenant_id {
                    with_auto_refresh(auth_store, |token| async move {
                        DistributionPolicyApi::platform_tenant(&get_client(), tenant_id)?
                            .list(request_key.1, request_key.2, &token)
                            .await
                            .map(Some)
                    })
                    .await
                } else {
                    Ok(None)
                }
            } else {
                Ok(None)
            };
            KeyedResourceValue::new(request_key, result)
        }
    });

    let total_earnings = match earnings() {
        Some(Ok(Some(ref value))) => format_precise_cny_str(&value.total_earnings),
        _ => "¥0.00".to_string(),
    };
    let available = match earnings() {
        Some(Ok(Some(ref value))) => format_precise_cny_str(&value.available_earnings),
        _ => "¥0.00".to_string(),
    };
    let pending = match earnings() {
        Some(Ok(Some(ref value))) => format_precise_cny_str(&value.pending_earnings),
        _ => "¥0.00".to_string(),
    };
    let selected_id = Uuid::parse_str(selected_tenant().trim())
        .ok()
        .filter(|id| !id.is_nil());
    let page_desc = if can_manage_platform {
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

            if can_manage_platform {
                section { class: "card platform-target-card",
                    div { class: "card-header",
                        div {
                            h2 { class: "card-title", {i18n.t("distribution_records.target_title")} }
                            p { class: "text-secondary card-description", {i18n.t("distribution_records.target_hint")} }
                        }
                    }
                    div { class: "card-body platform-target-body",
                        match tenants() {
                            None => rsx! {
                                div { class: "inline-loading", role: "status",
                                    span { class: "spinner", aria_hidden: "true" }
                                    span { {i18n.t("common.loading")} }
                                }
                            },
                            Some(Err(ref error)) => rsx! {
                                div { class: "alert alert-error", role: "alert", {user_error_message(i18n, error)} }
                            },
                            Some(Ok(ref items)) if items.is_empty() => rsx! {
                                div { class: "empty-state compact-empty-state",
                                    h3 { class: "empty-title", {i18n.t("distribution_records.no_tenants_title")} }
                                    p { class: "empty-description", {i18n.t("distribution_records.no_tenants_hint")} }
                                    Link { class: "btn btn-primary", to: Route::Tenants {}, {i18n.t("page.tenants")} }
                                }
                            },
                            Some(Ok(ref items)) => rsx! {
                                div { class: "form-field platform-target-field",
                                    label { class: "form-label", r#for: "distribution-tenant", {i18n.t("distribution_records.target_label")} }
                                    select {
                                        id: "distribution-tenant",
                                        class: "input-field",
                                        value: "{selected_tenant}",
                                        onchange: move |event| {
                                            selected_tenant.set(event.value());
                                            page.set(1);
                                            rule_page.set(1);
                                        },
                                        option { value: "", {i18n.t("distribution_records.target_placeholder")} }
                                        for tenant in items.iter() {
                                            option { value: "{tenant.id}", "{tenant.name} · {tenant.slug}" }
                                        }
                                    }
                                    p { class: "form-hint", {i18n.t("distribution_records.target_safety_hint")} }
                                }
                            },
                        }
                    }
                }

                if let Some(tenant_id) = selected_id {
                    div { class: "scope-banner", role: "status",
                        span { class: "scope-banner-label", {i18n.t("distribution_records.current_target")} }
                        code { "{tenant_id}" }
                    }

                    section { class: "section distribution-rules-section",
                        h2 { class: "section-title", {i18n.t("distribution_records.rules_title")} }
                        div { class: "section-body",
                            div { class: "alert alert-info",
                                span { class: "alert-icon", "ℹ" }
                                div { class: "alert-content",
                                    p { class: "alert-body", {i18n.t("distribution_records.rules_hint")} }
                                }
                            }
                            {
                                let request_key = (
                                    selected_tenant(),
                                    rule_page(),
                                    rule_page_size(),
                                );
                                let current_rules = current_keyed_value(
                                    &request_key,
                                    rules.state().cloned(),
                                    rules(),
                                );
                                let total = current_rules
                                    .as_ref()
                                    .and_then(|result| result.as_ref().ok())
                                    .and_then(|result| result.as_ref())
                                    .map(|result| result.total.max(0) as u64)
                                    .unwrap_or(0);
                                let total_pages = current_rules
                                    .as_ref()
                                    .and_then(|result| result.as_ref().ok())
                                    .and_then(|result| result.as_ref())
                                    .map(|result| result.total_pages.max(1) as u32)
                                    .unwrap_or(1);
                                rsx! {
                                    div { class: "table-pagination-panel table-pagination-frame",
                                        match current_rules {
                                            None => rsx! {
                                                div { class: "content-loading", role: "status",
                                                    span { class: "spinner", aria_hidden: "true" }
                                                    span { {i18n.t("common.loading")} }
                                                }
                                            },
                                            Some(Err(error)) => rsx! {
                                                div { class: "alert alert-error", role: "alert", {user_error_message(i18n, &error)} }
                                            },
                                            Some(Ok(None)) => rsx! {},
                                            Some(Ok(Some(result))) => rsx! {
                                                div { class: "table-container",
                                                    Table {
                                                        empty: result.rules.is_empty(),
                                                        empty_text: i18n.t("distribution_records.no_rules"),
                                                        col_count: 4,
                                                        thead { tr {
                                                            TableHead { {i18n.t("distribution_records.rule_name")} }
                                                            TableHead { {i18n.t("distribution_records.commission_rate")} }
                                                            TableHead { {i18n.t("table.status")} }
                                                            TableHead { {i18n.t("table.created_at")} }
                                                        } }
                                                        tbody {
                                                            for rule in result.rules.iter() {
                                                                tr { key: "{rule.id}",
                                                                    td {
                                                                        span { class: "table-primary", "{rule.name}" }
                                                                        span { class: "table-secondary",
                                                                            {beneficiary_scope_label(&rule.beneficiary_scope, &i18n)}
                                                                        }
                                                                        if let Some(beneficiary_id) = rule.beneficiary_id {
                                                                            code {
                                                                                class: "table-secondary",
                                                                                title: "{beneficiary_id}",
                                                                                {short_id(&beneficiary_id.to_string())}
                                                                            }
                                                                        }
                                                                    }
                                                                    td { {format_rate(&rule.commission_rate)} }
                                                                    td {
                                                                        if rule.is_active {
                                                                            Badge { variant: BadgeVariant::Success, {i18n.t("common.enabled")} }
                                                                        } else {
                                                                            Badge { variant: BadgeVariant::Neutral, {i18n.t("common.disabled")} }
                                                                        }
                                                                    }
                                                                    td { {format_time(&rule.created_at)} }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            },
                                        }
                                    }
                                    Pagination {
                                        current: rule_page(),
                                        total_pages,
                                        total,
                                        page_size: rule_page_size(),
                                        summary: pagination_summary(
                                            &i18n,
                                            total,
                                            rule_page(),
                                            total_pages,
                                        ),
                                        page_size_label: i18n.t("common.pagination_page_size").to_string(),
                                        page_size_suffix: i18n.t("common.items_suffix").to_string(),
                                        previous_label: i18n.t("table.previous").to_string(),
                                        next_label: i18n.t("table.next").to_string(),
                                        on_page_change: move |value| rule_page.set(value),
                                        on_page_size_change: move |size| {
                                            rule_page_size.set(size);
                                            rule_page.set(1);
                                        },
                                    }
                                }
                            }
                        }
                    }

                    {
                        let request_key = (selected_tenant(), page(), page_size());
                        let current_records = current_keyed_value(
                            &request_key,
                            admin_records.state().cloned(),
                            admin_records(),
                        );
                        let total = current_records
                            .as_ref()
                            .and_then(|result| result.as_ref().ok())
                            .and_then(|result| result.as_ref())
                            .map(|result| result.total.max(0) as u64)
                            .unwrap_or(0);
                        let total_pages = current_records
                            .as_ref()
                            .and_then(|result| result.as_ref().ok())
                            .and_then(|result| result.as_ref())
                            .map(|result| result.total_pages.max(1) as u32)
                            .unwrap_or(1);
                        rsx! {
                            div { class: "table-pagination-panel table-pagination-frame",
                                match current_records {
                                    None => rsx! {
                                        div { class: "content-loading", role: "status",
                                            span { class: "spinner", aria_hidden: "true" }
                                            span { {i18n.t("common.loading")} }
                                        }
                                    },
                                    Some(Err(error)) => rsx! {
                                        div { class: "alert alert-error", role: "alert", {user_error_message(i18n, &error)} }
                                    },
                                    Some(Ok(None)) => rsx! {},
                                    Some(Ok(Some(result))) => rsx! {
                                        Table {
                                            empty: result.records.is_empty(),
                                            empty_text: i18n.t("distribution_records.empty_admin"),
                                            col_count: 7,
                                            thead { tr {
                                                TableHead { {i18n.t("distribution_records.record_id")} }
                                                TableHead { {i18n.t("distribution_records.source_user_id")} }
                                                TableHead { {i18n.t("distribution_records.amount_spent")} }
                                                TableHead { {i18n.t("distribution_records.commission_amount")} }
                                                TableHead { {i18n.t("table.status")} }
                                                TableHead { {i18n.t("table.created_at")} }
                                                TableHead { {i18n.t("distribution_records.beneficiary_id")} }
                                            } }
                                            tbody {
                                                for record in result.records.iter() {
                                                    tr { key: "{record.id}",
                                                        td { code { title: "{record.id}", {short_id(&record.id.to_string())} } }
                                                        td { code { title: "{record.referred_id}", {short_id(&record.referred_id.to_string())} } }
                                                        td {
                                                            span { class: "table-primary", {format_record_amount(&record.amount, &record.currency)} }
                                                        }
                                                        td {
                                                            span { class: "table-primary", {format_record_amount(&record.commission, &record.currency)} }
                                                        }
                                                        td { Badge {
                                                            variant: dist_status_variant(&record.status),
                                                            {distribution_status_label(&record.status, &i18n)}
                                                        } }
                                                        td { {format_time(&record.created_at)} }
                                                        td {
                                                            if let Some(beneficiary) = record.beneficiary_id {
                                                                code { title: "{beneficiary}", {short_id(&beneficiary.to_string())} }
                                                            } else {
                                                                span { class: "text-secondary", "—" }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    },
                                }
                            }
                            Pagination {
                                current: page(),
                                total_pages,
                                total,
                                page_size: page_size(),
                                summary: pagination_summary(&i18n, total, page(), total_pages),
                                page_size_label: i18n.t("common.pagination_page_size").to_string(),
                                page_size_suffix: i18n.t("common.items_suffix").to_string(),
                                previous_label: i18n.t("table.previous").to_string(),
                                next_label: i18n.t("table.next").to_string(),
                                on_page_change: move |value| page.set(value),
                                on_page_size_change: move |size| {
                                    page_size.set(size);
                                    page.set(1);
                                },
                            }
                        }
                    }
                } else if tenants().as_ref().is_some_and(|result| result.as_ref().is_ok_and(|items| !items.is_empty())) {
                    div { class: "empty-state platform-target-empty",
                        h3 { class: "empty-title", {i18n.t("distribution_records.select_target_title")} }
                        p { class: "empty-description", {i18n.t("distribution_records.select_target_hint")} }
                    }
                }
            } else {
                div { class: "stats-grid",
                    DistributionStat { label: i18n.t("distribution.total_earnings"), value: total_earnings }
                    DistributionStat { label: i18n.t("distribution.available_balance"), value: available }
                    DistributionStat { label: i18n.t("distribution.pending"), value: pending }
                }

                {
                    let (is_empty, empty_text) = match referrals() {
                        None => (true, i18n.t("table.loading").to_string()),
                        Some(Err(ref error)) => (true, user_error_message(i18n, error)),
                        Some(Ok(ref items)) if items.is_empty() => {
                            (true, i18n.t("distribution_records.empty_user").to_string())
                        }
                        _ => (false, String::new()),
                    };
                    let start = (page() as usize - 1) * page_size() as usize;
                    let total = referrals()
                        .and_then(|result| result.ok())
                        .map(|items| items.len())
                        .unwrap_or(0);
                    let total_pages = total.div_ceil(page_size() as usize).max(1) as u32;
                    rsx! {
                        div { class: "table-pagination-panel table-pagination-frame",
                            Table { empty: is_empty, empty_text, col_count: 4,
                                thead { tr {
                                    TableHead { {i18n.t("distribution_records.referred_user")} }
                                    TableHead { {i18n.t("distribution.joined_at")} }
                                    TableHead { {i18n.t("distribution.total_spent")} }
                                    TableHead { {i18n.t("distribution.my_earnings")} }
                                } }
                                tbody {
                                    if let Some(Ok(ref items)) = referrals() {
                                        for referral in items.iter().skip(start).take(page_size() as usize) {
                                            tr {
                                                td { div { class: "user-cell",
                                                    span { class: "user-name", {referral.name.clone().unwrap_or_else(|| referral.email.clone())} }
                                                    span { class: "user-email text-secondary", "{referral.email}" }
                                                } }
                                                td { {format_time(&referral.joined_at)} }
                                                td { {format_precise_cny_str(&referral.total_spent)} }
                                                td { {format_precise_cny_str(&referral.earnings_from_referral)} }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Pagination {
                            current: page(),
                            total_pages,
                            total: total as u64,
                            page_size: page_size(),
                            summary: pagination_summary(&i18n, total as u64, page(), total_pages),
                            page_size_label: i18n.t("common.pagination_page_size").to_string(),
                            page_size_suffix: i18n.t("common.items_suffix").to_string(),
                            previous_label: i18n.t("table.previous").to_string(),
                            next_label: i18n.t("table.next").to_string(),
                            on_page_change: move |value| page.set(value),
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

#[component]
fn DistributionStat(label: &'static str, value: String) -> Element {
    rsx! {
        div { class: "stat-card card",
            div { class: "card-body",
                p { class: "stat-label", "{label}" }
                p { class: "stat-value", "{value}" }
            }
        }
    }
}

fn pagination_summary(i18n: &crate::i18n::I18n, total: u64, page: u32, total_pages: u32) -> String {
    i18n.t_with_args(
        "common.pagination_summary",
        &[
            ("total", &total.to_string()),
            ("current", &page.to_string()),
            ("total_pages", &total_pages.to_string()),
        ],
    )
}

fn format_rate(rate: &str) -> String {
    rate.parse::<f64>()
        .map(|value| format!("{:.2}%", value * 100.0))
        .unwrap_or_else(|_| rate.to_string())
}

fn format_record_amount(amount: &str, currency: &str) -> String {
    let currency = currency.trim().to_ascii_uppercase();
    let amount = format_precise_money_str(amount);
    if currency.is_empty() {
        amount
    } else {
        format!("{currency} {amount}")
    }
}

fn beneficiary_scope_label(scope: &BeneficiaryScope, i18n: &crate::i18n::I18n) -> &'static str {
    match scope {
        BeneficiaryScope::Everyone => i18n.t("tenant_distribution.everyone"),
        BeneficiaryScope::TenantMember => i18n.t("tenant_distribution.member"),
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

#[cfg(test)]
mod tests {
    use super::{beneficiary_scope_label, format_record_amount};
    use crate::i18n::{I18n, Lang};
    use client_api::api::distribution_policy::BeneficiaryScope;

    #[test]
    fn platform_record_amounts_keep_the_response_currency() {
        assert_eq!(format_record_amount("12.3400000000", "usd"), "USD 12.34");
        assert_eq!(format_record_amount("0.0090000000", "CNY"), "CNY 0.009");
        assert_eq!(format_record_amount("7", "  "), "7.00");
    }

    #[test]
    fn beneficiary_scopes_use_localized_labels() {
        let zh = I18n::new(Lang::Zh);
        let en = I18n::new(Lang::En);
        assert_eq!(
            beneficiary_scope_label(&BeneficiaryScope::Everyone, &zh),
            "全部成员"
        );
        assert_eq!(
            beneficiary_scope_label(&BeneficiaryScope::TenantMember, &en),
            "Tenant member"
        );
    }
}
