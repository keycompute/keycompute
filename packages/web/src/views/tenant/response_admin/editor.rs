use super::{
    super::common::{self, WorkspaceScope},
    types::{self, Mutation},
};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, ui_store::UiStore, user_store::UserStore},
};
use client_api::api::response_control::{
    AppendItemsCommand, MetadataCommand, ResponseControlApi, RevisionCommand,
};
use dioxus::prelude::*;
#[component]
pub(super) fn MutationEditor(
    scope: WorkspaceScope,
    op: Mutation,
    on_close: EventHandler<()>,
    on_changed: EventHandler<()>,
) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut ui = use_context::<UiStore>();
    let i18n = use_i18n();
    let initial = op.initial();
    let mut text = use_signal(move || initial);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let label = op.label();
    let editing = matches!(op, Mutation::Metadata(_) | Mutation::Append(_));
    let id = op.row().id().to_owned();
    let owner = op.row().owner();
    let revision = op.row().revision().ok();
    let family = op.row().identity().mode;
    let item_id = match &op {
        Mutation::RemoveItem(_, id) => Some(id.clone()),
        _ => None,
    };
    let submit = move |_| {
        if busy() {
            return;
        }
        let command = op.clone();
        let text = text();
        let validation = match &command {
            Mutation::Metadata(_) => types::metadata(&text).map(|_| ()),
            Mutation::Append(_) => types::items(&text).map(|_| ()),
            _ => Ok(()),
        }
        .and_then(|_| command.row().revision().map(|_| ()));
        if let Err(e) = validation {
            error.set(user_error_message(&e));
            return;
        }
        busy.set(true);
        error.set(String::new());
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| async move {
                let api = ResponseControlApi::tenant(&get_client(), scope.tenant_id)?;
                let row = command.row();
                let addr = row.address()?;
                let body = RevisionCommand {
                    expected_revision: row.revision()?,
                    reason: None,
                };
                match command {
                    Mutation::Cancel(_) => {
                        api.cancel_response(addr.mode, addr.owner, &addr.id, &body, &token)
                            .await?;
                    }
                    Mutation::Delete(row) => {
                        if row.is_conversation() {
                            api.delete_conversation(addr.mode, addr.owner, &addr.id, &body, &token)
                                .await?;
                        } else {
                            api.delete_response(addr.mode, addr.owner, &addr.id, &body, &token)
                                .await?;
                        }
                    }
                    Mutation::Metadata(_) => {
                        api.update_conversation(
                            addr.mode,
                            addr.owner,
                            &addr.id,
                            &MetadataCommand {
                                expected_revision: body.expected_revision,
                                metadata: types::metadata(&text)?,
                                reason: None,
                            },
                            &token,
                        )
                        .await?;
                    }
                    Mutation::Append(_) => {
                        api.append_conversation_items(
                            addr.mode,
                            addr.owner,
                            &addr.id,
                            &AppendItemsCommand {
                                expected_revision: body.expected_revision,
                                items: types::items(&text)?,
                                reason: None,
                            },
                            &token,
                        )
                        .await?;
                    }
                    Mutation::RemoveItem(_, item) => {
                        api.remove_conversation_item(
                            addr.mode, addr.owner, &addr.id, &item, &body, &token,
                        )
                        .await?;
                    }
                }
                Ok(())
            })
            .await;
            if !scope.is_current(auth, users) {
                return;
            }
            busy.set(false);
            match result {
                Ok(()) => {
                    ui.show_success(i18n.t("tenant_responses.saved"));
                    on_changed.call(());
                }
                Err(e) => error.set(user_error_message(&e)),
            }
        });
    };
    rsx! {div{class:"modal-overlay",div{class:"modal",style:"width:min(800px,95vw);max-height:85vh;overflow:auto;box-sizing:border-box;overflow-wrap:anywhere;background:var(--bg-primary,#fff);animation:none;opacity:1",role:"dialog",aria_modal:"true",aria_label:i18n.t(label),tabindex:"-1",onkeydown:move|e|{if e.key()==Key::Escape&&!busy(){e.stop_propagation();on_close.call(());}},
        h2{{i18n.t(label)}}p{code{"{id}"}}p{{i18n.t("tenant_responses.owner")} " {owner}"}p{{i18n.t("tenant_responses.revision")} " " {revision.map(|v|v.to_string()).unwrap_or_else(||"—".into())}}
        p{{i18n.t("tenant_responses.mode")} " {family}"}
        if let Some(item)=item_id{p{{i18n.t("tenant_responses.item_id")} " " code{"{item}"}}}
        if !error().is_empty(){p{class:"alert alert-error",role:"alert","{error}"}}
        if editing{label{class:"form-label",r#for:"resource-content-json",{i18n.t("tenant_responses.json")}}textarea{id:"resource-content-json",class:"input-field",rows:"10",value:"{text}",maxlength:"2097152",disabled:busy(),oninput:move|e|text.set(e.value())}p{class:"text-secondary",{i18n.t(if label=="tenant_responses.metadata"{"tenant_responses.metadata_hint"}else{"tenant_responses.items_hint"})}}}
        p{class:"alert alert-info",{i18n.t("tenant_responses.owner_preserved")}}
        p{class:"text-secondary",{i18n.t("tenant.command_hint")}}
        div{class:"modal-actions",button{class:"btn btn-secondary",onmounted:move|e|async move{let _=e.set_focus(true).await;},disabled:busy(),onclick:move |_|on_close.call(()),{i18n.t("form.cancel")}}button{class:"btn btn-primary",disabled:busy()||revision.is_none(),onclick:submit,{i18n.t("tenant.confirm")}}}
    }}}
}
