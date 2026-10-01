use super::{
    super::common::{self, WorkspaceScope},
    types::{Draft, Operation},
};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, ui_store::UiStore, user_store::UserStore},
};
use client_api::api::distribution_policy::{BeneficiaryScope, DistributionPolicyApi};
use client_api::{MembershipStatus, UserStatus};
use dioxus::prelude::*;

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
    let i = use_i18n();
    let initial = op.clone();
    let mut draft = use_signal(move || Draft::for_operation(&initial));
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    let creating = matches!(op, Operation::Create);
    let editing = creating || matches!(op, Operation::Edit(_));
    let defaulting = matches!(op, Operation::Default);
    let deleting = matches!(op, Operation::Delete(_));
    let policy_id = op.row().map(|row| row.id.to_string());
    let label = op.label();
    let submit_op = op.clone();
    let members = use_resource(move || async move {
        if creating {
            common::read(auth, users, scope, move |token| async move {
                scope.api()?.members(1, 100, None, &token).await
            })
            .await
        } else {
            Err(client_api::ClientError::Config(
                "member picker not used".into(),
            ))
        }
    });
    let submit = move |_| {
        if busy() {
            return;
        }
        let current = draft();
        let command = submit_op.clone();
        let validation = match &command {
            Operation::Create => current.create().map(|_| ()),
            Operation::Edit(row) => current.patch(row).map(|_| ()),
            Operation::Delete(row) => {
                if row.updated_at.trim().is_empty() {
                    Err(client_api::ClientError::Config(
                        "Reload the policy revision".into(),
                    ))
                } else if current.reason.trim().is_empty() {
                    Err(client_api::ClientError::Config(
                        "A change reason is required".into(),
                    ))
                } else {
                    Ok(())
                }
            }
            Operation::Default => {
                client_api::api::distribution_policy::validate_commission_rate(current.rate.trim())
                    .and_then(|_| {
                        if current.name.trim().is_empty() || current.reason.trim().is_empty() {
                            Err(client_api::ClientError::Config(
                                "Default policy name and reason are required".into(),
                            ))
                        } else {
                            Ok(())
                        }
                    })
            }
        };
        if let Err(e) = validation {
            error.set(user_error_message(&e));
            return;
        }
        busy.set(true);
        error.set(String::new());
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| async move {
                let api = DistributionPolicyApi::tenant(&get_client(), scope.tenant_id)?;
                match command {
                    Operation::Create => {
                        api.create(&current.create()?, &token).await?;
                    }
                    Operation::Edit(row) => {
                        api.patch(row.id, &current.patch(&row)?, &token).await?;
                    }
                    Operation::Delete(row) => {
                        api.delete(row.id, &row.updated_at, current.reason.trim(), &token)
                            .await?;
                    }
                    Operation::Default => {
                        api.set_default(
                            current.name.trim(),
                            current.rate.trim(),
                            current.reason.trim(),
                            &token,
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
                    ui.show_success(i.t("tenant.saved"));
                    on_changed.call(())
                }
                Err(e) => error.set(user_error_message(&e)),
            }
        });
    };
    rsx! {div{class:"modal-overlay",div{class:"modal tenant-distribution-editor",style:"width:min(820px,95vw);max-height:85vh;overflow:auto",role:"dialog",aria_modal:"true",aria_label:i.t(label),tabindex:"-1",onkeydown:move|e|{if e.key()==Key::Escape&&!busy(){e.stop_propagation();on_close.call(())}},h2{{i.t(label)}}p{class:"text-secondary",{i.t("tenant_distribution.scope")} " {scope.tenant_id}"}
     if let Some(policy_id)=policy_id.as_ref(){p{class:"text-secondary",{i.t("tenant_distribution.policy_id")} " {policy_id}"}}
     if !error().is_empty(){p{class:"alert alert-error",role:"alert","{error}"}}
     if editing||defaulting{label{class:"form-label",r#for:"distribution-name",{i.t("tenant_distribution.name")}}input{id:"distribution-name",class:"input-field",maxlength:"255",value:"{draft().name}",disabled:busy(),oninput:move|e|draft.write().name=e.value()}
      label{class:"form-label",r#for:"distribution-rate",{i.t("tenant_distribution.rate")}}input{id:"distribution-rate",class:"input-field",inputmode:"decimal",maxlength:"16",value:"{draft().rate}",disabled:busy(),oninput:move|e|draft.write().rate=e.value()}p{class:"text-secondary",{i.t("tenant_distribution.rate_hint")}}
     }
     if editing{
      if creating{label{class:"form-label",r#for:"distribution-beneficiary",{i.t("tenant_distribution.beneficiary")}}select{id:"distribution-beneficiary",class:"input-field",value:match draft().beneficiary_scope{BeneficiaryScope::Everyone=>"everyone",BeneficiaryScope::TenantMember=>"tenant_member"},disabled:busy(),onchange:move|e|draft.write().beneficiary_scope=if e.value()=="tenant_member"{BeneficiaryScope::TenantMember}else{BeneficiaryScope::Everyone},option{value:"everyone",{i.t("tenant_distribution.everyone")}}option{value:"tenant_member",{i.t("tenant_distribution.member")}}}
       if draft().beneficiary_scope==BeneficiaryScope::TenantMember{label{class:"form-label",r#for:"distribution-member",{i.t("tenant_distribution.member")}}input{id:"distribution-member",class:"input-field",list:"distribution-member-options",maxlength:"36",value:"{draft().beneficiary_id}",disabled:busy(),oninput:move|e|draft.write().beneficiary_id=e.value()}datalist{id:"distribution-member-options",if let Some(Ok(page))=members(){for m in page.items.iter().filter(|m|m.membership_status==MembershipStatus::Active&&m.user_status==UserStatus::Active){option{value:"{m.user_id}","{m.email}"}}}}p{class:"text-secondary",{i.t("tenant_distribution.member_hint")}}}}
      else{p{class:"text-secondary",{i.t("tenant_distribution.immutable_beneficiary")} " " {draft().beneficiary_id.clone()}}}
      label{class:"form-label",r#for:"distribution-description",{i.t("tenant_distribution.description")}}textarea{id:"distribution-description",class:"input-field",maxlength:"4096",value:"{draft().description}",disabled:busy(),oninput:move|e|draft.write().description=e.value()}
      label{class:"form-label",r#for:"distribution-priority",{i.t("tenant_distribution.priority")}}input{id:"distribution-priority",class:"input-field",r#type:"number",min:"-1000",max:"1000",value:"{draft().priority}",disabled:busy(),oninput:move|e|draft.write().priority=e.value()}
      label{class:"form-label",r#for:"distribution-from",{i.t("tenant_distribution.from")}}input{id:"distribution-from",class:"input-field",value:"{draft().from}",placeholder:"2026-10-01T00:00:00Z",disabled:busy()||!creating,oninput:move|e|draft.write().from=e.value()}
      label{class:"form-label",r#for:"distribution-until",{i.t("tenant_distribution.until")}}input{id:"distribution-until",class:"input-field",value:"{draft().until}",placeholder:"2026-12-01T00:00:00Z",disabled:busy(),oninput:move|e|draft.write().until=e.value()}
      if !creating{label{class:"form-label",input{r#type:"checkbox",checked:draft().active,disabled:busy(),onchange:move|e|draft.write().active=e.checked()}{i.t("tenant_distribution.active")}}}
     }
     if deleting{p{{i.t("tenant_distribution.delete_hint")}}}
     label{class:"form-label",r#for:"distribution-reason",{i.t("tenant_distribution.reason")}}textarea{id:"distribution-reason",class:"input-field",maxlength:"500",value:"{draft().reason}",disabled:busy(),oninput:move|e|draft.write().reason=e.value()}
     p{class:"text-secondary",{i.t("tenant_distribution.effect_hint")}}
     div{class:"modal-actions",button{class:"btn btn-secondary",onmounted:move|e|async move{let _=e.set_focus(true).await;},disabled:busy(),onclick:move |_|on_close.call(()),{i.t("form.cancel")}}button{class:"btn btn-primary",disabled:busy(),onclick:submit,{i.t("tenant.confirm")}}}
    }}}
}
