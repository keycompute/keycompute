use super::{
    super::common::{self, WorkspaceScope},
    types::{Inspection, Mutation, ReadKind, Row},
};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::api::response_control::{ResponseControlApi, ResponseMode};
use dioxus::prelude::*;
use uuid::Uuid;

#[component]
pub(super) fn NativeDirect(
    scope: WorkspaceScope,
    generation: u64,
    on_inspect: EventHandler<Inspection>,
    on_mutation: EventHandler<Mutation>,
) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut owner_text = use_signal(String::new);
    let mut id_text = use_signal(String::new);
    let mut target = use_signal(|| None::<(Uuid, String)>);
    let mut error = use_signal(String::new);
    let key = (scope, target(), generation);
    let mut data = use_resource(move || {
        let target = target();
        let key = (scope, target.clone(), generation);
        async move {
            let result = if let Some((owner, id)) = target {
                common::read(auth, users, scope, move |token| {
                    let id = id.clone();
                    async move {
                        let api = ResponseControlApi::tenant(&get_client(), scope.tenant_id)?;
                        api.response(ResponseMode::AccountPool, owner, &id, None, &token)
                            .await
                            .map(|detail| Row::Response(detail.summary))
                    }
                })
                .await
                .map(Some)
            } else {
                Ok(None)
            };
            KeyedResourceValue::new(key, result)
        }
    });
    let loaded = current_keyed_value(&key, data.state().cloned(), data());
    let open = move |_| {
        let owner = owner_text()
            .trim()
            .parse::<Uuid>()
            .ok()
            .filter(|id| !id.is_nil());
        let id = id_text().trim().to_string();
        if owner.is_none() || id.is_empty() || id.len() > 2048 || id.chars().any(char::is_control) {
            error.set(i18n.t("tenant_responses.native_selector_error").into());
            return;
        }
        error.set(String::new());
        target.set(Some((owner.unwrap(), id)));
    };
    rsx! {section{class:"resource-panel native-response-direct",
        p{class:"alert alert-info",{i18n.t("tenant_responses.native_direct_hint")}}
        div{class:"toolbar",
            label{r#for:"native-response-owner",{i18n.t("tenant_responses.owner")}}
            input{id:"native-response-owner",class:"input-field",maxlength:"36",value:"{owner_text}",oninput:move|e|owner_text.set(e.value())}
            label{r#for:"native-response-id",{i18n.t("tenant_responses.resource")}}
            input{id:"native-response-id",class:"input-field",maxlength:"2048",value:"{id_text}",oninput:move|e|id_text.set(e.value())}
            button{class:"btn btn-primary",onclick:open,{i18n.t("tenant_responses.open_native")}}
            button{class:"btn btn-secondary",disabled:target().is_none(),onclick:move |_|data.restart(),{i18n.t("tenant.reload")}}
        }
        if !error().is_empty(){p{class:"alert alert-error",role:"alert","{error}"}}
        match loaded{
            None=>rsx!{p{role:"status",{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{p{class:"alert alert-error",role:"alert",{user_error_message(&e)}}},
            Some(Ok(None))=>rsx!{},
            Some(Ok(Some(row)))=>{let detail=row.clone();let items=row.clone();let cancel=row.clone();let delete=row.clone();rsx!{
                div{class:"card",p{code{"{row.id()}"}}p{{i18n.t("tenant_responses.owner")} " " code{"{row.owner()}"}}p{{i18n.t("tenant_responses.state")} " {row.status()}"}p{"{row.model()}"}
                    div{class:"toolbar",
                        button{class:"btn btn-secondary btn-sm",onclick:move |_|on_inspect.call(Inspection{row:detail.clone(),read:ReadKind::Detail}),{i18n.t("tenant_responses.inspect")}}
                        button{class:"btn btn-secondary btn-sm",onclick:move |_|on_inspect.call(Inspection{row:items.clone(),read:ReadKind::Items}),{i18n.t("tenant_responses.items")}}
                        if row.can_cancel(){button{class:"btn btn-secondary btn-sm",onclick:move |_|on_mutation.call(Mutation::Cancel(cancel.clone())),{i18n.t("tenant_responses.cancel")}}}
                        button{class:"btn btn-danger btn-sm",onclick:move |_|on_mutation.call(Mutation::Delete(delete.clone())),{i18n.t("tenant_responses.delete")}}
                    }
                }
            }}
        }
    }}
}
