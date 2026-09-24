use super::super::common::{self, Pager, WorkspaceScope};
use super::types::{Draft, Expiry, Operation};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, ui_store::UiStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::{
    ClientError, MembershipStatus, UserStatus,
    api::key_control::{IssuanceOutcome, TenantKeyApi},
};
use dioxus::prelude::*;
use uuid::Uuid;

#[component]
pub(super) fn Editor(
    scope: WorkspaceScope,
    op: Operation,
    on_close: EventHandler<()>,
    on_changed: EventHandler<()>,
) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut ui = use_context::<UiStore>();
    let i18n = use_i18n();
    let initial = op.clone();
    let mut draft = use_signal(move || Draft::for_operation(&initial));
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    let label = op.label();
    let requesting = matches!(op, Operation::Request);
    let editing = matches!(op, Operation::Edit(_));
    let input_fields = requesting || editing || matches!(op, Operation::Rotate(_));
    let original = op.clone();
    let submit = move |_| {
        if busy() {
            return;
        }
        let data = draft();
        let operation = original.clone();
        let validated = match &operation {
            Operation::Request => data.request().map(|_| ()),
            Operation::Edit(k) => data.patch(k).map(|_| ()),
            Operation::Rotate(k) => data.rotate(k).map(|_| ()),
            _ => Ok(()),
        };
        if let Err(e) = validated {
            error.set(user_error_message(&e));
            return;
        }
        busy.set(true);
        error.set(String::new());
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| async move {
                let api = TenantKeyApi::new(&get_client(), scope.tenant_id)?;
                let invalid = || {
                    ClientError::InvalidResponse(
                        "Key result did not preserve its original owner".into(),
                    )
                };
                match operation {
                    Operation::Request => {
                        let result = api.request(&data.request()?, &token).await?;
                        Ok(if result.outcome == IssuanceOutcome::AlreadyPending {
                            "tenant_keys.already_pending"
                        } else {
                            "tenant_keys.requested"
                        })
                    }
                    Operation::Rotate(key) => {
                        let result = api.rotate(key.id, &data.rotate(&key)?, &token).await?;
                        if result.intent.owner_user_id != key.owner_user_id {
                            return Err(invalid());
                        }
                        Ok(if result.outcome == IssuanceOutcome::AlreadyPending {
                            "tenant_keys.already_pending"
                        } else {
                            "tenant_keys.requested"
                        })
                    }
                    Operation::Edit(key) => {
                        let result = api.patch(key.id, &data.patch(&key)?, &token).await?;
                        if result.owner_user_id != key.owner_user_id {
                            return Err(invalid());
                        }
                        Ok("tenant.saved")
                    }
                    Operation::Revoke(key) => {
                        let result = api.revoke(key.id, &token).await?;
                        if result
                            .key
                            .as_ref()
                            .is_some_and(|r| r.owner_user_id != key.owner_user_id)
                        {
                            return Err(invalid());
                        }
                        Ok("tenant_keys.revoked_result")
                    }
                    Operation::Delete(key) => {
                        let result = api.delete(key.id, &token).await?;
                        if result
                            .key
                            .as_ref()
                            .is_some_and(|r| r.owner_user_id != key.owner_user_id)
                        {
                            return Err(invalid());
                        }
                        Ok(if result.deleted {
                            "tenant_keys.deleted_result"
                        } else {
                            "tenant_keys.retained_result"
                        })
                    }
                    Operation::Cancel(intent) => {
                        let result = api.cancel(intent.id, &token).await?;
                        if result.intent.owner_user_id != intent.owner_user_id {
                            return Err(invalid());
                        }
                        Ok("tenant_keys.cancelled_result")
                    }
                }
            })
            .await;
            if !scope.is_current(auth, users) {
                return;
            }
            busy.set(false);
            match result {
                Ok(message) => {
                    ui.show_success(i18n.t(message));
                    on_changed.call(());
                }
                Err(e) => error.set(user_error_message(&e)),
            }
        });
    };
    rsx! {div{class:"modal-overlay",div{class:"modal tenant-key-editor",style:"width:min(800px,95vw);max-height:85vh;overflow:auto;box-sizing:border-box;overflow-wrap:anywhere;background:var(--bg-primary,#fff);animation:none;opacity:1",tabindex:"-1",onkeydown:move|e|{if e.key()==Key::Escape&&!busy(){e.stop_propagation();on_close.call(());}},role:"dialog",aria_modal:"true",aria_label:i18n.t(label),
        h2{{i18n.t(label)}}p{class:"text-secondary","{scope.tenant_id}"}
        if let Some(row)=op.key_row(){p{code{"{row.id}"}}}
        if let Operation::Cancel(intent)=&op{p{code{"{intent.id}"}}}
        if !error().is_empty(){p{class:"alert alert-error",role:"alert","{error}"}}
        if requesting{
            OwnerPicker{scope,disabled:busy(),on_select:move|id:Uuid|draft.write().owner=id.to_string()}
        }
        label{class:"form-label",r#for:"key-request-owner",{i18n.t("tenant_keys.owner")}}
        input{id:"key-request-owner",class:"input-field",value:"{draft().owner}",maxlength:"36",disabled:busy()||!requesting,oninput:move|e|draft.write().owner=e.value()}
        if input_fields{
            label{class:"form-label",r#for:"key-request-name",{i18n.t("tenant_keys.name")}}
            input{id:"key-request-name",class:"input-field",value:"{draft().name}",maxlength:"255",disabled:busy(),oninput:move|e|draft.write().name=e.value()}
            label{class:"form-label",r#for:"key-expiry-mode",{i18n.t("tenant_keys.expiration")}}
            select{id:"key-expiry-mode",class:"input-field",value:draft().expiry.as_str(),disabled:busy(),onchange:move|e|if let Some(choice)=Expiry::parse(&e.value()){draft.write().expiry=choice;},
                if editing{option{value:"keep",{i18n.t("tenant_keys.keep_expiry")}}}
                option{value:"never",{i18n.t("tenant_keys.never")}}option{value:"at",{i18n.t("tenant_keys.set_expiry")}}
            }
            if draft().expiry==Expiry::At{
                label{class:"form-label",r#for:"key-expiry-time",{i18n.t("tenant_keys.expiry_time")}}
                input{id:"key-expiry-time",class:"input-field",value:"{draft().date}",maxlength:"128",disabled:busy(),oninput:move|e|draft.write().date=e.value()}
            }
        }
        p{class:"text-secondary",{i18n.t("tenant_keys.metadata_only")}}
        p{class:"text-secondary",{i18n.t("tenant.command_hint")}}
        div{class:"modal-actions",
            button{class:"btn btn-secondary",onmounted:move|e|async move{let _=e.set_focus(true).await;},disabled:busy(),onclick:move |_|on_close.call(()),{i18n.t("form.cancel")}}
            button{class:"btn btn-primary",disabled:busy(),onclick:submit,{i18n.t("tenant.confirm")}}
        }
    }}}
}

#[component]
fn OwnerPicker(scope: WorkspaceScope, disabled: bool, on_select: EventHandler<Uuid>) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut search = use_signal(String::new);
    let mut filter = use_signal(String::new);
    let mut page = use_signal(|| 1u32);
    let data = use_resource(move || {
        let q = (filter(), page());
        let key = (scope, q.clone());
        async move {
            let result = common::read(auth, users, scope, move |token| {
                let q = q.clone();
                async move { scope.api()?.members(q.1, 20, Some(&q.0), &token).await }
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let loaded = current_keyed_value(&(scope, (filter(), page())), data.state().cloned(), data());
    rsx! {section{class:"key-owner-picker",
        label{class:"form-label",r#for:"key-owner-search",{i18n.t("tenant_keys.search_member")}}
        div{class:"toolbar",style:"gap:8px;flex-wrap:wrap",
            input{id:"key-owner-search",class:"input-field",value:"{search}",maxlength:"255",disabled,oninput:move|e|search.set(e.value())}
            button{class:"btn btn-secondary",disabled,onclick:move |_|{filter.set(search().trim().into());page.set(1);},{i18n.t("tenant_keys.search_member")}}
        }
        match loaded{
            None=>rsx!{p{{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{p{role:"alert",{user_error_message(&e)}}},
            Some(Ok(members))=>rsx!{
                select{id:"key-owner-option",class:"input-field",disabled,value:"",onchange:move|e|if let Ok(id)=e.value().parse::<Uuid>() && !id.is_nil(){on_select.call(id);},
                    option{value:"",{i18n.t("tenant_keys.choose_member")}}
                    for member in &members.items{option{key:"{member.user_id}",value:"{member.user_id}",disabled:member.user_id.is_nil()||member.membership_status!=MembershipStatus::Active||member.user_status!=UserStatus::Active,"{member.email} · {member.user_id}"}}
                }
                Pager{page:page(),total_pages:members.total_pages,total:members.total,on_page:move|value|if !disabled{page.set(value);}}
            }
        }
    }}
}
