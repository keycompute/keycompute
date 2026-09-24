//! Read-only current-tenant usage, order and per-member wallet views.
mod inspector;
#[cfg(test)]
mod tests;
mod types;
use super::common::{self, Pager, WorkspaceLinks, WorkspaceScope};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::{
        resource::{KeyedResourceValue, current_keyed_value},
        time::format_time,
    },
};
use client_api::api::tenant_reporting::{
    MemberWallet, ReportPage, TenantPaymentRecord, TenantReportingApi, TenantUsageRecord,
};
use dioxus::prelude::*;
use types::{Detail, Query, Tab};
#[derive(Clone)]
enum Rows {
    Usage(ReportPage<TenantUsageRecord>),
    Orders(ReportPage<TenantPaymentRecord>),
    Wallet(MemberWallet),
    ChooseOwner,
}
#[component]
pub fn TenantFinance() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let allowed = users
        .info
        .read()
        .as_ref()
        .is_some_and(|v| v.can_manage_tenant());
    rsx! {if let Some(scope)=WorkspaceScope::from_stores(auth,users).filter(|_|allowed){for key in [format!("{scope:?}")]{FinanceWorkspace{key:"{key}",scope}}}else{p{role:"alert",{i18n.t("tenant.admin_required")}}}}
}
#[component]
fn FinanceWorkspace(scope: WorkspaceScope) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut query = use_signal(Query::default);
    let mut owner = use_signal(String::new);
    let mut from = use_signal(move || query.peek().window.from.clone());
    let mut to = use_signal(move || query.peek().window.to.clone());
    let mut status = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut detail = use_signal(|| None::<Detail>);
    let mut generation = use_signal(|| 0u64);
    let data = use_resource(move || {
        let q = query();
        let key = (scope, q.clone(), generation());
        async move {
            let result = common::read(auth, users, scope, move |token| {
                let q = q.clone();
                async move {
                    let api = TenantReportingApi::new(&get_client(), scope.tenant_id)?;
                    match q.tab {
                        Tab::Usage => api
                            .usage(&q.report(), &q.window, &token)
                            .await
                            .map(Rows::Usage),
                        Tab::Orders => api
                            .payments(&q.report(), q.status, &token)
                            .await
                            .map(Rows::Orders),
                        Tab::Wallet => match q.owner {
                            Some(owner) => api.wallet(owner, &token).await.map(Rows::Wallet),
                            None => Ok(Rows::ChooseOwner),
                        },
                    }
                }
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });
    // Summary is independent of page number: changing pages cannot refetch expensive totals.
    let totals_key = use_memo(move || {
        let q = query();
        (
            scope,
            q.tab == Tab::Usage,
            q.owner,
            q.window.clone(),
            generation(),
        )
    });
    let totals = use_resource(move || {
        let key = totals_key();
        async move {
            let result = if key.1 {
                common::read(auth, users, scope, {
                    let w = key.3.clone();
                    move |token| {
                        let w = w.clone();
                        async move {
                            TenantReportingApi::new(&get_client(), scope.tenant_id)?
                                .totals(&w, key.2, &token)
                                .await
                                .map(Some)
                        }
                    }
                })
                .await
            } else {
                Ok(None)
            };
            KeyedResourceValue::new(key, result)
        }
    });
    let loaded = current_keyed_value(
        &(scope, query(), generation()),
        data.state().cloned(),
        data(),
    );
    let summary = current_keyed_value(&totals_key(), totals.state().cloned(), totals());
    let mut select_tab = move |tab| {
        query.write().tab = tab;
        query.write().page = 1;
        detail.set(None);
        error.set(String::new());
    };
    let apply = move |_| {
        let mut next = query();
        let result = (|| {
            next.owner = types::owner(&owner())?;
            match next.tab {
                Tab::Usage => next.window = types::window(&from(), &to())?,
                Tab::Orders => next.status = types::status(&status())?,
                Tab::Wallet => {}
            };
            Ok::<_, client_api::ClientError>(next)
        })();
        match result {
            Ok(mut q) => {
                q.page = 1;
                query.set(q);
                detail.set(None);
                error.set(String::new());
            }
            Err(e) => error.set(user_error_message(&e)),
        }
    };
    rsx! {div{class:"page-container tenant-finance",
        ui::PageHeader{title:i18n.t("tenant_finance.title").to_string(),description:i18n.t("tenant_finance.hint").to_string()}
        WorkspaceLinks{}
        p{class:"alert alert-info",{i18n.t("tenant_finance.shared_payment")} " · {scope.tenant_id}"}
        div{class:"toolbar",style:"display:flex;flex-wrap:wrap;gap:12px",
            button{class:"btn btn-secondary",onclick:move |_|select_tab(Tab::Usage),{i18n.t("tenant_finance.usage")}}
            button{class:"btn btn-secondary",onclick:move |_|select_tab(Tab::Orders),{i18n.t("tenant_finance.orders")}}
            button{class:"btn btn-secondary",onclick:move |_|select_tab(Tab::Wallet),{i18n.t("tenant_finance.wallet")}}
            label{r#for:"finance-owner",{i18n.t("tenant_finance.owner")}}input{id:"finance-owner",class:"input-field",style:"width:25rem;max-width:100%",value:"{owner}",maxlength:"36",oninput:move|e|owner.set(e.value())}
            if query().tab==Tab::Usage{
                label{r#for:"finance-from",{i18n.t("tenant_finance.from")}}input{id:"finance-from",class:"input-field",style:"width:22rem;max-width:100%",value:"{from}",maxlength:"128",oninput:move|e|from.set(e.value())}
                label{r#for:"finance-to",{i18n.t("tenant_finance.to")}}input{id:"finance-to",class:"input-field",style:"width:22rem;max-width:100%",value:"{to}",maxlength:"128",oninput:move|e|to.set(e.value())}
            }
            if query().tab==Tab::Orders{label{r#for:"finance-payment-state",{i18n.t("tenant_finance.state")}}select{id:"finance-payment-state",class:"input-field",value:"{status}",onchange:move|e|status.set(e.value()),option{value:"",{i18n.t("tenant_finance.all_states")}}for value in ["pending","paid","failed","closed"]{option{value,"{value}"}}}}
            button{class:"btn btn-secondary",onclick:apply,{i18n.t("tenant_finance.apply")}}
            button{class:"btn btn-secondary",onclick:move |_|{detail.set(None);generation+=1;},{i18n.t("tenant.reload")}}
        }
        p{class:"text-secondary",{i18n.t("tenant_finance.active_owner")} " " {query().owner.map(|id|id.to_string()).unwrap_or_else(||i18n.t("tenant_finance.all_owners").into())}}
        if query().tab==Tab::Usage{p{class:"text-secondary",{i18n.t("tenant_finance.window")} " {query().window.from} — {query().window.to}"}}
        if !error().is_empty(){p{role:"alert",class:"alert alert-error","{error}"}}
        if query().tab==Tab::Usage{
            match summary{
                Some(Ok(Some(totals)))=>rsx!{section{class:"finance-currency-totals",aria_label:i18n.t("tenant_finance.currency_totals"),
                    h2{{i18n.t("tenant_finance.currency_totals")}}
                    div{style:"display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px",for g in &totals.currencies{article{class:"card",key:"{g.currency}",h3{"{g.currency}"}p{{i18n.t("tenant_finance.amount")} ": {g.total_amount}"}p{{i18n.t("tenant_finance.requests")} ": {g.total_requests}"}p{{i18n.t("tenant_finance.tokens")} ": {g.total_input_tokens} / {g.total_output_tokens} / {g.total_tokens}"}}}}
                    if totals.currencies.is_empty(){p{{i18n.t("tenant.empty")}}}
                }},
                Some(Err(e))=>rsx!{p{role:"alert",class:"alert alert-error",{user_error_message(&e)}}},
                _=>rsx!{p{role:"status",{i18n.t("common.loading")}}},
            }
        }
        match loaded{
            None=>rsx!{p{role:"status",{i18n.t("common.loading")}}},Some(Err(e))=>rsx!{p{role:"alert",class:"alert alert-error",{user_error_message(&e)}}},
            Some(Ok(Rows::ChooseOwner))=>rsx!{p{class:"alert alert-info",{i18n.t("tenant_finance.choose_wallet")}}},
            Some(Ok(Rows::Wallet(wallet)))=>rsx!{section{class:"card tenant-member-wallet",h2{{i18n.t("tenant_finance.wallet")}}p{code{"{wallet.user_id}"}}p{class:"text-secondary",{i18n.t("tenant_finance.wallet_scope")}}
                p{{i18n.t("tenant_finance.available")} ": {wallet.available_balance}"}p{{i18n.t("tenant_finance.frozen")} ": {wallet.frozen_balance}"}
                p{{i18n.t("tenant_finance.recharged")} ": {wallet.total_recharged}"}p{{i18n.t("tenant_finance.consumed")} ": {wallet.total_consumed}"}
                p{{i18n.t("tenant_finance.as_of")} ": " {format_time(&wallet.as_of)}}
                if !wallet.initialized{p{class:"alert alert-info",{i18n.t("tenant_finance.uninitialized")}}}
            }},
            Some(Ok(Rows::Usage(p)))=>rsx!{UsageTable{page:p,on_detail:move|row|detail.set(Some(Detail::Usage(row))),on_page:move|page|{query.write().page=page;detail.set(None);}}},
            Some(Ok(Rows::Orders(p)))=>rsx!{PaymentTable{page:p,on_detail:move|row|detail.set(Some(Detail::Order(row))),on_page:move|page|{query.write().page=page;detail.set(None);}}},
        }
        if let Some(d)=detail(){for key in [d.key()]{inspector::Inspector{key:"{key}",scope,detail:d.clone(),on_close:move |_|detail.set(None)}}}
    }}
}

#[component]
fn UsageTable(
    page: ReportPage<TenantUsageRecord>,
    on_detail: EventHandler<TenantUsageRecord>,
    on_page: EventHandler<u32>,
) -> Element {
    let i18n = use_i18n();
    rsx! {
        div { class:"table-pagination-panel",
            div { style:"overflow-x:auto",
                table { class:"table",
                    thead { tr {
                        th {{i18n.t("tenant_finance.model")}}
                        th {{i18n.t("tenant_finance.owner")}}
                        th {{i18n.t("tenant_finance.tokens")}}
                        th {{i18n.t("tenant_finance.amount")}}
                        th {{i18n.t("tenant_finance.created")}}
                        th {{i18n.t("tenant.actions")}}
                    }}
                    tbody {
                        for row in &page.items {
                            {let selected=row.clone();rsx! {tr {key:"{row.id}",
                                td { p {"{row.model_name}"} p {"{row.provider_name}"} details {summary {"ID"} code {"{row.id}"}} }
                                td {code {"{row.user_id}"}}
                                td {"{row.input_tokens} / {row.output_tokens} / {row.total_tokens}"}
                                td {"{row.currency} {row.user_amount}"}
                                td {{format_time(&row.created_at)}}
                                td {button {class:"btn btn-secondary",onclick:move |_|on_detail.call(selected.clone()),{i18n.t("tenant_finance.details")}}}
                            }}}
                        }
                    }
                }
            }
            if page.items.is_empty() {p {{i18n.t("tenant.empty")}}}
        }
        Pager {page:page.page,total_pages:page.total_pages,total:page.total,on_page}
    }
}
#[component]
fn PaymentTable(
    page: ReportPage<TenantPaymentRecord>,
    on_detail: EventHandler<TenantPaymentRecord>,
    on_page: EventHandler<u32>,
) -> Element {
    let i18n = use_i18n();
    rsx! {
        div {class:"table-pagination-panel",
            div {style:"overflow-x:auto",
                table {class:"table",
                    thead {tr {
                        th {{i18n.t("tenant_finance.order")}}
                        th {{i18n.t("tenant_finance.owner")}}
                        th {{i18n.t("tenant_finance.amount")}}
                        th {{i18n.t("tenant_finance.state")}}
                        th {{i18n.t("tenant_finance.created")}}
                        th {{i18n.t("tenant.actions")}}
                    }}
                    tbody {
                        for row in &page.items {
                            {let selected=row.clone();rsx! {tr {key:"{row.id}",
                                td {code {"{row.id}"} p {"{row.payment_method} / {row.payment_scene}"}}
                                td {code {"{row.user_id}"}}
                                td {"{row.currency} {row.amount}"}
                                td {"{row.status.as_str()}"}
                                td {{format_time(&row.created_at)}}
                                td {button {class:"btn btn-secondary",onclick:move |_|on_detail.call(selected.clone()),{i18n.t("tenant_finance.details")}}}
                            }}}
                        }
                    }
                }
            }
            if page.items.is_empty() {p {{i18n.t("tenant.empty")}}}
        }
        Pager {page:page.page,total_pages:page.total_pages,total:page.total,on_page}
    }
}
