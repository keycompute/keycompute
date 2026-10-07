use super::scope::OperationsScope;
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::{
        resource::{KeyedResourceValue, current_keyed_value},
        time::format_time,
    },
};
use client_api::api::platform_operations::{PlatformOperationsApi, TenantHealthQuery};
use dioxus::prelude::*;
use ui::{Badge, BadgeVariant};
use uuid::Uuid;

const SIZE: u32 = 20;

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct Filter {
    pub search: String,
    pub status: String,
    pub offset: u32,
}

impl Filter {
    pub fn query(&self) -> Option<TenantHealthQuery> {
        if self.search.len() > 128
            || self.search.chars().any(char::is_control)
            || !matches!(self.status.as_str(), "" | "active" | "inactive")
            || self.offset > 1_000_000
        {
            return None;
        }
        Some(TenantHealthQuery {
            search: (!self.search.is_empty()).then(|| self.search.clone()),
            status: (!self.status.is_empty()).then(|| self.status.clone()),
            limit: Some(SIZE),
            offset: Some(self.offset),
        })
    }
}

pub(super) fn should_show_pager(item_count: usize, offset: u32) -> bool {
    item_count > 0 || offset > 0
}

#[component]
pub(super) fn HealthPanel(scope: OperationsScope) -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut search = use_signal(String::new);
    let mut status = use_signal(String::new);
    let mut filter = use_signal(Filter::default);
    let mut error = use_signal(String::new);
    let mut selected = use_signal(|| None::<Uuid>);
    let mut data = use_resource(move || {
        let filter = filter();
        let query = filter.query();
        let key = (scope, filter);
        async move {
            let result = match query {
                Some(query) if scope.health => {
                    scope
                        .read(auth, users, move |token| {
                            let query = query.clone();
                            async move {
                                PlatformOperationsApi::new(&get_client())
                                    .tenants(&query, &token)
                                    .await
                            }
                        })
                        .await
                }
                _ => Err(client_api::ClientError::Config(
                    "Invalid platform health query".into(),
                )),
            };
            KeyedResourceValue::new(key, result)
        }
    });
    let loaded = current_keyed_value(&(scope, filter()), data.state().cloned(), data());

    rsx! {
        section { class: "operations-health",
            div { class: "card filter-panel operations-filters",
                div { class: "filter-grid filter-grid-health",
                    div { class: "form-field filter-field-grow",
                        label { class: "form-label", r#for: "operations-search",
                            {i18n.t("operations.search")}
                        }
                        input {
                            id: "operations-search",
                            class: "input-field",
                            r#type: "search",
                            value: "{search}",
                            maxlength: "128",
                            placeholder: i18n.t("operations.search_placeholder"),
                            oninput: move |event| search.set(event.value()),
                        }
                    }
                    div { class: "form-field",
                        label { class: "form-label", r#for: "operations-status",
                            {i18n.t("operations.status")}
                        }
                        select {
                            id: "operations-status",
                            class: "input-field",
                            value: "{status}",
                            onchange: move |event| status.set(event.value()),
                            option { value: "", {i18n.t("operations.all_status")} }
                            option { value: "active", {i18n.t("common.enabled")} }
                            option { value: "inactive", {i18n.t("common.disabled")} }
                        }
                    }
                    div { class: "filter-actions",
                        button {
                            class: "btn btn-primary",
                            r#type: "button",
                            onclick: move |_| {
                                let next = Filter {
                                    search: search().trim().into(),
                                    status: status(),
                                    offset: 0,
                                };
                                if next.query().is_none() {
                                    error.set(i18n.t("operations.invalid_search").into());
                                    return;
                                }
                                error.set(String::new());
                                selected.set(None);
                                if filter() == next { data.restart(); } else { filter.set(next); }
                            },
                            {i18n.t("operations.apply")}
                        }
                        button {
                            class: "btn btn-secondary",
                            r#type: "button",
                            onclick: move |_| data.restart(),
                            {i18n.t("tenant.reload")}
                        }
                    }
                }
            }

            if !error().is_empty() {
                p { class: "alert alert-error", role: "alert", "{error}" }
            }

            match loaded {
                None => rsx! {
                    div { class: "content-loading", role: "status",
                        span { class: "spinner", aria_hidden: "true" }
                        span { {i18n.t("common.loading")} }
                    }
                },
                Some(Err(error)) => rsx! {
                    p { class: "alert alert-error", role: "alert", {user_error_message(i18n, &error)} }
                },
                Some(Ok(value)) => rsx! {
                    div { class: "data-meta-row",
                        span { class: "data-meta-label", {i18n.t("operations.as_of")} }
                        time { "{format_time(&value.as_of)}" }
                    }
                    if value.items.is_empty() {
                        div { class: "empty-state bordered-empty-state",
                            h3 { class: "empty-title", {i18n.t("tenant.empty")} }
                            p { class: "empty-description", {i18n.t("operations.empty_hint")} }
                        }
                    } else {
                        div { class: "table-pagination-panel table-pagination-frame operations-table",
                            div { class: "table-container", tabindex: "0",
                                table { class: "table",
                                    thead { tr {
                                        th { {i18n.t("operations.tenant")} }
                                        th { {i18n.t("operations.status")} }
                                        th { {i18n.t("operations.members")} }
                                        th { {i18n.t("operations.providers")} }
                                        th { {i18n.t("operations.nodes")} }
                                        th { {i18n.t("operations.tasks")} }
                                        th { {i18n.t("operations.actions")} }
                                    } }
                                    tbody {
                                        for row in &value.items {
                                            tr { key: "{row.tenant_id}",
                                                td {
                                                    strong { class: "table-primary", "{row.name}" }
                                                    code { class: "table-secondary", "{row.tenant_id}" }
                                                }
                                                td { HealthStatusBadge { status: row.status.clone() } }
                                                td { "{row.active_members} / {row.active_admins}" }
                                                td { "{row.enabled_accounts} / {row.provider_accounts}" }
                                                td { "{row.online_nodes} / {row.excluded_nodes}" }
                                                td { "{row.queued_tasks} / {row.leased_tasks}" }
                                                td {
                                                    button {
                                                        class: "btn btn-ghost btn-sm",
                                                        r#type: "button",
                                                        onclick: {
                                                            let id = row.tenant_id;
                                                            move |_| selected.set(Some(id))
                                                        },
                                                        {i18n.t("operations.detail")}
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if should_show_pager(value.items.len(), filter().offset) {
                        div { class: "table-pagination-footer",
                            button {
                                class: "btn btn-secondary btn-sm",
                                r#type: "button",
                                disabled: filter().offset == 0,
                                onclick: move |_| {
                                    filter.write().offset = filter().offset.saturating_sub(SIZE);
                                    selected.set(None);
                                },
                                {i18n.t("tenant.previous")}
                            }
                            span { class: "pagination-summary",
                                "{filter().offset / SIZE + 1} · {value.total} " {i18n.t("tenant.records")}
                            }
                            button {
                                class: "btn btn-secondary btn-sm",
                                r#type: "button",
                                disabled: i64::from(filter().offset) + i64::from(SIZE) >= value.total
                                    || filter().offset >= 1_000_000,
                                onclick: move |_| {
                                    filter.write().offset = filter().offset.saturating_add(SIZE);
                                    selected.set(None);
                                },
                                {i18n.t("tenant.next")}
                            }
                        }
                    }
                },
            }

            if let Some(id) = selected() {
                HealthDetail { scope, id, on_close: move |_| selected.set(None) }
            }
        }
    }
}

#[component]
fn HealthStatusBadge(status: String) -> Element {
    let i18n = use_i18n();
    let (variant, label) = match status.as_str() {
        "active" => (BadgeVariant::Success, i18n.t("common.enabled")),
        "inactive" => (BadgeVariant::Neutral, i18n.t("common.disabled")),
        _ => (BadgeVariant::Warning, status.as_str()),
    };
    rsx! { Badge { variant, "{label}" } }
}

#[component]
fn HealthDetail(scope: OperationsScope, id: Uuid, on_close: EventHandler<()>) -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let data = use_resource(move || async move {
        let result = scope
            .read(auth, users, move |token| async move {
                PlatformOperationsApi::new(&get_client())
                    .tenant(id, &token)
                    .await
            })
            .await;
        KeyedResourceValue::new((scope, id), result)
    });
    let loaded = current_keyed_value(&(scope, id), data.state().cloned(), data());

    rsx! {
        section { class: "card operations-detail", aria_label: i18n.t("operations.detail"),
            div { class: "card-header",
                div {
                    h2 { class: "card-title", {i18n.t("operations.detail")} }
                    code { class: "card-description", "{id}" }
                }
                button {
                    class: "btn btn-ghost btn-sm",
                    r#type: "button",
                    onclick: move |_| on_close.call(()),
                    {i18n.t("operations.close")}
                }
            }
            div { class: "card-body",
                match loaded {
                    None => rsx! { p { role: "status", {i18n.t("common.loading")} } },
                    Some(Err(error)) => rsx! {
                        p { role: "alert", class: "alert alert-error", {user_error_message(i18n, &error)} }
                    },
                    Some(Ok(row)) => rsx! {
                        div { class: "detail-grid",
                            div { dt { {i18n.t("operations.tenant")} } dd { "{row.name}" } }
                            div { dt { "Slug" } dd { "{row.slug}" } }
                            div { dt { {i18n.t("operations.status")} } dd { HealthStatusBadge { status: row.status } } }
                            div { dt { "RPM" } dd { "{row.default_rpm_limit}" } }
                            div { dt { "TPM" } dd { "{row.default_tpm_limit}" } }
                            div { dt { {i18n.t("operations.suspended")} } dd { "{row.suspended_members}" } }
                        }
                    },
                }
            }
        }
    }
}
