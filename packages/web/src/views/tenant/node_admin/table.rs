use super::super::common::{MemberIdField, Pager};
use super::{
    NodeAdminScope,
    command::CommandDialog,
    types::{Filter, Kind, Pending, Row},
};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::user_error_message,
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::{
        resource::{KeyedResourceValue, current_keyed_value},
        time::format_time,
    },
};
use client_api::ClientError;
use dioxus::prelude::*;
use ui::{Badge, BadgeVariant};

fn status_label(i18n: crate::i18n::I18n, kind: Kind, status: &str) -> String {
    kind.status_key(status)
        .map(|key| i18n.t(key))
        .unwrap_or(status)
        .to_string()
}

fn status_variant(kind: Kind, status: &str) -> BadgeVariant {
    match (kind, status) {
        (Kind::Nodes, "online") | (Kind::Tasks, "succeeded") => BadgeVariant::Success,
        (Kind::Nodes, "offline") | (Kind::Tasks, "leased") | (Kind::Registrations, "pending") => {
            BadgeVariant::Warning
        }
        (Kind::Nodes, "excluded")
        | (Kind::Tasks, "failed" | "expired")
        | (Kind::Registrations, "rejected") => BadgeVariant::Error,
        (Kind::Registrations, "approved") => BadgeVariant::Success,
        (Kind::Registrations, "consumed") => BadgeVariant::Info,
        _ => BadgeVariant::Neutral,
    }
}

#[component]
fn ResourceStatus(kind: Kind, status: String) -> Element {
    let i18n = use_i18n();
    let label = status_label(i18n, kind, &status);
    rsx! { Badge { variant: status_variant(kind, &status), "{label}" } }
}

#[derive(Clone)]
struct Rows {
    items: Vec<Row>,
    total: i64,
    total_pages: i64,
}
#[component]
pub(super) fn ResourceTable(scope: NodeAdminScope, kind: Kind) -> Element {
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
            let result = scope
                .read(auth, users, move |token, api| {
                    let filter = filter.clone();
                    async move {
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
                            r.tenant() != scope.tenant_id() || r.owner().is_nil() || r.id().is_nil()
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
        div {class:"card filter-panel tenant-node-filters",
            div {class:"filter-grid filter-grid-nodes",
                if let NodeAdminScope::Tenant(tenant_scope)=scope {
                    MemberIdField {scope:tenant_scope,input_id:"node-owner".to_string(),label:i18n.t("tenant_nodes.owner").to_string(),value:draft().owner,on_input:move |value|draft.write().owner=value}
                } else {
                    div {class:"form-field",
                        label {class:"form-label",r#for:"node-owner",{i18n.t("tenant_nodes.owner")}}
                        input {id:"node-owner",class:"input-field",value:"{draft().owner}",maxlength:"36",oninput:move |e|draft.write().owner=e.value()}
                    }
                }
                div {class:"form-field filter-field-grow",
                    label {class:"form-label",r#for:"node-search",{i18n.t("tenant_nodes.search")}}
                    input {id:"node-search",class:"input-field",r#type:"search",value:"{draft().search}",maxlength:"128",oninput:move |e|draft.write().search=e.value()}
                }
                div {class:"form-field",
                    label {class:"form-label",r#for:"node-status",{i18n.t("operations.status")}}
                    select {id:"node-status",class:"input-field",value:"{draft().status}",onchange:move |e|draft.write().status=e.value(),
                        option {value:"",{i18n.t("operations.all_status")}}
                        for value in kind.statuses(){option {value:"{value}",{status_label(i18n,kind,value)}}}
                    }
                }
                if kind==Kind::Tasks{label {class:"checkbox-field",input {id:"node-archived",r#type:"checkbox",checked:draft().archived,onchange:move |e|draft.write().archived=e.checked()} span {{i18n.t("tenant_nodes.archived_only")}}}}
                div {class:"filter-actions",
                    button {class:"btn btn-primary",r#type:"button",onclick:move |_|{let mut next=draft();next.page=1;if next.query(kind).is_err(){error.set(i18n.t("tenant_nodes.invalid_query").into());return;}
                        details.set(None);error.set(String::new());if filter()==next{data.restart();}else{filter.set(next);}
                    },{i18n.t("operations.apply")}}
                    button {class:"btn btn-secondary",r#type:"button",onclick:move |_|{details.set(None);data.restart();},{i18n.t("tenant.reload")}}
                }
            }
        }
        if !error().is_empty(){p {class:"alert alert-error",role:"alert","{error}"}}
        if !message().is_empty(){p {class:"alert alert-info",role:"status","{message}"}}
        match loaded{
            None=>rsx!{div {class:"content-loading",role:"status",span {class:"spinner",aria_hidden:"true"} span {{i18n.t("common.loading")}}}},
            Some(Err(e))=>rsx!{p {class:"alert alert-error",role:"alert",{user_error_message(i18n, &e)}}},
            Some(Ok(value))=>rsx!{
                div {class:"table-pagination-panel tenant-node-table",table {class:"table",thead {tr {th {{i18n.t("tenant_nodes.resource")}} th {{i18n.t("tenant_nodes.owner_short")}} th {{i18n.t("operations.status")}} th {{i18n.t("tenant.actions")}}}}
                    tbody {for row in &value.items {tr {key:"{row.id()}",
                        td {span {"{row.title()}"} p {class:"text-secondary","{row.id()}"}}
                        td {"{row.owner()}"} td {ResourceStatus {kind,status:row.status().to_string()}
                            if let Row::Task(r)=&row {if r.cancellation_requested_at.is_some(){p {{i18n.t("tenant_nodes.cancel_pending")}}} if r.archived_at.is_some(){p {{i18n.t("tenant_nodes.archived")}}}}
                        }
                        td {div {class:"toolbar",
                            button {class:"btn btn-secondary btn-sm",onclick:{let row=row.clone();move |_|details.set(Some(row.clone()))},{i18n.t("tenant_nodes.details")}}
                            for action in row.actions(){button {class:"btn btn-secondary btn-sm",onclick:{let row=row.clone();move |_|{message.set(String::new());pending.set(Some(Pending{row:row.clone(),action}));}},{i18n.t(action.label())}}}
                        }}
                    }}}
                }}
                if value.items.is_empty(){div {class:"empty-state bordered-empty-state",h3 {class:"empty-title",{i18n.t("tenant.empty")}} p {class:"empty-description",{i18n.t("tenant_nodes.empty_hint")}}}}
                Pager {page:filter().page,total_pages:value.total_pages,total:value.total,on_page:move |page|{filter.write().page=page;details.set(None);}}
            },
        }
        if let Some(row)=details(){section {class:"card tenant-node-detail",
            h2 {{i18n.t("tenant_nodes.details")}} p {"{row.id()}"} p {{format_time(row.version())}}
            match row{
                Row::Node(r)=>rsx!{p {"{r.display_name} · " ResourceStatus {kind:Kind::Nodes,status:r.status.clone()}} p {{i18n.t("tenant_nodes.failures")} " {r.consecutive_failure_count} / {r.failure_threshold}"} p {{i18n.t("tenant_nodes.heartbeat")} " " {r.last_heartbeat_at.as_deref().map(format_time).unwrap_or_else(||"—".into())}}},
                Row::Task(r)=>rsx!{p {{i18n.t("tenant_nodes.request_id")} " {r.request_id}"} p {{i18n.t("tenant_nodes.assigned")} " " {r.assigned_node_id.map(|v|v.to_string()).unwrap_or_else(||"—".into())}} p {{i18n.t("tenant_nodes.deadline")} " {format_time(&r.deadline_at)}"}},
                Row::Registration(r)=>rsx!{p {{i18n.t("tenant_nodes.preview")} " {r.token_preview}"} p {{i18n.t("tenant_nodes.claimed")} " " {i18n.t(if r.is_revealed{"common.yes"}else{"common.no"})}} p {{i18n.t("tenant_nodes.consumed")} " " {r.consumed_at.as_deref().map(format_time).unwrap_or_else(||"—".into())}}},
            }
            button {class:"btn btn-secondary",onclick:move |_|details.set(None),{i18n.t("operations.close")}}
        }}
        if let Some(command)=pending(){for key in [format!("{}:{}:{:?}",command.row.id(),command.row.version(),command.action)]{
            CommandDialog {key:"{key}",scope,pending:command.clone(),on_close:move |_|pending.set(None),on_success:move |result:String|{pending.set(None);details.set(None);message.set(result);data.restart();}}
        }}
    }}
}
