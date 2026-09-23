use super::{
    super::common::{self, WorkspaceScope},
    types::{Draft, Operation},
};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, ui_store::UiStore, user_store::UserStore},
};
use client_api::api::tenant_pricing::{BillingDimension, TenantPricingApi};
use dioxus::prelude::*;

#[component]
pub(super) fn Editor(
    scope: WorkspaceScope,
    op: Operation,
    on_close: EventHandler<()>,
    on_changed: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut ui = use_context::<UiStore>();
    let initial = op.clone();
    let mut draft = use_signal(move || Draft::for_operation(&initial));
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    let creating = matches!(op, Operation::Create);
    let editing = creating || matches!(op, Operation::Edit(_));
    let label = op.label();
    let submit = move |_| {
        if busy() {
            return;
        }
        let current = draft();
        let command = op.clone();
        let validation = match &command {
            Operation::Create => current.create().map(|_| ()),
            Operation::Edit(row) => current.update(row).map(|_| ()),
            _ => Ok(()),
        };
        if let Err(e) = validation {
            error.set(user_error_message(&e));
            return;
        }
        busy.set(true);
        error.set(String::new());
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| async move {
                let api = TenantPricingApi::new(&get_client(), scope.tenant_id)?;
                match command {
                    Operation::Create => {
                        api.create(&current.create()?, &token).await?;
                    }
                    Operation::Edit(row) => {
                        api.update(row.id, &current.update(&row)?, &token).await?;
                    }
                    Operation::Delete(row) => {
                        api.delete(row.id, &token).await?;
                    }
                    Operation::Default(row) => {
                        api.make_default(row.id, &token).await?;
                    }
                };
                Ok(())
            })
            .await;
            if !scope.is_current(auth, users) {
                return;
            }
            busy.set(false);
            match result {
                Ok(()) => {
                    ui.show_success(i18n.t("tenant.saved"));
                    on_changed.call(());
                }
                Err(e) => error.set(user_error_message(&e)),
            }
        });
    };
    rsx! {div {class:"modal-overlay",
        div {class:"modal tenant-pricing-editor",role:"dialog",aria_modal:"true",aria_label:i18n.t(label),
            h2 {{i18n.t(label)}}
            p {class:"text-secondary",{i18n.t("tenant_pricing.scope")} " {scope.tenant_id}"}
            if !error().is_empty(){p {class:"alert alert-error",role:"alert","{error}"}}
            if editing {
                label {class:"form-label",r#for:"pricing-model",{i18n.t("tenant_pricing.model")}}
                input {id:"pricing-model",class:"input-field",value:"{draft().model}",maxlength:"255",disabled:busy()||!creating,oninput:move|e|draft.write().model=e.value()}
                label {class:"form-label",r#for:"pricing-dimension",{i18n.t("tenant_pricing.dimension")}}
                select {id:"pricing-dimension",class:"input-field",value:draft().dimension.as_str(),disabled:busy()||!creating,onchange:move |e|{
                    let value=match e.value().as_str(){"provideraccount"=>BillingDimension::ProviderAccount,"node"=>BillingDimension::Node,_=>return};draft.write().dimension=value;
                },option {value:"provideraccount","Provider account"} option {value:"node","Node"}}
                label {class:"form-label",r#for:"pricing-currency",{i18n.t("tenant_pricing.currency")}}
                input {id:"pricing-currency",class:"input-field",value:"{draft().currency}",maxlength:"3",disabled:busy()||!creating,oninput:move|e|draft.write().currency=e.value()}
                label {class:"form-label",r#for:"pricing-input",{i18n.t("tenant_pricing.input")}}
                input {id:"pricing-input",class:"input-field",r#type:"text",inputmode:"decimal",value:"{draft().input}",maxlength:"64",disabled:busy(),oninput:move|e|draft.write().input=e.value()}
                label {class:"form-label",r#for:"pricing-output",{i18n.t("tenant_pricing.output")}}
                input {id:"pricing-output",class:"input-field",r#type:"text",inputmode:"decimal",value:"{draft().output}",maxlength:"64",disabled:busy(),oninput:move|e|draft.write().output=e.value()}
                label {class:"form-label",r#for:"pricing-from",{i18n.t("tenant_pricing.from")}}
                input {id:"pricing-from",class:"input-field",value:"{draft().from}",placeholder:"2026-10-01T00:00:00Z",disabled:busy()||!creating,oninput:move|e|draft.write().from=e.value()}
                label {class:"form-label",r#for:"pricing-until",{i18n.t("tenant_pricing.until")}}
                input {id:"pricing-until",class:"input-field",value:"{draft().until}",placeholder:"2026-11-01T00:00:00Z",disabled:busy(),oninput:move|e|draft.write().until=e.value()}
                p {class:"text-secondary",{i18n.t(if creating{"tenant_pricing.create_times"}else{"tenant_pricing.edit_times"})}}
                if creating {label {class:"form-label",input {r#type:"checkbox",checked:draft().is_default,disabled:busy(),onchange:move|e|draft.write().is_default=e.checked()} {i18n.t("tenant_pricing.default")}}}
            } else {p {"{draft().model} · {draft().dimension.as_str()} · {draft().currency}"}
                p {{i18n.t(if label=="tenant_pricing.delete"{"tenant_pricing.delete_hint"}else{"tenant_pricing.default_hint"})}}
            }
            p {class:"text-secondary",{i18n.t("tenant.command_hint")}}
            div {class:"modal-actions",
                button {class:"btn btn-secondary",disabled:busy(),onclick:move |_|on_close.call(()),{i18n.t("form.cancel")}}
                button {class:"btn btn-primary",disabled:busy(),onclick:submit,{i18n.t("tenant.confirm")}}
            }
        }
    }}
}
