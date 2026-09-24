use super::{
    super::common::{self, WorkspaceScope},
    types::Detail,
};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::{ClientError, api::tenant_reporting::TenantReportingApi};
use dioxus::prelude::*;
#[component]
pub(super) fn Inspector(
    scope: WorkspaceScope,
    detail: Detail,
    on_close: EventHandler<()>,
) -> Element {
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
                    let api = TenantReportingApi::new(&get_client(), scope.tenant_id)?;
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
    rsx! {div{class:"modal-overlay",div{class:"modal finance-inspector",style:"width:min(950px,95vw);max-height:85vh;overflow:auto;box-sizing:border-box;overflow-wrap:anywhere;background:var(--bg-primary,#fff);animation:none;opacity:1",role:"dialog",aria_modal:"true",aria_label:i18n.t("tenant_finance.details"),tabindex:"-1",onkeydown:move|e|{if e.key()==Key::Escape{e.stop_propagation();on_close.call(());}},
        h2{{i18n.t("tenant_finance.details")}}p{code{"{detail.id()}"}}p{{i18n.t("tenant_finance.owner")} " {detail.owner()}"}
        p{class:"text-secondary",{i18n.t("tenant_finance.safe_fields")}}
        match loaded{None=>rsx!{p{role:"status",{i18n.t("common.loading")}}},Some(Err(e))=>rsx!{p{role:"alert",class:"alert alert-error",{user_error_message(&e)}}},Some(Ok(value))=>rsx!{pre{style:"white-space:pre-wrap;overflow-wrap:anywhere","{value}"}}}
        div{class:"modal-actions",button{class:"btn btn-secondary",onclick:move |_|data.restart(),{i18n.t("tenant.reload")}}button{class:"btn btn-primary",onmounted:move|e|async move{let _=e.set_focus(true).await;},onclick:move |_|on_close.call(()),{i18n.t("common.close")}}}
    }}}
}
