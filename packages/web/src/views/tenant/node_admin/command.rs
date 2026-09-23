use super::super::common::{self, WorkspaceScope};
use super::types::{Action, Pending, Row};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, user_store::UserStore},
};
use client_api::{
    ClientError, Result,
    api::node_control::{
        NodeCommand, NodeControlApi, NodeOperation, NodePatch, RegistrationAction,
        RegistrationCommand, TaskOperation,
    },
};
use dioxus::prelude::*;
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Draft {
    pub reason: String,
    pub name: String,
    pub threshold: String,
}
impl Draft {
    pub fn valid(&self, pending: &Pending) -> bool {
        let reason = self.reason.trim();
        if reason.is_empty()
            || reason.chars().count() > 1000
            || reason.chars().any(char::is_control)
            || chrono::DateTime::parse_from_rfc3339(pending.row.version()).is_err()
            || !pending.row.actions().contains(&pending.action)
        {
            return false;
        }
        if pending.action == Action::Configure {
            let threshold = self.threshold.parse::<i32>().ok();
            if self.name.trim().is_empty()
                || self.name.chars().count() > 200
                || self.name.chars().any(char::is_control)
                || threshold.is_none_or(|v| !(1..=100).contains(&v))
            {
                return false;
            }
        }
        true
    }
}
#[component]
pub(super) fn CommandDialog(
    scope: WorkspaceScope,
    pending: Pending,
    on_close: EventHandler<()>,
    on_success: EventHandler<String>,
) -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut draft = use_signal(|| match &pending.row {
        Row::Node(r) => Draft {
            name: r.display_name.clone(),
            threshold: r.failure_threshold.to_string(),
            ..Default::default()
        },
        _ => Draft::default(),
    });
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    let configure = pending.action == Action::Configure;
    let title = i18n.t(pending.action.label());
    let resource = pending.row.id();
    let owner = pending.row.owner();
    let version = pending.row.version().to_string();
    let allowed = draft().valid(&pending);
    let confirm = move |_| {
        if busy() {
            return;
        }
        let body = draft();
        let command = pending.clone();
        if !body.valid(&command)
            || command.row.tenant() != scope.tenant_id
            || command.row.id().is_nil()
            || command.row.owner().is_nil()
        {
            error.set(i18n.t("tenant_nodes.invalid_command").into());
            return;
        }
        busy.set(true);
        error.set(String::new());
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| async move {
                let api = NodeControlApi::tenant(&get_client(), scope.tenant_id)?;
                execute(api, command, body, &token).await
            })
            .await;
            if !scope.is_current(auth, users) {
                return;
            }
            busy.set(false);
            match result {
                Ok((label, extra)) => on_success.call(if extra.is_empty() {
                    i18n.t(label).to_string()
                } else {
                    format!("{}: {}", i18n.t(label), extra)
                }),
                Err(e) => error.set(user_error_message(&e)),
            }
        });
    };
    rsx! {div {class:"modal-backdrop",
        div {class:"modal",role:"dialog",aria_modal:"true",aria_label:title,
            div {class:"modal-header",h2 {"{title}"}}
            div {class:"modal-body",
                p {"{resource}"} p {{i18n.t("tenant_nodes.owner_short")} " {owner}"}
                p {class:"text-secondary",{i18n.t("tenant_nodes.revision")} " {version}"}
                p {class:"text-secondary",{i18n.t("tenant_nodes.lifecycle")}}
                if !error().is_empty(){p {class:"alert alert-error",role:"alert","{error}"}}
                if configure {
                    label {r#for:"node-name",{i18n.t("tenant_nodes.name")}}
                    input {id:"node-name",class:"input-field",value:"{draft().name}",maxlength:"200",disabled:busy(),oninput:move |e|draft.write().name=e.value()}
                    label {r#for:"node-threshold",{i18n.t("tenant_nodes.threshold")}}
                    input {id:"node-threshold",class:"input-field",r#type:"number",min:"1",max:"100",value:"{draft().threshold}",disabled:busy(),oninput:move |e|draft.write().threshold=e.value()}
                }
                label {r#for:"node-command-reason",{i18n.t("tenant_nodes.reason")}}
                textarea {id:"node-command-reason",class:"input-field",value:"{draft().reason}",maxlength:"1000",disabled:busy(),oninput:move |e|draft.write().reason=e.value()}
                p {{i18n.t("tenant.command_hint")}}
            }
            div {class:"modal-footer",
                button {class:"btn btn-secondary",disabled:busy(),onclick:move |_|on_close.call(()),{i18n.t("form.cancel")}}
                button {class:"btn btn-primary",disabled:busy()||!allowed,onclick:confirm,{i18n.t("tenant.confirm")}}
            }
        }
    }}
}
async fn execute(
    api: NodeControlApi,
    pending: Pending,
    draft: Draft,
    token: &str,
) -> Result<(&'static str, String)> {
    let expected = pending.row.version().to_string();
    let id = pending.row.id();
    let command = NodeCommand {
        expected_updated_at: expected.clone(),
        reason: draft.reason.trim().into(),
    };
    match (&pending.row, pending.action) {
        (Row::Node(_), action) => {
            let result = match action {
                Action::Configure => {
                    api.configure(
                        id,
                        &NodePatch {
                            expected_updated_at: expected,
                            reason: command.reason,
                            display_name: Some(draft.name.trim().into()),
                            failure_threshold: draft.threshold.parse().ok(),
                        },
                        token,
                    )
                    .await?
                }
                Action::Exclude => {
                    api.operate(id, NodeOperation::Exclude, &command, token)
                        .await?
                }
                Action::Recover => {
                    api.operate(id, NodeOperation::Recover, &command, token)
                        .await?
                }
                Action::Revoke => {
                    api.operate(id, NodeOperation::Revoke, &command, token)
                        .await?
                }
                Action::Delete => api.delete(id, &command, token).await?,
                _ => return Err(ClientError::Config("Invalid node operation".into())),
            };
            if result.node.id != id
                || result.node.tenant_id != pending.row.tenant()
                || result.node.owner_user_id != pending.row.owner()
            {
                return Err(ClientError::InvalidResponse(
                    "Node mutation identity mismatch; refresh records".into(),
                ));
            }
            Ok((
                if result.deleted {
                    "tenant_nodes.deleted"
                } else if result.changed {
                    "tenant_nodes.updated"
                } else {
                    "tenant_nodes.unchanged"
                },
                result.node.status,
            ))
        }
        (Row::Task(_), action @ (Action::Cancel | Action::Archive)) => {
            let operation = if action == Action::Cancel {
                TaskOperation::Cancel
            } else {
                TaskOperation::Archive
            };
            let result = api.operate_task(id, operation, &command, token).await?;
            if result.task.id != id
                || result.task.tenant_id != pending.row.tenant()
                || result.task.user_id != pending.row.owner()
            {
                return Err(ClientError::InvalidResponse(
                    "Task mutation identity mismatch; refresh records".into(),
                ));
            }
            Ok((
                if result.archived {
                    "tenant_nodes.archived"
                } else if result.cancellation_requested {
                    "tenant_nodes.cancel_requested"
                } else {
                    "tenant_nodes.unchanged"
                },
                result.task.status,
            ))
        }
        (Row::Registration(_), action) => {
            let action = match action {
                Action::Approve => RegistrationAction::Approve,
                Action::Reject => RegistrationAction::Reject,
                Action::RevokeRegistration => RegistrationAction::Revoke,
                _ => return Err(ClientError::Config("Invalid registration decision".into())),
            };
            let result = api
                .decide_registration(
                    id,
                    &RegistrationCommand {
                        expected_updated_at: expected,
                        reason: command.reason,
                        action,
                    },
                    token,
                )
                .await?;
            if result.token.id != id
                || result.token.tenant_id != pending.row.tenant()
                || result.token.user_id != pending.row.owner()
            {
                return Err(ClientError::InvalidResponse(
                    "Registration identity mismatch; refresh records".into(),
                ));
            }
            Ok((
                "tenant_nodes.registration_decided",
                format!("{}; {}", result.token.status, result.notification),
            ))
        }
        _ => Err(ClientError::Config("Invalid resource command".into())),
    }
}
