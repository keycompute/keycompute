use super::super::common::{
    self, MemberIdField, Pager, TechnicalId, WorkspaceContext, WorkspaceLinks, WorkspaceScope,
};
use super::{
    editor::Editor,
    types::{self, Operation, Query, Tab},
};
use crate::{
    hooks::use_i18n::use_i18n,
    router::Route,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::{
        resource::{KeyedResourceValue, current_keyed_value},
        time::format_time,
    },
};
use client_api::api::key_control::{IssuancePage, KeyPage, KeyQuery, TenantKeyApi};
use dioxus::prelude::*;
#[derive(Clone)]
enum Rows {
    Keys(KeyPage),
    Pending(IssuancePage),
}
impl Rows {
    fn total(&self) -> i64 {
        match self {
            Self::Keys(p) => p.total,
            Self::Pending(p) => p.total,
        }
    }
    fn pages(&self) -> i64 {
        match self {
            Self::Keys(p) => p.total_pages,
            Self::Pending(p) => p.total_pages,
        }
    }
}
#[component]
pub fn TenantKeys() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let allowed = users
        .info
        .read()
        .as_ref()
        .is_some_and(|u| u.can_manage_api_keys());
    rsx! {if let Some(scope)=WorkspaceScope::from_stores(auth,users).filter(|_|allowed){for key in [format!("{scope:?}")]{KeyWorkspace{key:"{key}",scope}}}else{p{role:"alert",{i18n.t("tenant.admin_required")}}}}
}
#[component]
fn KeyWorkspace(scope: WorkspaceScope) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut query = use_signal(Query::default);
    let mut owner = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut operation = use_signal(|| None::<Operation>);
    let mut generation = use_signal(|| 0u64);
    let mut data = use_resource(move || {
        let q = query();
        let key = (scope, q.clone(), generation());
        async move {
            let result = common::read(auth, users, scope, move |token| {
                let q = q.clone();
                async move {
                    let api = TenantKeyApi::new(&get_client(), scope.tenant_id)?;
                    match q.tab {
                        Tab::Keys => api
                            .list(
                                &KeyQuery {
                                    owner_user_id: q.owner,
                                    include_revoked: q.revoked,
                                    page: q.page,
                                    page_size: 20,
                                },
                                &token,
                            )
                            .await
                            .map(Rows::Keys),
                        Tab::Pending => api
                            .issuances(q.page, 20, q.owner, &token)
                            .await
                            .map(Rows::Pending),
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
    rsx! {div{class:"page-container tenant-key-admin",
        ui::PageHeader{title:i18n.t("tenant_keys.title").to_string(),description:i18n.t("tenant_keys.hint").to_string()}
        WorkspaceLinks{}
        WorkspaceContext{note:i18n.t("tenant_keys.metadata_only").to_string()}
        div{class:"tenant-key-actions",
            nav{class:"segmented-control",aria_label:i18n.t("tenant_keys.title"),
                button{class:"btn btn-secondary",aria_pressed:query().tab==Tab::Keys,onclick:move |_|{query.write().tab=Tab::Keys;query.write().page=1;operation.set(None);},{i18n.t("tenant_keys.keys")}}
                button{class:"btn btn-secondary",aria_pressed:query().tab==Tab::Pending,onclick:move |_|{query.write().tab=Tab::Pending;query.write().page=1;operation.set(None);},{i18n.t("tenant_keys.pending")}}
            }
            Link{class:"btn btn-ghost",to:Route::OwnerKeyIssuance{},{i18n.t("tenant_keys.my_requests")}}
            button{class:"btn btn-primary",onclick:move |_|operation.set(Some(Operation::Request)),{i18n.t("tenant_keys.request")}}
        }
        section{class:"filter-panel",aria_label:i18n.t("tenant_keys.filter"),
            div{class:"filter-grid tenant-key-filter-grid",
                MemberIdField{scope,input_id:"tenant-key-owner".to_string(),label:i18n.t("tenant_keys.owner").to_string(),value:owner(),on_input:move|value|owner.set(value)}
                if query().tab==Tab::Keys{label{class:"checkbox-field",input{r#type:"checkbox",checked:query().revoked,onchange:move|e|{query.write().revoked=e.checked();query.write().page=1;}}{i18n.t("tenant_keys.include_revoked")}}}
                div{class:"filter-actions",
                    button{class:"btn btn-primary",onclick:move |_|match types::owner_filter(&owner()){Ok(owner)=>{query.write().owner=owner;query.write().page=1;operation.set(None);error.set(String::new());},Err(e)=>error.set(user_error_message(i18n, &e))},{i18n.t("tenant_keys.filter")}}
                    button{class:"btn btn-ghost",onclick:move |_|{operation.set(None);data.restart();},{i18n.t("tenant.reload")}}
                }
            }
        }
        div{class:"data-meta-row",span{{i18n.t("tenant_keys.active_filter")}} span{if let Some(id)=query().owner{TechnicalId{value:id.to_string()}}else{{i18n.t("tenant_keys.all_owners")}}}}
        if !error().is_empty(){p{class:"alert alert-error",role:"alert","{error}"}}
        match loaded{
            None=>rsx!{p{role:"status",{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{p{class:"alert alert-error",role:"alert",{user_error_message(i18n, &e)}}},
            Some(Ok(rows))=>rsx!{
                div { class:"table-pagination-panel",
                    div { style:"overflow-x:auto",
                        table { class:"table",
                            thead { tr {
                                th {{i18n.t("tenant_keys.name")}}
                                th {{i18n.t("tenant_keys.owner")}}
                                th {{i18n.t("tenant_keys.state")}}
                                th {{i18n.t("tenant_keys.expiration")}}
                                th {{i18n.t("tenant.actions")}}
                            }}
                            tbody {
                                match &rows {
                                    Rows::Keys(page)=>rsx!{for row in &page.keys {
                                        KeyRow { key:"{row.id}", row:row.clone(), on_operation:move|op|operation.set(Some(op)) }
                                    }},
                                    Rows::Pending(page)=>rsx!{for row in &page.intents {
                                        IntentRow { key:"{row.id}", row:row.clone(), on_operation:move|op|operation.set(Some(op)) }
                                    }},
                                }
                            }
                        }
                    }
                    if rows.total()==0 { div{class:"empty-state compact-empty-state",h3{class:"empty-title",{i18n.t("tenant.empty")}}} }
                }
                Pager { page:query().page,total_pages:rows.pages(),total:rows.total(),on_page:move|p|{query.write().page=p;operation.set(None);} }
            }

        }
        if let Some(op)=operation(){for key in [op.key()]{Editor{key:"{key}",scope,op:op.clone(),on_close:move |_|operation.set(None),on_changed:move |_|{operation.set(None);generation+=1;}}}}
    }}
}

#[component]
fn KeyRow(
    row: client_api::api::key_control::KeyMetadata,
    on_operation: EventHandler<Operation>,
) -> Element {
    let i18n = use_i18n();
    let edit = row.clone();
    let rotate = row.clone();
    let revoke = row.clone();
    let delete = row.clone();
    rsx! { tr {
        td { p {"{row.name}"} code {"{row.key_preview}"}
            details { summary {"ID"} code {"{row.id}"} p {{i18n.t("tenant_keys.version")} " {format_time(&row.updated_at)}"} }
        }
        td {TechnicalId{value:row.owner_user_id.to_string()}}
        td {{i18n.t(if row.revoked{"tenant_keys.revoked"}else if types::live_key(&row){"tenant_keys.active"}else{"tenant_keys.expired"})}}
        td {{row.expires_at.as_deref().map(format_time).unwrap_or_else(||i18n.t("tenant_keys.never").into())}}
        td { div { class:"action-buttons",style:"display:flex;flex-wrap:wrap;gap:8px",
            button {class:"btn btn-secondary btn-sm",onclick:move |_|on_operation.call(Operation::Edit(edit.clone())),{i18n.t("tenant_keys.edit")}}
            button {class:"btn btn-secondary btn-sm",disabled:!types::live_key(&row),onclick:move |_|on_operation.call(Operation::Rotate(rotate.clone())),{i18n.t("tenant_keys.rotate")}}
            button {class:"btn btn-secondary btn-sm",disabled:row.revoked,onclick:move |_|on_operation.call(Operation::Revoke(revoke.clone())),{i18n.t("tenant_keys.revoke")}}
            button {class:"btn btn-danger btn-sm",onclick:move |_|on_operation.call(Operation::Delete(delete.clone())),{i18n.t("tenant_keys.delete")}}
        }}
    }}
}
#[component]
fn IntentRow(
    row: client_api::api::key_control::IssuanceIntent,
    on_operation: EventHandler<Operation>,
) -> Element {
    let i18n = use_i18n();
    let cancel = row.clone();
    rsx! { tr {
        td { p {"{row.requested_name}"} details { summary {"ID"} code {"{row.id}"}
            p {{i18n.t("tenant_keys.requester")} " {row.requested_by_user_id}"}
            if let Some(old)=row.replaces_key_id {p {{i18n.t("tenant_keys.replaces")} " {old}"}}
        }}
        td {TechnicalId{value:row.owner_user_id.to_string()}}
        td {{i18n.t("tenant_keys.pending")}}
        td {p {{i18n.t("tenant_keys.claim_by")} " " {format_time(&row.expires_at)}}
            p {{i18n.t("tenant_keys.expiration")} " " {row.requested_expires_at.as_deref().map(format_time).unwrap_or_else(||i18n.t("tenant_keys.never").into())}}
        }
        td {button {class:"btn btn-secondary btn-sm",onclick:move |_|on_operation.call(Operation::Cancel(cancel.clone())),{i18n.t("tenant_keys.cancel_request")}}}
    }}
}
