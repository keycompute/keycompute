use super::{
    super::common::{self, WorkspaceScope},
    types::Detail,
};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::user_error_message,
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::ClientError;
use dioxus::prelude::*;
#[component]
pub(super) fn Inspector(
    scope: WorkspaceScope,
    detail: Detail,
    on_close: EventHandler<()>,
) -> Element {
    ui::components::modal::use_dialog_focus_return(true);
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let selected = detail.clone();
    let mut data = use_resource(move || {
        let detail = selected.clone();
        let key = (scope, detail.key());
        async move {
            let result = common::read(auth, users, scope, move |token| {
                let detail = detail.clone();
                async move {
                    let api = scope.reporting_api()?;
                    let value = match detail {
                        Detail::Usage(row) => serde_json::to_value(
                            api.usage_detail(row.id, row.user_id, &token).await?,
                        ),
                        Detail::Order(row) => {
                            serde_json::to_value(api.payment(row.id, row.user_id, &token).await?)
                        }
                    }
                    .map_err(|_| ClientError::InvalidResponse("Invalid report metadata".into()))?;
                    serde_json::to_string_pretty(&value)
                        .map_err(|_| ClientError::InvalidResponse("Invalid report metadata".into()))
                }
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let loaded = current_keyed_value(&(scope, detail.key()), data.state().cloned(), data());
    rsx! {div{class:"modal-backdrop",tabindex:"-1",onclick:move |_|on_close.call(()),div{class:"modal finance-inspector",role:"dialog",aria_modal:"true",aria_label:i18n.t("tenant_finance.details"),tabindex:"-1",onclick:move|e|e.stop_propagation(),onkeydown:move|e|{if e.key()==Key::Escape{e.stop_propagation();on_close.call(());}else{ui::components::modal::contain_modal_focus(&e)}},
        div{class:"modal-header",h2{class:"modal-title",{i18n.t("tenant_finance.details")}}button{class:"modal-close btn btn-ghost btn-sm",r#type:"button",autofocus:true,aria_label:i18n.t("common.close"),onclick:move |_|on_close.call(()),"✕"}}
        div{class:"modal-body",p{code{"{detail.id()}"}}p{{i18n.t("tenant_finance.owner")} " {detail.owner()}"}
        p{class:"text-secondary",{i18n.t("tenant_finance.safe_fields")}}
        match loaded{None=>rsx!{p{role:"status",{i18n.t("common.loading")}}},Some(Err(e))=>rsx!{p{role:"alert",class:"alert alert-error",{user_error_message(i18n, &e)}}},Some(Ok(value))=>rsx!{pre{style:"white-space:pre-wrap;overflow-wrap:anywhere","{value}"}}}
        div{class:"modal-footer",button{class:"btn btn-secondary",r#type:"button",onclick:move |_|data.restart(),{i18n.t("tenant.reload")}}button{class:"btn btn-primary",r#type:"button",onmounted:move|e|async move{let _=e.set_focus(true).await;},onclick:move |_|on_close.call(()),{i18n.t("common.close")}}}
        }
    }}}
}
