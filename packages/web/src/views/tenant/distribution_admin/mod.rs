//! Current-tenant distribution allocation policy controls. This page does not settle or pay earnings.
mod command;
#[cfg(test)]
mod tests;
mod types;

use super::common::{self, Pager, WorkspaceLinks, WorkspaceScope};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
    utils::time::format_time,
};
use client_api::api::distribution_policy::DistributionPolicyApi;
use dioxus::prelude::*;
use types::{Operation, Query};

#[component]
pub fn TenantDistribution() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let scope = WorkspaceScope::from_stores(auth, users);
    let allowed = users
        .info
        .read()
        .as_ref()
        .is_some_and(|u| u.can_manage_tenant());
    rsx! {
        if let Some(scope)=scope.filter(|_|allowed) {
            for key in [format!("{scope:?}")] { DistributionWorkspace {key:"{key}",scope} }
        } else { p {role:"alert",{i18n.t("tenant.admin_required")}} }
    }
}

#[component]
fn DistributionWorkspace(scope: WorkspaceScope) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut query = use_signal(Query::default);
    let mut generation = use_signal(|| 0u64);
    let mut operation = use_signal(|| None::<Operation>);
    let mut data = use_resource(move || {
        let q = query();
        let key = (scope, q.clone(), generation());
        async move {
            let result = common::read(auth, users, scope, move |token| async move {
                DistributionPolicyApi::tenant(&get_client(), scope.tenant_id)?
                    .list(q.page, 20, &token)
                    .await
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let key = (scope, query(), generation());
    let loaded = current_keyed_value(&key, data.state().cloned(), data());
    let mut open = move |value| operation.set(Some(value));
    rsx! {
        div {class:"page-container tenant-distribution-admin",
            ui::PageHeader {title:i18n.t("tenant_distribution.title").to_string(),description:i18n.t("tenant_distribution.hint").to_string()}
            WorkspaceLinks {}
            div {class:"scope-banner",
                span {class:"scope-banner-label",{i18n.t("tenant_distribution.scope")}}
                code {"{scope.tenant_id}"}
            }
            div {class:"toolbar",
                button {class:"btn btn-secondary",onclick:move |_|data.restart(),{i18n.t("tenant.reload")}}
                button {class:"btn btn-secondary",onclick:move |_|open(Operation::Default),{i18n.t("tenant_distribution.default")}}
                button {class:"btn btn-primary",onclick:move |_|open(Operation::Create),{i18n.t("tenant_distribution.create")}}
            }
            match loaded {
                None => rsx! {
                    div {class:"content-loading",role:"status",
                        span {class:"spinner",aria_hidden:"true"}
                        span {{i18n.t("common.loading")}}
                    }
                },
                Some(Err(e)) => rsx! { p {class:"alert alert-error",role:"alert",{user_error_message(i18n, &e)}} },
                Some(Ok(page)) => rsx! {
                    div {class:"table-pagination-panel table-pagination-frame",
                        div {class:"table-container",
                            table {class:"table",
                                thead {tr {
                                    th {{i18n.t("tenant_distribution.name")}}
                                    th {{i18n.t("tenant_distribution.beneficiary")}}
                                    th {{i18n.t("tenant_distribution.rate")}}
                                    th {{i18n.t("tenant_distribution.state")}}
                                    th {{i18n.t("tenant_distribution.window")}}
                                    th {{i18n.t("tenant.actions")}}
                                }}
                                tbody {
                                    for row in &page.rules {
                                        {let edit=row.clone();let del=row.clone();rsx! {
                                            tr {key:"{row.id}",
                                                td {strong {"{row.name}"} details {summary {"ID"} code {"{row.id}"}}}
                                                td {if let Some(id)=row.beneficiary_id {code {"{id}"}} else {{i18n.t("tenant_distribution.everyone")}}}
                                                td {"{row.commission_rate}"}
                                                td {p {if row.is_active {{i18n.t("tenant_distribution.active")}} else {{i18n.t("tenant_distribution.inactive")}}} small {class:"table-meta",{i18n.t("tenant_distribution.priority")} " {row.priority}"}}
                                                td {details {summary {{i18n.t("tenant_distribution.window")}}
                                                    p {{format_time(&row.effective_from)}}
                                                    p {{row.effective_until.as_deref().map(format_time).unwrap_or_else(|| "—".into())}}
                                                    p {code {{format_time(&row.updated_at)}}}
                                                }}
                                                td {
                                                    button {class:"btn btn-secondary btn-sm",onclick:move |_|open(Operation::Edit(edit.clone())),{i18n.t("form.edit")}}
                                                    button {class:"btn btn-danger btn-sm",onclick:move |_|open(Operation::Delete(del.clone())),{i18n.t("form.delete")}}
                                                }
                                            }
                                        }}
                                    }
                                }
                            }
                        }
                        if page.rules.is_empty() {
                            div {class:"empty-state compact-empty-state",
                                h3 {class:"empty-title",{i18n.t("tenant.empty")}}
                            }
                        }
                    }
                    Pager {page:query().page,total_pages:page.total_pages,total:page.total,on_page:move|p|query.write().page=p}
                },
            }
            if let Some(current)=operation() {
                for identity in [current.key()] {
                    command::Editor {key:"{identity}",scope,op:current.clone(),on_close:move |_|operation.set(None),on_changed:move |_|{operation.set(None);generation+=1;}}
                }
            }
        }
    }
}
