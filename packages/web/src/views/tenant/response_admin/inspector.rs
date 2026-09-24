use super::{
    super::common::{self, WorkspaceScope},
    types::{Inspection, Mutation, ReadKind},
};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::api::response_control::{ItemPage, ItemQuery, ResponseControlApi};
use dioxus::prelude::*;
use serde_json::Value;
#[derive(Clone)]
enum Content {
    Detail(Value),
    Items(ItemPage),
}
#[component]
pub(super) fn Inspector(
    scope: WorkspaceScope,
    view: Inspection,
    on_close: EventHandler<()>,
    on_mutation: EventHandler<Mutation>,
) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut cursors = use_signal(|| vec![None::<String>]);
    let current = view.clone();
    let mut data = use_resource(move || {
        let view = current.clone();
        let after = cursors().last().cloned().flatten();
        let key = (scope, view.clone(), after.clone());
        async move {
            let result = common::read(auth, users, scope, move |token| {
                let view = view.clone();
                let after = after.clone();
                async move {
                    let api = ResponseControlApi::tenant(&get_client(), scope.tenant_id)?;
                    let address = view.row.address()?;
                    match (view.row.is_conversation(), view.read) {
                        (false, ReadKind::Detail) => {
                            let detail = api
                                .response(address.mode, address.owner, &address.id, None, &token)
                                .await?;
                            Ok(Content::Detail(detail.response.unwrap_or(Value::Null)))
                        }
                        (true, ReadKind::Detail) => {
                            let detail = api
                                .conversation(
                                    address.mode,
                                    address.owner,
                                    &address.id,
                                    None,
                                    &token,
                                )
                                .await?;
                            Ok(Content::Detail(detail.conversation))
                        }
                        (false, ReadKind::Items) => api
                            .response_input_items_page(
                                &address,
                                &ItemQuery {
                                    after,
                                    ..Default::default()
                                },
                                None,
                                &token,
                            )
                            .await
                            .map(Content::Items),
                        (true, ReadKind::Items) => api
                            .conversation_items_page(
                                &address,
                                &ItemQuery {
                                    after,
                                    ..Default::default()
                                },
                                None,
                                &token,
                            )
                            .await
                            .map(Content::Items),
                    }
                }
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let key = (scope, view.clone(), cursors().last().cloned().flatten());
    let loaded = current_keyed_value(&key, data.state().cloned(), data());
    let id = view.row.id().to_owned();
    let row = view.row.clone();
    let conversation = view.row.is_conversation();
    let owner = view.row.owner();
    let family = view.row.identity().mode;
    rsx! {div{class:"modal-overlay",div{class:"modal resource-inspector",style:"width:min(950px,95vw);max-height:85vh;overflow:auto;box-sizing:border-box;overflow-wrap:anywhere;background:var(--bg-primary,#fff);animation:none;opacity:1",role:"dialog",aria_modal:"true",aria_label:i18n.t("tenant_responses.inspect"),tabindex:"-1",onkeydown:move|e|{if e.key()==Key::Escape{e.stop_propagation();on_close.call(());}},
        h2{{i18n.t(if view.read==ReadKind::Detail{"tenant_responses.inspect"}else{"tenant_responses.items"})}}
        p{code{"{id}"}}p{{i18n.t("tenant_responses.owner")} " {owner}"}p{{i18n.t("tenant_responses.mode")} " {family}"}p{class:"text-secondary",{i18n.t("tenant_responses.private_hint")}}
        match loaded{
            None=>rsx!{p{role:"status",{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{p{class:"alert alert-error",role:"alert",{user_error_message(&e)}}},
            Some(Ok(Content::Detail(body)))=>rsx!{pre{class:"resource-content",style:"white-space:pre-wrap;overflow-wrap:anywhere",{serde_json::to_string_pretty(&body).unwrap_or_default()}}},
            Some(Ok(Content::Items(page)))=>rsx!{
                for item in &page.data{{let id=item["id"].as_str().unwrap_or_default().to_owned();let row=row.clone();rsx!{section{class:"card",key:"{id}",pre{class:"resource-content",style:"white-space:pre-wrap;overflow-wrap:anywhere",{serde_json::to_string_pretty(item).unwrap_or_default()}}
                    if conversation{button{class:"btn btn-danger btn-sm",onclick:move |_|on_mutation.call(Mutation::RemoveItem(row.clone(),id.clone())),{i18n.t("tenant_responses.remove_item")}}}
                }}}}
                if page.data.is_empty(){p{{i18n.t("tenant.empty")}}}
                div{class:"toolbar",
                    button{class:"btn btn-secondary",disabled:cursors().len()<=1,onclick:move |_|{cursors.write().pop();},{i18n.t("tenant.previous")}}
                    button{class:"btn btn-secondary",disabled:!page.has_more||page.last_id.is_none(),onclick:move |_|{cursors.write().push(page.last_id.clone());},{i18n.t("tenant.next")}}
                }
            }
        }
        div{class:"modal-actions",button{class:"btn btn-secondary",onclick:move |_|data.restart(),{i18n.t("tenant.reload")}}button{class:"btn btn-primary",onmounted:move|e|async move{let _=e.set_focus(true).await;},onclick:move |_|on_close.call(()),{i18n.t("common.close")}}}
    }}}
}
