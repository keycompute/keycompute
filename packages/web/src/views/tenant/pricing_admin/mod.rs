//! Tenant-only pricing editor. Platform defaults are not tenant-owned records.
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
};
use client_api::api::tenant_pricing::TenantPricingApi;
use dioxus::prelude::*;
use types::{Operation, Query};

#[component]
pub fn TenantPricing() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let scope = WorkspaceScope::from_stores(auth, users);
    let allowed = users
        .info
        .read()
        .as_ref()
        .is_some_and(|u| u.can_manage_tenant());
    rsx! {if let Some(scope)=scope.filter(|_|allowed){for key in [format!("{scope:?}")]{PricingWorkspace{key:"{key}",scope}}}
    else {p {role:"alert",{i18n.t("tenant.admin_required")}}}}
}
#[component]
fn PricingWorkspace(scope: WorkspaceScope) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut query = use_signal(Query::default);
    let mut search = use_signal(String::new);
    let mut operation = use_signal(|| None::<Operation>);
    let mut generation = use_signal(|| 0u64);
    let mut data = use_resource(move || {
        let q = query();
        let key = (scope, q.clone(), generation());
        async move {
            let result = common::read(auth, users, scope, move |token| {
                let q = q.clone();
                async move {
                    TenantPricingApi::new(&get_client(), scope.tenant_id)?
                        .list(q.page, 20, &q.search, &token)
                        .await
                }
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let key = (scope, query(), generation());
    let loaded = current_keyed_value(&key, data.state().cloned(), data());
    let mut open = move |op| operation.set(Some(op));
    rsx! {div {class:"page-container tenant-pricing-admin",
        ui::PageHeader{title:i18n.t("tenant_pricing.title").to_string(),description:i18n.t("tenant_pricing.hint").to_string()}
        WorkspaceLinks{}
        p {class:"alert alert-info",{i18n.t("tenant_pricing.scope")} " {scope.tenant_id}"}
        div {class:"toolbar",
            label {r#for:"tenant-pricing-search",{i18n.t("tenant_pricing.search")}}
            input {id:"tenant-pricing-search",class:"input-field",value:"{search}",maxlength:"255",oninput:move|e|search.set(e.value())}
            button {class:"btn btn-secondary",onclick:move |_|query.set(Query{search:search().trim().into(),page:1}),{i18n.t("tenant_pricing.search")}}
            button {class:"btn btn-secondary",onclick:move |_|data.restart(),{i18n.t("tenant.reload")}}
            button {class:"btn btn-primary",onclick:move |_|open(Operation::Create),{i18n.t("tenant_pricing.create")}}
        }
        match loaded {
            None=>rsx!{p{role:"status",{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{p{class:"alert alert-error",role:"alert",{user_error_message(&e)}}},
            Some(Ok(page))=>rsx!{
                div {class:"table-pagination-panel",div {class:"tenant-pricing-table-scroll",table {class:"table",
                    thead {tr {th {{i18n.t("tenant_pricing.model")}} th {{i18n.t("tenant_pricing.dimension")}} th {{i18n.t("tenant_pricing.prices")}} th {{i18n.t("tenant_pricing.validity")}} th {{i18n.t("tenant.actions")}}}}
                    tbody {for row in &page.pricing {{
                        let edit=row.clone();let del=row.clone();let default=row.clone();
                        rsx!{tr {key:"{row.id}",
                            td {span {"{row.model_name}"} details {summary {"ID"} code {"{row.id}"}}}
                            td {"{row.billing_dimension.as_str()}"}
                            td {"{row.currency}" p {"{row.input_price_per_1k} / {row.output_price_per_1k}"}}
                            td {p {{i18n.t(if row.is_effective{"tenant_pricing.effective"}else{"tenant_pricing.ineffective"})}}
                                if row.is_default {span {class:"badge",{i18n.t("tenant_pricing.default_badge")}}}
                                details {summary {{i18n.t("tenant_pricing.window")}} p {"{row.effective_from}"} p {{row.effective_until.as_deref().unwrap_or("—")}} p {{i18n.t("tenant_pricing.version")} ": {row.version}"}}}
                            td {button {class:"btn btn-secondary btn-sm",onclick:move |_|open(Operation::Edit(edit.clone())),{i18n.t("tenant_pricing.edit")}}
                                button {class:"btn btn-secondary btn-sm",disabled:row.is_default,onclick:move |_|open(Operation::Default(default.clone())),{i18n.t("tenant_pricing.default")}}
                                button {class:"btn btn-danger btn-sm",onclick:move |_|open(Operation::Delete(del.clone())),{i18n.t("tenant_pricing.delete")}}}
                        }}
                    }}}
                }} if page.pricing.is_empty(){p {{i18n.t("tenant.empty")}}}}
                Pager {page:query().page,total_pages:page.total_pages,total:page.total,on_page:move|page|query.write().page=page}
            }
        }
        if let Some(op)=operation(){for key in [format!("{}:{}",op.label(),op.row().map(|v|v.id.to_string()).unwrap_or_default())]{
            command::Editor {key:"{key}",scope,op:op.clone(),on_close:move |_|operation.set(None),on_changed:move |_|{operation.set(None);generation+=1;}}
        }}
    }}
}
