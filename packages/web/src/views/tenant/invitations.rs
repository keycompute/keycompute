use super::common::{self, CommandDialog, Pager, WorkspaceLinks, WorkspaceScope};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::user_error_message,
    stores::{auth_store::AuthStore, ui_store::UiStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::{
    ClientError, TenantRole,
    api::tenant_control::{
        CreateInvitation, InvitationCreateResponse, InvitationOutcome, NotificationStatus,
        TenantInvitation,
    },
};
use dioxus::prelude::*;
fn notification_label(value: &NotificationStatus) -> &'static str {
    match value {
        NotificationStatus::Sent => "tenant.notification_sent",
        NotificationStatus::Failed => "tenant.notification_failed",
        NotificationStatus::Unconfigured => "tenant.notification_unconfigured",
        NotificationStatus::NotApplicable => "tenant.notification_duplicate",
    }
}
/// The router may reuse an Outlet VNode across AppShell remounts. A page-local
/// key must discard drafts, confirmation dialogs and one-time secrets as well.
#[component]
pub fn TenantInvitations() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let epoch = (auth.state)().session_id;
    let scope = WorkspaceScope::from_stores(auth, users);
    let key = format!("{epoch}:{scope:?}");
    // A keyed fragment is required: a key on one static component alone
    // does not engage Dioxus's keyed child reconciliation.
    rsx! { for identity in [key] { TenantInvitationsPage { key: "{identity}" } } }
}

#[component]
fn TenantInvitationsPage() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut ui = use_context::<UiStore>();
    let i18n = use_i18n();
    let scope = WorkspaceScope::from_stores(auth, users);
    let mut page = use_signal(|| 1u32);
    let mut email = use_signal(String::new);
    let mut role = use_signal(|| TenantRole::Member);
    let mut hours = use_signal(|| "24".to_owned());
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut pending = use_signal(|| None::<TenantInvitation>);
    // This one-time recovery link is intentionally kept only in this keyed component.
    let mut created = use_signal(|| None::<InvitationCreateResponse>);
    let mut data = use_resource(move || {
        let scope = WorkspaceScope::from_stores(auth, users);
        let page = page();
        async move {
            let result = if let Some(scope) = scope {
                common::read(auth, users, scope, move |token| async move {
                    scope.api()?.invitations(page, 20, &token).await
                })
                .await
            } else {
                Err(ClientError::Forbidden(
                    "selected membership required".into(),
                ))
            };
            KeyedResourceValue::new((scope, page), result)
        }
    });
    let loaded = current_keyed_value(&(scope, page()), data.state().cloned(), data());
    let create = move |_| {
        if busy() {
            return;
        }
        let Some(scope) = WorkspaceScope::from_stores(auth, users) else {
            return;
        };
        let Some(seconds) = hours()
            .parse::<i64>()
            .ok()
            .filter(|h| (1..=168).contains(h))
            .and_then(|h| h.checked_mul(3600))
        else {
            error.set(i18n.t("tenant.invalid_duration").into());
            return;
        };
        let email = email().trim().to_owned();
        if email.is_empty() || email.len() > 255 {
            error.set(i18n.t("tenant.invalid_email").into());
            return;
        }
        let body = CreateInvitation {
            email,
            tenant_role: role(),
            expires_in_seconds: seconds,
        };
        busy.set(true);
        error.set(String::new());
        created.set(None);
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| async move {
                scope.api()?.create_invitation(&body, &token).await
            })
            .await;
            if !scope.is_current(auth, users) {
                return;
            }
            busy.set(false);
            match result {
                Ok(result) => {
                    created.set(Some(result));
                    page.set(1);
                    data.restart();
                }
                Err(e) => {
                    error.set(user_error_message(&e));
                    data.restart();
                }
            }
        });
    };
    let revoke = move |_| {
        if busy() {
            return;
        }
        let (Some(scope), Some(invitation)) = (WorkspaceScope::from_stores(auth, users), pending())
        else {
            return;
        };
        busy.set(true);
        error.set(String::new());
        created.set(None);
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| async move {
                scope.api()?.revoke_invitation(invitation.id, &token).await
            })
            .await;
            if !scope.is_current(auth, users) {
                return;
            }
            busy.set(false);
            pending.set(None);
            match result {
                Ok(_) => {
                    data.restart();
                    ui.show_success(i18n.t("tenant.saved"));
                }
                Err(e) => {
                    error.set(user_error_message(&e));
                    data.restart();
                }
            }
        });
    };
    rsx! {div {class:"page-container tenant-invitations",
        ui::PageHeader {title:i18n.t("tenant.invitations").to_string(),description:i18n.t("tenant.invitations_hint").to_string()}
        WorkspaceLinks {}
        if !error().is_empty(){div {class:"alert alert-error",role:"alert","{error}" p {{i18n.t("tenant.command_hint")}}}}
        section {class:"section",aria_label:i18n.t("tenant.invite"),
            label {class:"form-label",r#for:"invite-email",{i18n.t("tenant.email")}}
            input {id:"invite-email",class:"input-field",r#type:"email",maxlength:"255",value:"{email}",disabled:busy(),oninput:move |e|email.set(e.value())}
            label {class:"form-label",r#for:"invite-role",{i18n.t("tenant.role")}}
            select {id:"invite-role",class:"input-field",value:"{role().as_str()}",disabled:busy(),onchange:move |e|{if let Ok(value)=e.value().parse::<TenantRole>(){role.set(value);}},option {value:"member","member"} option {value:"admin","admin"}}
            label {class:"form-label",r#for:"invite-duration",{i18n.t("tenant.duration_hours")}}
            input {id:"invite-duration",class:"input-field",r#type:"number",min:"1",max:"168",value:"{hours}",disabled:busy(),oninput:move |e|hours.set(e.value())}
            button {class:"btn btn-primary",disabled:busy()||email().trim().is_empty(),onclick:create,{i18n.t("tenant.invite")}}
        }
        if let Some(result)=created(){section {class:"alert alert-info",role:"status",
            p {{i18n.t(notification_label(&result.notification))}}
            if result.outcome==InvitationOutcome::AlreadyPending {p {{i18n.t("tenant.already_pending")}}}
            if let Some(link)=result.acceptance_link {label {class:"form-label",r#for:"invite-recovery",{i18n.t("tenant.recovery_link")}}
                input {id:"invite-recovery",class:"input-field",readonly:true,value:link,autocomplete:"off"}
                p {{i18n.t("tenant.recovery_hint")}}
            }
            button {class:"btn btn-secondary",onclick:move |_|created.set(None),{i18n.t("tenant.hide")}}
        }}
        button {class:"btn btn-secondary",disabled:busy(),onclick:move |_|data.restart(),{i18n.t("tenant.reload")}}
        match loaded {
            None=>rsx!{p {role:"status",{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{div {class:"alert alert-error",role:"alert",{user_error_message(&e)}}},
            Some(Ok(value))=>rsx!{
                div {class:"table-pagination-panel",table {class:"table",
                    thead {tr {th {{i18n.t("tenant.email")}} th {{i18n.t("tenant.role")}} th {{i18n.t("tenant.result")}} th {{i18n.t("tenant.expires")}} th {{i18n.t("tenant.actions")}}}}
                    tbody {for invitation in value.items.iter(){
                        {let selected=invitation.clone();rsx!{tr {key:"{invitation.id}",td {"{invitation.email}"} td {"{invitation.tenant_role.as_str()}"} td {"{invitation.status}"} td {"{invitation.expires_at}"}
                            td {if invitation.status=="pending" {button {class:"btn btn-danger btn-sm",disabled:busy(),onclick:move |_|pending.set(Some(selected.clone())),{i18n.t("tenant.revoke")}}}}
                        }}}
                    }}
                } if value.items.is_empty(){p {{i18n.t("tenant.empty")}}}}
                Pager {page:page(),total_pages:value.total_pages,total:value.total,on_page:move |p|page.set(p)}
            }
        }
        if let Some(invitation)=pending(){CommandDialog {title:i18n.t("tenant.revoke").to_string(),target:invitation.email,busy:busy(),on_cancel:move |_|pending.set(None),on_confirm:revoke}}
    }}
}
