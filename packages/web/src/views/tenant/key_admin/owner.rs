use super::super::common::{self, Pager, WorkspaceLinks, WorkspaceScope};
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
use client_api::api::key_control::{ClaimedKey, IssuanceIntent, OwnerKeyIssuanceApi};
use dioxus::prelude::*;
#[derive(Clone, PartialEq)]
struct Choice {
    intent: IssuanceIntent,
    claim: bool,
}
#[component]
pub fn OwnerKeyIssuance() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    rsx! {if let Some(scope)=WorkspaceScope::from_stores(auth,users){for key in [format!("{scope:?}")]{OwnerWorkspace{key:"{key}",scope}}}
    else{div{class:"page-container",p{role:"alert",{i18n.t("tenant_keys.select_workspace")}}Link{to:Route::TenantWorkspace{},{i18n.t("tenant.workspace")}}}}}
}
#[component]
fn OwnerWorkspace(scope: WorkspaceScope) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut page = use_signal(|| 1u32);
    let mut generation = use_signal(|| 0u64);
    let mut choice = use_signal(|| None::<Choice>);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut notice = use_signal(String::new);
    let mut secret = use_signal(|| None::<ClaimedKey>);
    let mut data = use_resource(move || {
        let key = (scope, page(), generation());
        async move {
            let result = common::read(auth, users, scope, move |token| async move {
                OwnerKeyIssuanceApi::new(&get_client(), scope.tenant_id, scope.user_id)?
                    .list(key.1, 20, &token)
                    .await
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let loaded = current_keyed_value(
        &(scope, page(), generation()),
        data.state().cloned(),
        data(),
    );
    let confirm = move |_| {
        if busy() || secret.read().is_some() {
            return;
        }
        let Some(selected) = choice() else {
            return;
        };
        busy.set(true);
        error.set(String::new());
        notice.set(String::new());
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| async move {
                let api = OwnerKeyIssuanceApi::new(&get_client(), scope.tenant_id, scope.user_id)?;
                if selected.claim {
                    api.claim(&selected.intent, &token).await.map(Some)
                } else {
                    api.decline(&selected.intent, &token).await.map(|_| None)
                }
            })
            .await;
            if !scope.is_current(auth, users) {
                return;
            }
            busy.set(false);
            choice.set(None);
            match result {
                Ok(Some(key)) => {
                    secret.set(Some(key));
                    generation += 1;
                }
                Ok(None) => {
                    notice.set(i18n.t("tenant_keys.declined_result").into());
                    generation += 1;
                }
                Err(e) => error.set(user_error_message(&e)),
            }
        });
    };
    rsx! {div{class:"page-container owner-key-issuance",
        ui::PageHeader{title:i18n.t("tenant_keys.my_requests").to_string(),description:i18n.t("tenant_keys.owner_hint").to_string()}
        WorkspaceLinks{}
        p{class:"text-secondary","{scope.tenant_id} · {scope.user_id}"}
        Link{class:"btn btn-secondary",to:Route::ApiKeyList{},{i18n.t("page.api_keys")}}
        SecretPanel{scope,secret,on_close:move |_|secret.set(None)}
        if !notice().is_empty(){p{class:"alert alert-success",role:"status","{notice}"}}
        if !error().is_empty(){p{class:"alert alert-error",role:"alert","{error}"}p{class:"text-secondary",{i18n.t("tenant_keys.claim_uncertain")}}}
        button{class:"btn btn-secondary",disabled:busy(),onclick:move |_|{choice.set(None);data.restart();},{i18n.t("tenant.reload")}}
        match loaded{
            None=>rsx!{p{role:"status",{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{p{class:"alert alert-error",role:"alert",{user_error_message(&e)}}},
            Some(Ok(result))=>rsx!{
                div {class:"table-pagination-panel",
                    div {style:"overflow-x:auto",
                        table {class:"table",
                            thead {tr {
                                th {{i18n.t("tenant_keys.name")}}
                                th {{i18n.t("tenant_keys.claim_by")}}
                                th {{i18n.t("tenant_keys.expiration")}}
                                th {{i18n.t("tenant.actions")}}
                            }}
                            tbody {for intent in &result.intents {
                                PendingRow {key:"{intent.id}",intent:intent.clone(),blocked:busy()||secret.read().is_some(),on_choice:move|value|choice.set(Some(value))}
                            }}
                        }
                    }
                    if result.intents.is_empty() {p {{i18n.t("tenant.empty")}}}
                }
                Pager {page:page(),total_pages:result.total_pages,total:result.total,on_page:move|p|if !busy(){page.set(p);choice.set(None);}}
            }

        }
        if let Some(selected)=choice(){div{class:"modal-overlay",div{class:"modal",style:"width:min(800px,95vw);max-height:85vh;overflow:auto;box-sizing:border-box;overflow-wrap:anywhere;background:var(--bg-primary,#fff);animation:none;opacity:1",tabindex:"-1",onkeydown:move|e|{if e.key()==Key::Escape&&!busy(){e.stop_propagation();choice.set(None);}},role:"dialog",aria_modal:"true",aria_label:i18n.t(if selected.claim{"tenant_keys.claim"}else{"tenant_keys.decline"}),
            h2{{i18n.t(if selected.claim{"tenant_keys.claim"}else{"tenant_keys.decline"})}}
            p{"{selected.intent.requested_name}"}p{code{"{selected.intent.id}"}}p{{i18n.t("tenant_keys.owner_hint")}}
            if selected.intent.replaces_key_id.is_some(){p{class:"alert alert-info",{i18n.t("tenant_keys.rotation_hint")}}}
            p{class:"text-secondary",{i18n.t("tenant.command_hint")}}
            div{class:"modal-actions",
                button{class:"btn btn-secondary",onmounted:move|e|async move{let _=e.set_focus(true).await;},disabled:busy(),onclick:move |_|choice.set(None),{i18n.t("form.cancel")}}
                button{class:"btn btn-primary",disabled:busy(),onclick:confirm,{i18n.t("tenant.confirm")}}
            }
        }}}
    }}
}
#[component]
fn SecretPanel(
    scope: WorkspaceScope,
    secret: Signal<Option<ClaimedKey>>,
    on_close: EventHandler<()>,
) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut copied = use_signal(|| false);
    let mut copying = use_signal(|| false);
    let mut copy_failed = use_signal(|| false);
    use_effect(move || {
        let _ = secret.read().as_ref().map(|k| k.key_id);
        copied.set(false);
        copy_failed.set(false);
        copying.set(false);
    });
    let copy = move |_| {
        if copying() || !scope.is_current(auth, users) {
            return;
        }
        let Some((id, text)) = secret
            .peek()
            .as_ref()
            .map(|k| (k.key_id, k.key.expose().to_owned()))
        else {
            return;
        };
        copying.set(true);
        copied.set(false);
        copy_failed.set(false);
        spawn(async move {
            let ok = copy_secret(&text).await;
            if !scope.is_current(auth, users)
                || secret.peek().as_ref().is_none_or(|k| k.key_id != id)
            {
                return;
            }
            copying.set(false);
            copied.set(ok);
            copy_failed.set(!ok);
        });
    };
    let held = secret.read();
    let Some(key) = held.as_ref() else {
        return rsx! {};
    };
    rsx! {section{class:"kc-api-success-panel owner-issued-secret",
        h2{{i18n.t("tenant_keys.secret_title")}}p{{i18n.t("tenant_keys.secret_hint")}}p{code{"{key.key_id}"}}
        pre{class:"kc-api-example",style:"white-space:pre-wrap;overflow-wrap:anywhere","{key.key.expose()}"}
        button{class:"btn btn-secondary",disabled:copying(),onclick:copy,{i18n.t("tenant_keys.copy")}}
        button{class:"btn btn-secondary",onclick:move |_|on_close.call(()),{i18n.t("tenant_keys.clear_secret")}}
        if copied(){p{role:"status",{i18n.t("tenant_keys.copied")}}}
        if copy_failed(){p{role:"alert",{i18n.t("tenant_keys.copy_failed")}}}
    }}
}
async fn copy_secret(text: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else {
            return false;
        };
        let clipboard = window.navigator().clipboard();
        let value: &wasm_bindgen::JsValue = clipboard.as_ref();
        if value.is_null() || value.is_undefined() {
            return false;
        }
        wasm_bindgen_futures::JsFuture::from(clipboard.write_text(text))
            .await
            .is_ok()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = text;
        false
    }
}

#[component]
fn PendingRow(intent: IssuanceIntent, blocked: bool, on_choice: EventHandler<Choice>) -> Element {
    let i18n = use_i18n();
    let claim = intent.clone();
    let decline = intent.clone();
    rsx! {tr {
        td {p {"{intent.requested_name}"} details {summary {"ID"} code {"{intent.id}"}
            p {{i18n.t("tenant_keys.requester")} " {intent.requested_by_user_id}"}
            if let Some(old)=intent.replaces_key_id {p {{i18n.t("tenant_keys.replaces")} " {old}"}}
        }}
        td {{format_time(&intent.expires_at)}}
        td {{intent.requested_expires_at.as_deref().map(format_time).unwrap_or_else(||i18n.t("tenant_keys.never").into())}}
        td {div {class:"action-buttons",style:"display:flex;flex-wrap:wrap;gap:8px",
            button {class:"btn btn-primary btn-sm",disabled:blocked,onclick:move |_|on_choice.call(Choice{intent:claim.clone(),claim:true}),{i18n.t("tenant_keys.claim")}}
            button {class:"btn btn-secondary btn-sm",disabled:blocked,onclick:move |_|on_choice.call(Choice{intent:decline.clone(),claim:false}),{i18n.t("tenant_keys.decline")}}
        }}
    }}
}
