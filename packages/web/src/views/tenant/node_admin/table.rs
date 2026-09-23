use super::super::common::{self, Pager, WorkspaceScope};
use super::{
    command::CommandDialog,
    types::{Filter, Kind, Pending, Row},
};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::{ClientError, api::node_control::NodeControlApi};
use dioxus::prelude::*;
#[derive(Clone)]
struct Rows {
    items: Vec<Row>,
    total: i64,
    total_pages: i64,
}
#[component]
pub(super) fn ResourceTable(scope: WorkspaceScope, kind: Kind) -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut draft = use_signal(Filter::initial);
    let mut filter = use_signal(Filter::initial);
    let mut pending = use_signal(|| None::<Pending>);
    let mut error = use_signal(String::new);
    let mut message = use_signal(String::new);
    let mut details = use_signal(|| None::<Row>);
    let mut data = use_resource(move || {
        let filter = filter();
        let key = (scope, kind, filter.clone());
        async move {
            let result = common::read(auth, users, scope, move |token| {
                let filter = filter.clone();
                async move {
                    let api = NodeControlApi::tenant(&get_client(), scope.tenant_id)?;
                    let q = filter.query(kind)?;
                    let rows = match kind {
                        Kind::Nodes => {
                            let p = api.nodes(&q, &token).await?;
                            Rows {
                                items: p.items.into_iter().map(Row::Node).collect(),
                                total: p.total,
                                total_pages: p.total_pages,
                            }
                        }
                        Kind::Tasks => {
                            let p = api.tasks(&q, &token).await?;
                            Rows {
                                items: p.items.into_iter().map(Row::Task).collect(),
                                total: p.total,
                                total_pages: p.total_pages,
                            }
                        }
                        Kind::Registrations => {
                            let p = api.registrations(&q, &token).await?;
                            Rows {
                                items: p.items.into_iter().map(Row::Registration).collect(),
                                total: p.total,
                                total_pages: p.total_pages,
                            }
                        }
                    };
                    if rows.items.iter().any(|r| {
                        r.tenant() != scope.tenant_id || r.owner().is_nil() || r.id().is_nil()
                    }) {
                        return Err(ClientError::InvalidResponse(
                            "Node response does not match the selected tenant".into(),
                        ));
                    }
                    Ok(rows)
                }
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let loaded = current_keyed_value(&(scope, kind, filter()), data.state().cloned(), data());
    rsx! {section {class:"tenant-node-resources",
        div {class:"card tenant-node-filters",
            label {r#for:"node-owner",{i18n.t("tenant_nodes.owner")}}
            input {id:"node-owner",class:"input-field",value:"{draft().owner}",maxlength:"36",oninput:move |e|draft.write().owner=e.value()}
            label {r#for:"node-search",{i18n.t("tenant_nodes.search")}}
            input {id:"node-search",class:"input-field",value:"{draft().search}",maxlength:"128",oninput:move |e|draft.write().search=e.value()}
            label {r#for:"node-status",{i18n.t("operations.status")}}
            select {id:"node-status",class:"input-field",value:"{draft().status}",onchange:move |e|draft.write().status=e.value(),
                option {value:"",{i18n.t("operations.all_status")}}
                for value in kind.statuses(){option {value:"{value}","{value}"}}
            }
            if kind==Kind::Tasks{label {input {id:"node-archived",r#type:"checkbox",checked:draft().archived,onchange:move |e|draft.write().archived=e.checked()}{i18n.t("tenant_nodes.archived_only")}}}
            div {class:"toolbar",
                button {class:"btn btn-primary",onclick:move |_|{let mut next=draft();next.page=1;if next.query(kind).is_err(){error.set(i18n.t("tenant_nodes.invalid_query").into());return;}
                    details.set(None);error.set(String::new());if filter()==next{data.restart();}else{filter.set(next);}
                },{i18n.t("operations.apply")}}
                button {class:"btn btn-secondary",onclick:move |_|{details.set(None);data.restart();},{i18n.t("tenant.reload")}}
            }
        }
        if !error().is_empty(){p {class:"alert alert-error",role:"alert","{error}"}}
        if !message().is_empty(){p {class:"alert alert-info",role:"status","{message}"}}
        match loaded{
            None=>rsx!{p {role:"status",{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{p {class:"alert alert-error",role:"alert",{user_error_message(&e)}}},
            Some(Ok(value))=>rsx!{
                div {class:"table-pagination-panel tenant-node-table",table {class:"table",thead {tr {th {{i18n.t("tenant_nodes.resource")}} th {{i18n.t("tenant_nodes.owner_short")}} th {{i18n.t("operations.status")}} th {{i18n.t("tenant.actions")}}}}
                    tbody {for row in &value.items {tr {key:"{row.id()}",
                        td {span {"{row.title()}"} p {class:"text-secondary","{row.id()}"}}
                        td {"{row.owner()}"} td {"{row.status()}"
                            if let Row::Task(r)=&row {if r.cancellation_requested_at.is_some(){p {{i18n.t("tenant_nodes.cancel_pending")}}} if r.archived_at.is_some(){p {{i18n.t("tenant_nodes.archived")}}}}
                        }
                        td {div {class:"toolbar",
                            button {class:"btn btn-secondary btn-sm",onclick:{let row=row.clone();move |_|details.set(Some(row.clone()))},{i18n.t("tenant_nodes.details")}}
                            for action in row.actions(){button {class:"btn btn-secondary btn-sm",onclick:{let row=row.clone();move |_|{message.set(String::new());pending.set(Some(Pending{row:row.clone(),action}));}},{i18n.t(action.label())}}}
                        }}
                    }}}
                }}
                if value.items.is_empty(){p {{i18n.t("tenant.empty")}}}
                Pager {page:filter().page,total_pages:value.total_pages,total:value.total,on_page:move |page|{filter.write().page=page;details.set(None);}}
            },
        }
        if let Some(row)=details(){section {class:"card tenant-node-detail",
            h2 {{i18n.t("tenant_nodes.details")}} p {"{row.id()}"} p {"{row.version()}"}
            match row{
                Row::Node(r)=>rsx!{p {"{r.display_name} · {r.status}"} p {{i18n.t("tenant_nodes.failures")} " {r.consecutive_failure_count} / {r.failure_threshold}"} p {{i18n.t("tenant_nodes.heartbeat")} " " {r.last_heartbeat_at.as_deref().unwrap_or("—")}}},
                Row::Task(r)=>rsx!{p {"Request ID: {r.request_id}"} p {{i18n.t("tenant_nodes.assigned")} " " {r.assigned_node_id.map(|v|v.to_string()).unwrap_or_else(||"—".into())}} p {{i18n.t("tenant_nodes.deadline")} " {r.deadline_at}"}},
                Row::Registration(r)=>rsx!{p {{i18n.t("tenant_nodes.preview")} " {r.token_preview}"} p {{i18n.t("tenant_nodes.claimed")} " {r.is_revealed}"} p {{i18n.t("tenant_nodes.consumed")} " " {r.consumed_at.as_deref().unwrap_or("—")}}},
            }
            button {class:"btn btn-secondary",onclick:move |_|details.set(None),{i18n.t("operations.close")}}
        }}
        if let Some(command)=pending(){for key in [format!("{}:{}:{:?}",command.row.id(),command.row.version(),command.action)]{
            CommandDialog {key:"{key}",scope,pending:command.clone(),on_close:move |_|pending.set(None),on_success:move |result:String|{pending.set(None);details.set(None);message.set(result);data.restart();}}
        }}
    }}
}
