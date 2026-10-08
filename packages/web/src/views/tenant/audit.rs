use super::common::{self, Pager, TechnicalId, WorkspaceContext, WorkspaceLinks, WorkspaceScope};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::user_error_message,
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::{
        resource::{KeyedResourceValue, current_keyed_value},
        time::format_time,
    },
};
use client_api::ClientError;
use dioxus::prelude::*;

fn action_label(i18n: crate::i18n::I18n, action: &str) -> String {
    let exact = match action {
        "tenant.transfer_owner" => Some("tenant.audit_action_transfer_owner"),
        "account.health_reset" => Some("tenant.audit_action_health_reset"),
        "account.probe_requested" | "passthrough_binding.probe_requested" => {
            Some("tenant.audit_action_probe")
        }
        "access_grant.pool_enabled" => Some("tenant.audit_action_pool"),
        "withdrawal.payout_access" => Some("tenant.audit_action_payout"),
        "balance.reservation_release" => Some("tenant.audit_action_release"),
        _ => None,
    };
    let generic = action
        .rsplit('.')
        .next()
        .and_then(|operation| match operation {
            "create" => Some("tenant.audit_action_create"),
            "update" => Some("tenant.audit_action_update"),
            "delete" => Some("tenant.audit_action_delete"),
            "status" => Some("tenant.audit_action_status"),
            "role" => Some("tenant.audit_action_role"),
            "request" => Some("tenant.audit_action_request"),
            "approve" => Some("tenant.audit_action_approve"),
            "reject" => Some("tenant.audit_action_reject"),
            "revoke" => Some("tenant.audit_action_revoke"),
            "cancel" => Some("tenant.audit_action_cancel"),
            "claim" => Some("tenant.audit_action_claim"),
            "decline" => Some("tenant.audit_action_decline"),
            "expire" => Some("tenant.audit_action_expire"),
            "probe" => Some("tenant.audit_action_probe"),
            "configure" => Some("tenant.audit_action_configure"),
            "exclude" => Some("tenant.audit_action_exclude"),
            "recover" => Some("tenant.audit_action_recover"),
            "archive" => Some("tenant.audit_action_archive"),
            "complete" => Some("tenant.audit_action_complete"),
            "accept" => Some("tenant.audit_action_accept"),
            "join" => Some("tenant.audit_action_join"),
            "reveal" => Some("tenant.audit_action_reveal"),
            "default" => Some("tenant.audit_action_default"),
            "security" => Some("tenant.audit_action_security"),
            "password" => Some("tenant.audit_action_password"),
            "profile" => Some("tenant.audit_action_profile"),
            "convert" => Some("tenant.audit_action_convert"),
            _ => None,
        });
    exact
        .or(generic)
        .map(|key| i18n.t(key).to_owned())
        .unwrap_or_else(|| action.to_owned())
}

fn resource_label(i18n: crate::i18n::I18n, resource: &str) -> String {
    let key = match resource {
        "tenant" => Some("tenant.audit_resource_tenant"),
        "tenant_membership" => Some("tenant.audit_resource_membership"),
        "tenant_invitation" => Some("tenant.audit_resource_invitation"),
        "account" => Some("tenant.audit_resource_account"),
        "passthrough_binding" => Some("tenant.audit_resource_binding"),
        "api_key" | "key" => Some("tenant.audit_resource_key"),
        "key_issuance" => Some("tenant.audit_resource_key_issuance"),
        "pricing_model" => Some("tenant.audit_resource_pricing"),
        "distribution_rule" => Some("tenant.audit_resource_distribution"),
        "node" => Some("tenant.audit_resource_node"),
        "node_registration" => Some("tenant.audit_resource_node_registration"),
        "node_task" => Some("tenant.audit_resource_node_task"),
        "response" => Some("tenant.audit_resource_response"),
        "conversation" => Some("tenant.audit_resource_conversation"),
        "tip_withdrawal" => Some("tenant.audit_resource_withdrawal"),
        "balance_reservation" => Some("tenant.audit_resource_reservation"),
        "user" => Some("tenant.audit_resource_user"),
        "system_setting" => Some("tenant.audit_resource_setting"),
        _ => None,
    };
    key.map(|key| i18n.t(key).to_owned())
        .unwrap_or_else(|| resource.to_owned())
}

fn result_label(i18n: crate::i18n::I18n, result: &str) -> String {
    match result {
        "success" => i18n.t("tenant.audit_result_success").to_owned(),
        "denied" => i18n.t("tenant.audit_result_denied").to_owned(),
        "failure" => i18n.t("tenant.audit_result_failure").to_owned(),
        _ => result.to_owned(),
    }
}
/// The router may reuse an Outlet VNode across AppShell remounts. A page-local
/// key must discard drafts, confirmation dialogs and one-time secrets as well.
#[component]
pub fn TenantAudit() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let epoch = (auth.state)().session_id;
    let scope = WorkspaceScope::from_stores(auth, users);
    let key = format!("{epoch}:{scope:?}");
    // A keyed fragment is required: a key on one static component alone
    // does not engage Dioxus's keyed child reconciliation.
    rsx! { for identity in [key] { TenantAuditPage { key: "{identity}" } } }
}

#[component]
fn TenantAuditPage() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut page = use_signal(|| 1u32);
    let scope = WorkspaceScope::from_stores(auth, users);
    let mut data = use_resource(move || {
        let scope = WorkspaceScope::from_stores(auth, users);
        let page = page();
        async move {
            let result = if let Some(scope) = scope {
                common::read(auth, users, scope, move |token| async move {
                    scope.api()?.audit(page, 20, &token).await
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
    rsx! {div {class:"page-container tenant-audit",
        ui::PageHeader {title:i18n.t("tenant.audit").to_string(),description:i18n.t("tenant.audit_hint").to_string()}
        WorkspaceLinks {}
        WorkspaceContext {}
        button {class:"btn btn-secondary",onclick:move |_|data.restart(),{i18n.t("tenant.reload")}}
        match loaded {
            None=>rsx!{div {class:"content-loading",role:"status",span {class:"spinner",aria_hidden:"true"} span {{i18n.t("common.loading")}}}},
            Some(Err(e))=>rsx!{div {class:"alert alert-error",role:"alert",{user_error_message(i18n, &e)}}},
            Some(Ok(value))=>rsx!{
                div {class:"table-pagination-panel table-pagination-frame",
                    div {class:"table-container",tabindex:"0",table {class:"table",thead {tr {th {{i18n.t("tenant.time")}} th {{i18n.t("tenant.actor")}} th {{i18n.t("tenant.action")}} th {{i18n.t("tenant.resource")}} th {{i18n.t("tenant.result")}}}}
                        tbody {for event in value.items.iter(){
                            {let request_id=event.request_id.map(|id|id.to_string()).unwrap_or_else(||"—".into());rsx!{
                                tr {key:"{event.id}",
                                    td {{format_time(&event.created_at)}}
                                    td {TechnicalId {value:event.actor_user_id.to_string()}}
                                    td {{action_label(i18n,&event.action)}}
                                    td {{resource_label(i18n,&event.resource_type)} if let Some(id)=event.resource_id.as_deref(){p{TechnicalId{value:id.to_string()}}}else{p{"—"}}}
                                    td {{result_label(i18n,&event.result)} details {summary {{i18n.t("tenant.audit_technical_details")}} p{{i18n.t("tenant.audit_request_id")}} code {"{request_id}"} p{{i18n.t("tenant.audit_metadata")}} pre {"{event.metadata}"}}}
                                }
                            }}
                        }}
                    }}
                    if value.items.is_empty(){div {class:"empty-state",h3 {class:"empty-title",{i18n.t("tenant.empty")}}}}
                }
                Pager {page:page(),total_pages:value.total_pages,total:value.total,on_page:move |p|page.set(p)}
            }
        }
    }}
}

#[cfg(test)]
mod tests {
    use super::{action_label, resource_label, result_label};
    use crate::i18n::{I18n, Lang};

    #[test]
    fn known_audit_codes_are_localized_and_unknown_codes_survive() {
        let zh = I18n::new(Lang::Zh);
        let en = I18n::new(Lang::En);
        assert_eq!(action_label(zh, "membership.create"), "创建");
        assert_eq!(resource_label(en, "tenant_membership"), "Membership");
        assert_eq!(result_label(zh, "denied"), "已拒绝");
        assert_eq!(action_label(en, "future.operation"), "future.operation");
        assert_eq!(resource_label(zh, "future_resource"), "future_resource");
    }
}
