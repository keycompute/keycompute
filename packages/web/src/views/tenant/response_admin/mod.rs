//! Current-tenant administration of local passthrough and node Responses/Conversations.
mod editor;
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
use client_api::api::response_control::ResponseControlApi;
use dioxus::prelude::*;
use types::{Inspection, Kind, Mutation, Query, ReadKind, Row};
#[derive(Clone)]
struct Rows {
    rows: Vec<Row>,
    total: i64,
    total_pages: i64,
}
#[component]
pub fn TenantResponses() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let allowed = users
        .info
        .read()
        .as_ref()
        .is_some_and(|u| u.can_manage_tenant());
    rsx! {if let Some(scope)=WorkspaceScope::from_stores(auth,users).filter(|_|allowed){for key in [format!("{scope:?}")]{ResourceWorkspace{key:"{key}",scope}}}else{p{role:"alert",{i18n.t("tenant.admin_required")}}}}
}
#[component]
fn ResourceWorkspace(scope: WorkspaceScope) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut query = use_signal(Query::default);
    let mut owner = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut inspection = use_signal(|| None::<Inspection>);
    let mut mutation = use_signal(|| None::<Mutation>);
    let mut generation = use_signal(|| 0u64);
    let mut data = use_resource(move || {
        let q = query();
        let key = (scope, q.clone(), generation());
        async move {
            let result = common::read(auth, users, scope, move |token| {
                let q = q.clone();
                async move {
                    let api = ResponseControlApi::tenant(&get_client(), scope.tenant_id)?;
                    match q.kind {
                        Kind::Responses => api.responses(&q.api(), &token).await.map(|p| Rows {
                            rows: p.items.into_iter().map(Row::Response).collect(),
                            total: p.total,
                            total_pages: p.total_pages,
                        }),
                        Kind::Conversations => {
                            api.conversations(&q.api(), &token).await.map(|p| Rows {
                                rows: p.items.into_iter().map(Row::Conversation).collect(),
                                total: p.total,
                                total_pages: p.total_pages,
                            })
                        }
                    }
                }
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let loaded = current_keyed_value(
        &(scope, query(), generation()),
        data.state().cloned(),
        data(),
    );
    let mut inspect = move |value| inspection.set(Some(value));
    let mut mutate = move |value| mutation.set(Some(value));
    rsx! {div{class:"page-container tenant-response-admin",
        ui::PageHeader{title:i18n.t("tenant_responses.title").to_string(),description:i18n.t("tenant_responses.hint").to_string()}
        WorkspaceLinks{}
        p{class:"alert alert-info",{i18n.t("tenant_responses.local_only")} " · {scope.tenant_id}"}
        div{class:"toolbar",
            button{class:"btn btn-secondary",onclick:move |_|{query.write().kind=Kind::Responses;query.write().page=1;inspection.set(None);mutation.set(None);},{i18n.t("tenant_responses.responses")}}
            button{class:"btn btn-secondary",onclick:move |_|{query.write().kind=Kind::Conversations;query.write().page=1;inspection.set(None);mutation.set(None);},{i18n.t("tenant_responses.conversations")}}
            label{r#for:"managed-resource-mode",{i18n.t("tenant_responses.mode")}}
            select{id:"managed-resource-mode",class:"input-field",value:query().mode.as_str(),onchange:move |e|{if let Some(value)=types::mode(&e.value()){query.write().mode=value;query.write().page=1;inspection.set(None);mutation.set(None);}},option{value:"passthrough","Passthrough"}option{value:"node_dispatch","Node dispatch"}}
            label{r#for:"managed-resource-owner",{i18n.t("tenant_responses.owner")}}
            input{id:"managed-resource-owner",class:"input-field",value:"{owner}",maxlength:"36",oninput:move|e|owner.set(e.value())}
            button{class:"btn btn-secondary",onclick:move |_|match types::owner(&owner()){Ok(owner)=>{query.write().owner=owner;query.write().page=1;error.set(String::new());inspection.set(None);mutation.set(None);},Err(e)=>error.set(user_error_message(&e))},{i18n.t("tenant_responses.filter")}}
            button{class:"btn btn-secondary",onclick:move |_|{inspection.set(None);mutation.set(None);data.restart();},{i18n.t("tenant.reload")}}
        }
        p {class:"text-secondary",{i18n.t("tenant_responses.active_owner")} " " {query().owner.map(|v|v.to_string()).unwrap_or_else(||i18n.t("tenant_responses.all_owners").into())}}
        if !error().is_empty(){p{class:"alert alert-error",role:"alert","{error}"}}
        match loaded{
            None=>rsx!{p{role:"status",{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{p{class:"alert alert-error",role:"alert",{user_error_message(&e)}}},
            Some(Ok(page))=>rsx!{
                div{class:"table-pagination-panel",div{class:"table-container",style:"overflow-x:auto",table{class:"table",
                    thead{tr{th{{i18n.t("tenant_responses.resource")}}th{{i18n.t("tenant_responses.owner")}}th{{i18n.t("tenant_responses.state")}}th{{i18n.t("tenant_responses.created")}}th{{i18n.t("tenant.actions")}}}}
                    tbody{for row in &page.rows{{let detail=row.clone();let items=row.clone();let cancel=row.clone();let delete=row.clone();let meta=row.clone();let append=row.clone();
                        rsx!{tr{key:"{row.key()}",td{code{"{row.id()}"}p{"{row.model()}"}p{{i18n.t("tenant_responses.revision")} " " {row.revision().map(|v|v.to_string()).unwrap_or_else(|_|"—".into())}}}
                        td{code{"{row.owner()}"}}td{p{"{row.status()}"}if let Row::Conversation(c)=row{if let Some(active)=&c.active_response_id{details{summary{{i18n.t("tenant_responses.active")}}code{"{active}"}}}}}
                        td{p{{format_time(row.created())}}details{summary{{i18n.t("tenant_responses.expires")}}{format_time(row.expires())}}}
                        td{
                            button{class:"btn btn-secondary btn-sm",onclick:move |_|inspect(Inspection{row:detail.clone(),read:ReadKind::Detail}),{i18n.t("tenant_responses.inspect")}}
                            button{class:"btn btn-secondary btn-sm",onclick:move |_|inspect(Inspection{row:items.clone(),read:ReadKind::Items}),{i18n.t("tenant_responses.items")}}
                            if row.can_cancel(){button{class:"btn btn-secondary btn-sm",onclick:move |_|mutate(Mutation::Cancel(cancel.clone())),{i18n.t("tenant_responses.cancel")}}}
                            if row.is_conversation(){button{class:"btn btn-secondary btn-sm",onclick:move |_|mutate(Mutation::Metadata(meta.clone())),{i18n.t("tenant_responses.metadata")}}button{class:"btn btn-secondary btn-sm",onclick:move |_|mutate(Mutation::Append(append.clone())),{i18n.t("tenant_responses.append")}}}
                            button{class:"btn btn-danger btn-sm",onclick:move |_|mutate(Mutation::Delete(delete.clone())),{i18n.t("tenant_responses.delete")}}
                        }
                    }}}}}
                }}if page.rows.is_empty(){p{{i18n.t("tenant.empty")}}}}
                Pager{page:query().page as u32,total_pages:page.total_pages,total:page.total,on_page:move |p|{query.write().page=i64::from(p);inspection.set(None);mutation.set(None);}}
            }
        }
        if let Some(view)=inspection(){if mutation().is_none(){for key in [view.key()]{inspector::Inspector{key:"{key}",scope,view:view.clone(),on_close:move |_|inspection.set(None),on_mutation:mutate}}}}
        if let Some(op)=mutation(){for key in [op.key()]{editor::MutationEditor{key:"{key}",scope,op:op.clone(),on_close:move |_|mutation.set(None),on_changed:move |_|{mutation.set(None);inspection.set(None);generation+=1;}}}}
    }}
}
