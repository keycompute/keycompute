//! Tenant-admin financial controls. No platform balance mutation or payout-secret access.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

use super::common::{
    self, MemberIdField, Pager, TechnicalId, WorkspaceContext, WorkspaceLinks, WorkspaceScope,
};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::time::format_time,
};
use chrono::{DateTime, Utc};
use client_api::api::{
    admin::{BalanceReservationInfo, UserBalanceReservationsResponse},
    tenant_financial_control::{
        TenantFinancialControlApi, TenantWithdrawal, TenantWithdrawalPage, WithdrawalDecision,
        WithdrawalQuery,
    },
};
use dioxus::prelude::*;
use uuid::Uuid;

const PAGE: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Reservations,
    Withdrawals,
}

fn owner(raw: &str) -> client_api::Result<Uuid> {
    Uuid::parse_str(raw.trim())
        .ok()
        .filter(|value| !value.is_nil())
        .ok_or_else(|| client_api::ClientError::Config("Select a valid member ID".into()))
}

fn expired(raw: &str) -> bool {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc) < Utc::now())
        .unwrap_or(false)
}

fn pages(total: i64) -> i64 {
    if total <= 0 {
        1
    } else {
        // The API accepts offsets through 1,000,000. Avoid arithmetic overflow
        // in untrusted totals and never offer a page beyond that bound.
        (total / i64::from(PAGE) + i64::from(total % i64::from(PAGE) != 0))
            .min(1_000_000 / i64::from(PAGE) + 1)
    }
}

fn withdrawal_status_label(i18n: crate::i18n::I18n, status: &str) -> String {
    let key = match status {
        "pending" => Some("tenant_financial_controls.pending"),
        "approved" => Some("tenant_financial_controls.approved"),
        "completed" => Some("tenant_financial_controls.completed"),
        "rejected" => Some("tenant_financial_controls.rejected"),
        _ => None,
    };
    key.map(|key| i18n.t(key).to_owned())
        .unwrap_or_else(|| status.to_owned())
}

fn withdrawal_type_label(i18n: crate::i18n::I18n, withdrawal_type: &str) -> String {
    let key = match withdrawal_type {
        "balance" => Some("tenant_financial_controls.type_balance"),
        "alipay" => Some("tenant_financial_controls.type_alipay"),
        _ => None,
    };
    key.map(|key| i18n.t(key).to_owned())
        .unwrap_or_else(|| withdrawal_type.to_owned())
}

#[component]
pub fn TenantFinancialControls() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let allowed = users
        .info
        .read()
        .as_ref()
        .is_some_and(|value| value.can_manage_billing());

    rsx! {
        if let Some(scope) = WorkspaceScope::from_stores(auth, users).filter(|_| allowed) {
            for key in [format!("{scope:?}")] {
                Workspace { key: "{key}", scope }
            }
        } else {
            p { role: "alert", {i18n.t("tenant.admin_required")} }
        }
    }
}

#[component]
fn Workspace(scope: WorkspaceScope) -> Element {
    let i18n = use_i18n();
    let mut tab = use_signal(|| Tab::Reservations);

    rsx! {
        div { class: "page-container tenant-financial-controls",
            ui::PageHeader {
                title: i18n.t("tenant_financial_controls.title").to_string(),
                description: i18n.t("tenant_financial_controls.hint").to_string(),
            }
            WorkspaceLinks {}
            WorkspaceContext { note: i18n.t("tenant_financial_controls.boundary").to_string() }
            nav { class: "segmented-control",
                button {
                    class: "btn btn-secondary",
                    aria_pressed: tab() == Tab::Reservations,
                    onclick: move |_| tab.set(Tab::Reservations),
                    {i18n.t("tenant_financial_controls.reservations")}
                }
                button {
                    class: "btn btn-secondary",
                    aria_pressed: tab() == Tab::Withdrawals,
                    onclick: move |_| tab.set(Tab::Withdrawals),
                    {i18n.t("tenant_financial_controls.withdrawals")}
                }
            }
            if tab() == Tab::Reservations {
                ReservationPanel { scope }
            } else {
                WithdrawalPanel { scope }
            }
        }
    }
}

#[component]
fn ReservationPanel(scope: WorkspaceScope) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut draft = use_signal(String::new);
    let mut selected = use_signal(|| None::<Uuid>);
    let mut cursor = use_signal(|| None::<String>);
    let mut history = use_signal(Vec::<Option<String>>::new);
    let mut generation = use_signal(|| 0u64);
    let mut action = use_signal(|| None::<(Uuid, BalanceReservationInfo)>);
    let mut reason = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut notice = use_signal(String::new);
    super::workspace_switcher::use_workspace_dirty_blocker(move || action().is_some());

    let resource = use_resource(move || {
        let key = (scope, selected(), cursor(), generation());
        async move {
            let owner = key.1;
            let current_cursor = key.2.clone();
            let result = if let Some(owner) = owner {
                common::read(auth, users, scope, move |token| {
                    let current_cursor = current_cursor.clone();
                    async move {
                        TenantFinancialControlApi::new(&get_client(), scope.tenant_id)?
                            .reservations(owner, current_cursor.as_deref(), PAGE as u64, &token)
                            .await
                            .map(Some)
                    }
                })
                .await
            } else {
                Ok(None)
            };
            (key, result)
        }
    });
    let key = (scope, selected(), cursor(), generation());
    let loaded = resource()
        .filter(|value| value.0 == key)
        .map(|value| value.1);

    let apply = move |_| match owner(&draft()) {
        Ok(value) => {
            selected.set(Some(value));
            cursor.set(None);
            history.set(Vec::new());
            action.set(None);
            reason.set(String::new());
            error.set(String::new());
            notice.set(String::new());
        }
        Err(value) => error.set(user_error_message(i18n, &value)),
    };

    let confirm = move |_| {
        let Some((owner, row)) = action() else { return };
        let why = reason();
        if busy() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        notice.set(String::new());
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| async move {
                TenantFinancialControlApi::new(&get_client(), scope.tenant_id)?
                    .release_expired(owner, &row, &why, &token)
                    .await
            })
            .await;
            busy.set(false);
            match result {
                Ok(value) => {
                    notice.set(format!("{} · {}", value.message, value.warning));
                    action.set(None);
                    reason.set(String::new());
                    generation += 1;
                }
                Err(value) => error.set(user_error_message(i18n, &value)),
            }
        });
    };

    rsx! {
        section { class: "tenant-reservation-control",
            h2 { {i18n.t("tenant_financial_controls.reservations")} }
            p { class: "text-secondary", {i18n.t("tenant_financial_controls.reservation_hint")} }
            div { class: "toolbar",
                MemberIdField { scope, input_id: "financial-owner".to_string(), label: i18n.t("tenant_financial_controls.owner").to_string(), value: draft(), on_input: move |value| draft.set(value) }
                button { class: "btn btn-secondary", onclick: apply, {i18n.t("tenant_financial_controls.load")} }
                button {
                    class: "btn btn-secondary",
                    disabled: selected().is_none(),
                    onclick: move |_| { notice.set(String::new()); generation += 1; },
                    {i18n.t("tenant.reload")}
                }
            }
            if !error().is_empty() {
                p { role: "alert", class: "alert alert-error", "{error}" }
            }
            if !notice().is_empty() {
                p { role: "status", class: "alert alert-info", "{notice}" }
            }
            match loaded {
                None => if selected().is_some() {
                    rsx! { p { role: "status", {i18n.t("common.loading")} } }
                } else {
                    rsx! { p { class: "alert alert-info", {i18n.t("tenant_financial_controls.choose_owner")} } }
                },
                Some(Err(value)) => rsx! { p { role: "alert", class: "alert alert-error", {user_error_message(i18n, &value)} } },
                Some(Ok(None)) => rsx! { p { class: "alert alert-info", {i18n.t("tenant_financial_controls.choose_owner")} } },
                Some(Ok(Some(value))) => rsx! {
                    ReservationPage {
                        page: value.clone(),
                        can_back: !history().is_empty(),
                        on_release: move |row| {
                            if let Some(owner) = selected() {
                                reason.set(String::new());
                                error.set(String::new());
                                notice.set(String::new());
                                action.set(Some((owner, row)));
                            }
                        },
                        on_next: move |next| {
                            let previous = cursor();
                            history.write().push(previous);
                            cursor.set(Some(next));
                        },
                        on_back: move |_| {
                            if let Some(previous) = history.write().pop() {
                                cursor.set(previous);
                            }
                        },
                    }
                },
            }
            if let Some((_, row)) = action() {
                div { class: "modal-backdrop",
                    div { class: "modal", role: "dialog", aria_modal: "true", aria_label: i18n.t("tenant_financial_controls.release"),
                        div { class: "modal-header", h2 { {i18n.t("tenant_financial_controls.release")} } }
                        div { class: "modal-body",
                            p { TechnicalId { value: row.request_id.to_string() } }
                            p { class: "alert alert-warning", {i18n.t("tenant_financial_controls.release_warning")} }
                            label { r#for: "financial-recovery-reason", {i18n.t("tenant_financial_controls.reason")} }
                            textarea {
                                id: "financial-recovery-reason",
                                class: "input-field",
                                maxlength: "500",
                                value: "{reason}",
                                oninput: move |event| reason.set(event.value()),
                            }
                        }
                        div { class: "modal-footer",
                            button { class: "btn btn-secondary", disabled: busy(), onclick: move |_| action.set(None), {i18n.t("form.cancel")} }
                            button { class: "btn btn-primary", disabled: busy(), onclick: confirm, {i18n.t("tenant.confirm")} }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ReservationPage(
    page: UserBalanceReservationsResponse,
    can_back: bool,
    on_release: EventHandler<BalanceReservationInfo>,
    on_next: EventHandler<String>,
    on_back: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let next_cursor = page.next_cursor.clone();
    rsx! {
        div { class: "reservation-page",
            div { style: "display:grid;grid-template-columns:repeat(auto-fit,minmax(190px,1fr));gap:12px",
                article { class: "card", h3 { {i18n.t("tenant_finance.available")} } p { "{page.available_balance}" } }
                article { class: "card", h3 { {i18n.t("tenant_finance.frozen")} } p { "{page.total_frozen_balance}" } }
                article { class: "card", h3 { {i18n.t("tenant_financial_controls.request_reserved")} } p { "{page.request_reserved_balance}" } }
                article { class: "card", h3 { {i18n.t("tenant_financial_controls.manual_frozen")} } p { "{page.manually_frozen_balance}" } }
            }
            div { class: "table-container",
                table { class: "data-table",
                    thead { tr {
                        th { {i18n.t("tenant_financial_controls.request")} }
                        th { {i18n.t("tenant_finance.amount")} }
                        th { {i18n.t("tenant_financial_controls.expires")} }
                        th { {i18n.t("tenant.actions")} }
                    } }
                    tbody {
                        for row in &page.reservations {
                            {
                                let selected_row = row.clone();
                                rsx! { tr { key: "{row.request_id}",
                                    td { TechnicalId { value: row.request_id.to_string() } p { class: "table-meta", "v {row.version}" } }
                                    td { "{row.amount}" }
                                    td { {format_time(&row.expires_at)} }
                                    td { button {
                                        class: "btn btn-danger btn-sm",
                                        disabled: !expired(&row.expires_at),
                                        onclick: move |_| on_release.call(selected_row.clone()),
                                        {i18n.t("tenant_financial_controls.release")}
                                    } }
                                } }
                            }
                        }
                    }
                }
            }
            if page.reservations.is_empty() {
                div { class: "empty-state compact-empty-state", h3 { class: "empty-title", {i18n.t("tenant.empty")} } }
            }
            if can_back || next_cursor.is_some() {
                div { class: "toolbar",
                    button { class: "btn btn-secondary", disabled: !can_back, onclick: move |_| on_back.call(()), {i18n.t("tenant.previous")} }
                if let Some(next) = next_cursor {
                    button { class: "btn btn-secondary", onclick: move |_| on_next.call(next.clone()), {i18n.t("tenant.next")} }
                }
                }
            }
        }
    }
}

#[component]
fn WithdrawalPanel(scope: WorkspaceScope) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut status = use_signal(String::new);
    let mut applied = use_signal(String::new);
    let mut page = use_signal(|| 1u32);
    let mut generation = use_signal(|| 0u64);
    let mut action = use_signal(|| None::<(WithdrawalDecision, TenantWithdrawal)>);
    let mut reason = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut notice = use_signal(String::new);
    super::workspace_switcher::use_workspace_dirty_blocker(move || action().is_some());

    let key = (scope, applied(), page(), generation());
    let resource = use_resource(move || {
        let key = (scope, applied(), page(), generation());
        async move {
            let selected_status = (!key.1.is_empty()).then(|| key.1.clone());
            let current_page = key.2;
            let result = common::read(auth, users, scope, move |token| {
                let selected_status = selected_status.clone();
                async move {
                    TenantFinancialControlApi::new(&get_client(), scope.tenant_id)?
                        .withdrawals(
                            &WithdrawalQuery {
                                status: selected_status,
                                limit: PAGE,
                                offset: (current_page - 1) * PAGE,
                            },
                            &token,
                        )
                        .await
                }
            })
            .await;
            (key, result)
        }
    });
    let loaded = resource()
        .filter(|value| value.0 == key)
        .map(|value| value.1);

    let confirm = move |_| {
        let Some((decision, row)) = action() else {
            return;
        };
        let why = reason();
        if busy() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        notice.set(String::new());
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| async move {
                TenantFinancialControlApi::new(&get_client(), scope.tenant_id)?
                    .review(&row, decision, &why, &token)
                    .await
            })
            .await;
            busy.set(false);
            match result {
                Ok(value) => {
                    notice.set(format!(
                        "{} · r{}",
                        withdrawal_status_label(i18n, &value.status),
                        value.revision
                    ));
                    action.set(None);
                    reason.set(String::new());
                    generation += 1;
                }
                Err(value) => error.set(user_error_message(i18n, &value)),
            }
        });
    };

    rsx! {
        section { class: "tenant-withdrawal-control",
            h2 { {i18n.t("tenant_financial_controls.withdrawals")} }
            p { class: "text-secondary", {i18n.t("tenant_financial_controls.withdrawal_hint")} }
            div { class: "toolbar",
                label { r#for: "financial-withdrawal-status", {i18n.t("tenant_finance.state")} }
                select {
                    id: "financial-withdrawal-status",
                    class: "input-field",
                    value: "{status}",
                    onchange: move |event| status.set(event.value()),
                    option { value: "", {i18n.t("tenant_finance.all_states")} }
                    option { value: "pending", {i18n.t("tenant_financial_controls.pending")} }
                    option { value: "approved", {i18n.t("tenant_financial_controls.approved")} }
                    option { value: "completed", {i18n.t("tenant_financial_controls.completed")} }
                    option { value: "rejected", {i18n.t("tenant_financial_controls.rejected")} }
                }
                button { class: "btn btn-secondary", onclick: move |_| { applied.set(status()); page.set(1); action.set(None); reason.set(String::new()); error.set(String::new()); notice.set(String::new()); }, {i18n.t("tenant_financial_controls.apply")} }
                button { class: "btn btn-secondary", onclick: move |_| { notice.set(String::new()); generation += 1; }, {i18n.t("tenant.reload")} }
            }
            if !error().is_empty() {
                p { role: "alert", class: "alert alert-error", "{error}" }
            }
            if !notice().is_empty() {
                p { role: "status", class: "alert alert-info", "{notice}" }
            }
            match loaded {
                None => rsx! { p { role: "status", {i18n.t("common.loading")} } },
                Some(Err(value)) => rsx! { p { role: "alert", class: "alert alert-error", {user_error_message(i18n, &value)} } },
                Some(Ok(value)) => rsx! {
                    WithdrawalTable {
                        data: value.clone(),
                        page: page(),
                        on_page: move |value| page.set(value),
                        on_review: move |(decision, row)| {
                            reason.set(String::new());
                            error.set(String::new());
                            notice.set(String::new());
                            action.set(Some((decision, row)));
                        },
                    }
                },
            }
            if let Some((decision, row)) = action() {
                div { class: "modal-backdrop",
                    div { class: "modal", role: "dialog", aria_modal: "true", aria_label: i18n.t("tenant_financial_controls.review"),
                        div { class: "modal-header", h2 { {i18n.t("tenant_financial_controls.review")} } }
                        div { class: "modal-body",
                            p { "{row.currency} {row.total_amount}" }
                            p { TechnicalId { value: row.id.to_string() } }
                            p { class: "text-secondary",
                                {if decision == WithdrawalDecision::Approve {
                                    i18n.t("tenant_financial_controls.approve_hint")
                                } else {
                                    i18n.t("tenant_financial_controls.reject_hint")
                                }}
                            }
                            label { r#for: "financial-review-reason", {i18n.t("tenant_financial_controls.reason")} }
                            textarea {
                                id: "financial-review-reason",
                                class: "input-field",
                                maxlength: "500",
                                value: "{reason}",
                                oninput: move |event| reason.set(event.value()),
                            }
                        }
                        div { class: "modal-footer",
                            button { class: "btn btn-secondary", disabled: busy(), onclick: move |_| action.set(None), {i18n.t("form.cancel")} }
                            button { class: "btn btn-primary", disabled: busy(), onclick: confirm, {i18n.t("tenant.confirm")} }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn WithdrawalTable(
    data: TenantWithdrawalPage,
    page: u32,
    on_page: EventHandler<u32>,
    on_review: EventHandler<(WithdrawalDecision, TenantWithdrawal)>,
) -> Element {
    let i18n = use_i18n();
    let total_pages = pages(data.total);
    rsx! {
        div { class: "table-pagination-panel",
            div { class: "table-container",
                table { class: "data-table",
                    thead { tr {
                        th { {i18n.t("tenant_financial_controls.owner")} }
                        th { {i18n.t("tenant_finance.amount")} }
                        th { {i18n.t("tenant_finance.state")} }
                        th { {i18n.t("tenant_financial_controls.kind")} }
                        th { {i18n.t("tenant_financial_controls.created")} }
                        th { {i18n.t("tenant.actions")} }
                    } }
                    tbody {
                        for row in &data.items {
                            {
                                let approve = row.clone();
                                let reject = row.clone();
                                rsx! { tr { key: "{row.id}",
                                    td { TechnicalId { value: row.owner_user_id.to_string() } details { summary { "ID" } code { "{row.id}" } } }
                                    td { "{row.currency} {row.total_amount}" }
                                    td { {withdrawal_status_label(i18n, &row.status)} " · r{row.revision}" }
                                    td { {withdrawal_type_label(i18n, &row.withdrawal_type)} }
                                    td { {format_time(&row.created_at)} }
                                    td {
                                        if row.status == "pending" && row.withdrawal_type == "alipay" {
                                            div { class: "table-actions",
                                                button { class: "btn btn-primary btn-sm", onclick: move |_| on_review.call((WithdrawalDecision::Approve, approve.clone())), {i18n.t("tenant_financial_controls.approve")} }
                                                button { class: "btn btn-danger btn-sm", onclick: move |_| on_review.call((WithdrawalDecision::Reject, reject.clone())), {i18n.t("tenant_financial_controls.reject")} }
                                            }
                                        }
                                    }
                                } }
                            }
                        }
                    }
                }
            }
            if data.items.is_empty() {
                div { class: "empty-state compact-empty-state", h3 { class: "empty-title", {i18n.t("tenant.empty")} } }
            }
            Pager { page, total_pages, total: data.total, on_page }
        }
    }
}
