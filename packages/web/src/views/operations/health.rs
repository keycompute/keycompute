use super::scope::OperationsScope;
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::api::platform_operations::{PlatformOperationsApi, TenantHealthQuery};
use dioxus::prelude::*;
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
    rsx! {section {class:"operations-health",
        div {class:"card operations-filters",
            label {r#for:"operations-search",{i18n.t("operations.search")}}
            input {id:"operations-search",class:"input-field",value:"{search}",maxlength:"128",oninput:move |e|search.set(e.value())}
            label {r#for:"operations-status",{i18n.t("operations.status")}}
            select {id:"operations-status",class:"input-field",value:"{status}",onchange:move |e|status.set(e.value()),
                option {value:"",{i18n.t("operations.all_status")}} option {value:"active","active"} option {value:"inactive","inactive"}}
            div {class:"toolbar",
                button {class:"btn btn-primary",onclick:move |_|{
                    let next=Filter{search:search().trim().into(),status:status(),offset:0};
                    if next.query().is_none(){error.set(i18n.t("operations.invalid_search").into());return;}
                    error.set(String::new());selected.set(None);
                    if filter()==next {data.restart();}else{filter.set(next);}
                },{i18n.t("operations.apply")}}
                button {class:"btn btn-secondary",onclick:move |_|data.restart(),{i18n.t("tenant.reload")}}
            }
        }
        if !error().is_empty(){p {class:"alert alert-error",role:"alert","{error}"}}
        match loaded {
            None=>rsx!{p {role:"status",{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{p {class:"alert alert-error",role:"alert",{user_error_message(&e)}}},
            Some(Ok(value))=>rsx!{
                p {class:"text-secondary",{i18n.t("operations.as_of")} " {value.as_of}"}
                div {class:"table-pagination-panel operations-table",table {class:"table",
                    thead {tr {th {{i18n.t("operations.tenant")}} th {{i18n.t("operations.status")}} th {{i18n.t("operations.members")}} th {{i18n.t("operations.providers")}} th {{i18n.t("operations.nodes")}} th {{i18n.t("operations.tasks")}} th {{i18n.t("operations.actions")}}}}
                    tbody {for row in &value.items {tr {key:"{row.tenant_id}",
                        td {"{row.name}" p {class:"text-secondary","{row.tenant_id}"}}
                        td {"{row.status}"} td {"{row.active_members} / {row.active_admins}"}
                        td {"{row.enabled_accounts} / {row.provider_accounts}"}
                        td {"{row.online_nodes} / {row.excluded_nodes}"}
                        td {"{row.queued_tasks} / {row.leased_tasks}"}
                        td {button {class:"btn btn-secondary",onclick:{let id=row.tenant_id;move |_|selected.set(Some(id))},{i18n.t("operations.detail")}}}
                    }}}
                }}
                if value.items.is_empty(){p {{i18n.t("tenant.empty")}}}
                div {class:"table-pagination-footer",
                    button {class:"btn btn-secondary",disabled:filter().offset==0,onclick:move |_|{filter.write().offset=filter().offset.saturating_sub(SIZE);selected.set(None);},{i18n.t("tenant.previous")}}
                    span {"{filter().offset / SIZE + 1} · {value.total} " {i18n.t("tenant.records")}}
                    button {class:"btn btn-secondary",disabled:i64::from(filter().offset)+i64::from(SIZE)>=value.total || filter().offset>=1_000_000,onclick:move |_|{filter.write().offset=filter().offset.saturating_add(SIZE);selected.set(None);},{i18n.t("tenant.next")}}
                }
            },
        }
        if let Some(id)=selected(){HealthDetail {scope,id,on_close:move |_|selected.set(None)}}
    }}
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
    rsx! {section {class:"card operations-detail",aria_label:i18n.t("operations.detail"),
        h2 {{i18n.t("operations.detail")}}
        p {"{id}"}
        match loaded {
            None=>rsx!{p {role:"status",{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{p {role:"alert",class:"alert alert-error",{user_error_message(&e)}}},
            Some(Ok(row))=>rsx!{p {"{row.name} · {row.slug} · {row.status}"}
                p {"RPM: {row.default_rpm_limit} · TPM: {row.default_tpm_limit}"}
                p {{i18n.t("operations.suspended")} " {row.suspended_members}"}},
        }
        button {class:"btn btn-secondary",onclick:move |_|on_close.call(()),{i18n.t("operations.close")}}
    }}
}
