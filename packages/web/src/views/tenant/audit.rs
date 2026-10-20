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
use dioxus::prelude::*;
use uuid::Uuid;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AuditFilter {
    actor_user_id: Option<Uuid>,
    request_id: Option<Uuid>,
    action: Option<String>,
    result: Option<String>,
}

fn optional_uuid(value: &str) -> Option<Option<Uuid>> {
    let value = value.trim();
    if value.is_empty() {
        return Some(None);
    }
    Uuid::parse_str(value)
        .ok()
        .filter(|id| !id.is_nil())
        .map(Some)
}

pub(crate) fn action_label(i18n: crate::i18n::I18n, action: &str) -> String {
    let exact = match action {
        "membership.grant" => Some("tenant.audit_action_grant"),
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

pub(crate) fn resource_label(i18n: crate::i18n::I18n, resource: &str) -> String {
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

pub(crate) fn result_label(i18n: crate::i18n::I18n, result: &str) -> String {
    match result {
        "success" => i18n.t("tenant.audit_result_success").to_owned(),
        "denied" => i18n.t("tenant.audit_result_denied").to_owned(),
        "failure" => i18n.t("tenant.audit_result_failure").to_owned(),
        _ => result.to_owned(),
    }
}

fn scope_label(i18n: crate::i18n::I18n, scope: &str) -> String {
    match scope {
        "platform" => i18n.t("tenant.audit_scope_platform").to_owned(),
        "tenant" => i18n.t("tenant.audit_scope_tenant").to_owned(),
        "user" => i18n.t("tenant.audit_scope_user").to_owned(),
        "system" => i18n.t("tenant.audit_scope_system").to_owned(),
        value => value.to_owned(),
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
    rsx! {
        if let Some(scope) = scope {
            for identity in [key] { TenantAuditPage { key: "{identity}", scope, embedded: false } }
        }
    }
}

#[component]
pub(crate) fn PlatformTenantAudit(scope: WorkspaceScope) -> Element {
    rsx! { TenantAuditPage { scope, embedded: true } }
}

#[component]
fn TenantAuditPage(scope: WorkspaceScope, embedded: bool) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut page = use_signal(|| 1u32);
    let mut actor_input = use_signal(String::new);
    let mut request_input = use_signal(String::new);
    let mut action_input = use_signal(String::new);
    let mut result_input = use_signal(String::new);
    let mut filter = use_signal(AuditFilter::default);
    let mut filter_error = use_signal(String::new);
    let mut data = use_resource(move || {
        let page = page();
        let filter_value = filter();
        let resource_key = (scope, filter_value.clone(), page);
        async move {
            let query = client_api::api::tenant_control::TenantAuditQuery {
                page,
                page_size: 20,
                actor_user_id: filter_value.actor_user_id,
                request_id: filter_value.request_id,
                action: filter_value.action,
                result: filter_value.result,
            };
            let result = common::read(auth, users, scope, move |token| {
                let query = query.clone();
                async move { scope.api()?.audit_filtered(&query, &token).await }
            })
            .await;
            KeyedResourceValue::new(resource_key, result)
        }
    });
    let loaded = current_keyed_value(&(scope, filter(), page()), data.state().cloned(), data());
    let apply_filter = move |event: Event<FormData>| {
        event.prevent_default();
        let Some(actor_user_id) = optional_uuid(&actor_input()) else {
            filter_error.set(i18n.t("tenant.audit_invalid_filter").to_owned());
            return;
        };
        let Some(request_id) = optional_uuid(&request_input()) else {
            filter_error.set(i18n.t("tenant.audit_invalid_filter").to_owned());
            return;
        };
        let action = (!action_input().trim().is_empty()).then(|| action_input().trim().to_owned());
        let result_value = result_input();
        let result = (!result_value.is_empty()).then_some(result_value);
        if action.as_ref().is_some_and(|value| value.len() > 128) {
            filter_error.set(i18n.t("tenant.audit_invalid_filter").to_owned());
            return;
        }
        filter_error.set(String::new());
        filter.set(AuditFilter {
            actor_user_id,
            request_id,
            action,
            result,
        });
        page.set(1);
    };
    rsx! {div {class:if embedded{"tenant-audit embedded-panel"}else{"page-container tenant-audit"},
        if !embedded {
            ui::PageHeader {title:i18n.t("tenant.audit").to_string(),description:i18n.t("tenant.audit_hint").to_string()}
            WorkspaceLinks {}
            WorkspaceContext {}
        } else {
            h2 { class: "section-title", {i18n.t("tenant.audit")} }
            p { class: "text-secondary", {i18n.t("tenant.audit_hint")} }
        }
        details { class:"card audit-filter-disclosure",
            summary { {i18n.t("tenant.audit_advanced_filters")} }
            form { class:"filter-panel", onsubmit:apply_filter,
                div { class:"filter-grid tenant-audit-filter-grid",
                    div { class:"form-field", label { class:"form-label", r#for:"tenant-audit-actor", {i18n.t("tenant.audit_actor_filter")} } input { id:"tenant-audit-actor", class:"input-field", maxlength:"36", value:"{actor_input}", oninput:move|event|actor_input.set(event.value()) } }
                    div { class:"form-field", label { class:"form-label", r#for:"tenant-audit-request", {i18n.t("tenant.audit_request_filter")} } input { id:"tenant-audit-request", class:"input-field", maxlength:"36", value:"{request_input}", oninput:move|event|request_input.set(event.value()) } }
                    div { class:"form-field", label { class:"form-label", r#for:"tenant-audit-action", {i18n.t("tenant.audit_action_filter")} } input { id:"tenant-audit-action", class:"input-field", maxlength:"128", value:"{action_input}", placeholder:"membership.update", oninput:move|event|action_input.set(event.value()) } }
                    div { class:"form-field", label { class:"form-label", r#for:"tenant-audit-result", {i18n.t("tenant.result")} } select { id:"tenant-audit-result", class:"input-field", value:"{result_input}", onchange:move|event|result_input.set(event.value()), option { value:"", {i18n.t("tenant.audit_all_results")} } option { value:"success", {i18n.t("tenant.audit_result_success")} } option { value:"denied", {i18n.t("tenant.audit_result_denied")} } option { value:"failure", {i18n.t("tenant.audit_result_failure")} } } }
                    div { class:"filter-actions",
                        button { class:"btn btn-primary", r#type:"submit", {i18n.t("tenant.audit_apply_filter")} }
                        button { class:"btn btn-secondary", r#type:"button", onclick:move |_|{actor_input.set(String::new());request_input.set(String::new());action_input.set(String::new());result_input.set(String::new());filter_error.set(String::new());filter.set(AuditFilter::default());page.set(1);},{i18n.t("tenant.audit_clear_filter")} }
                        button { class:"btn btn-secondary", r#type:"button", onclick:move |_|data.restart(), {i18n.t("tenant.reload")} }
                    }
                }
            }
        }
        if !filter_error().is_empty(){p{class:"alert alert-error",role:"alert","{filter_error}"}}
        match loaded {
            None=>rsx!{div {class:"content-loading",role:"status",span {class:"spinner",aria_hidden:"true"} span {{i18n.t("common.loading")}}}},
            Some(Err(e))=>rsx!{div {class:"alert alert-error",role:"alert",{user_error_message(i18n, &e)}}},
            Some(Ok(value))=>rsx!{
                div {class:"table-pagination-panel table-pagination-frame",
                    div {class:"table-container",tabindex:"0",table {class:"table",thead {tr {th {{i18n.t("tenant.time")}} th {{i18n.t("tenant.audit_scope")}} th {{i18n.t("tenant.actor")}} th {{i18n.t("tenant.action")}} th {{i18n.t("tenant.resource")}} th {{i18n.t("tenant.result")}}}}
                        tbody {for event in value.items.iter(){
                            {let request_id=event.request_id.map(|id|id.to_string()).unwrap_or_else(||"—".into());let metadata=serde_json::to_string_pretty(&event.metadata).unwrap_or_else(|_|event.metadata.to_string());let tenant_role=event.tenant_role.map(|role|role.as_str()).unwrap_or("—");let actor_label=event.actor_name.as_deref().or(event.actor_email.as_deref()).unwrap_or("—");rsx!{Fragment {key:"{event.id}",
                                tr {
                                    td {{format_time(&event.created_at)}}
                                    td {span {class:"badge",{scope_label(i18n,&event.scope_type)}}}
                                    td {
                                        strong { class:"table-primary", "{actor_label}" }
                                        if event.actor_name.is_some() { if let Some(email)=event.actor_email.as_deref() { span { class:"table-secondary", "{email}" } } }
                                        TechnicalId {value:event.actor_user_id.to_string()}
                                    }
                                    td {{action_label(i18n,&event.action)}}
                                    td {{resource_label(i18n,&event.resource_type)} if let Some(id)=event.resource_id.as_deref(){p{TechnicalId{value:id.to_string()}}}else{p{"—"}}}
                                    td {{result_label(i18n,&event.result)}}
                                }
                                tr {class:"audit-technical-row",
                                    td {colspan:"6",details {summary {{i18n.t("tenant.audit_technical_details")}}
                                        dl {class:"audit-detail-list",
                                            dt{{i18n.t("tenant.audit_event_code")}} dd{code {"{event.action}"}}
                                            dt{{i18n.t("tenant.audit_credential")}} dd{code {"{event.credential_kind}"}}
                                            dt{{i18n.t("tenant.audit_platform_role")}} dd{code {"{event.platform_role}"}}
                                            dt{{i18n.t("tenant.audit_tenant_role")}} dd{code {"{tenant_role}"}}
                                            dt{{i18n.t("tenant.audit_request_id")}} dd{code {"{request_id}"}}
                                            dt{{i18n.t("tenant.audit_metadata")}} dd{pre {"{metadata}"}}
                                        }
                                    }}
                                }
                            }
                            }}
                        }}
                    }}
                    if value.items.is_empty(){div {class:"empty-state compact-empty-state",h3 {class:"empty-title",{i18n.t("tenant.empty")}}}}
                }
                Pager {page:page(),total_pages:value.total_pages,total:value.total,on_page:move |p|page.set(p)}
            }
        }
    }}
}

#[cfg(test)]
mod tests {
    use super::{action_label, optional_uuid, resource_label, result_label, scope_label};
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
        assert_eq!(scope_label(zh, "platform"), "平台权限");
        assert_eq!(scope_label(en, "tenant"), "Tenant authority");
        assert_eq!(scope_label(zh, "user"), "用户权限");
        assert_eq!(scope_label(en, "system"), "System authority");
        assert_eq!(scope_label(en, "future"), "future");
    }

    #[test]
    fn audit_uuid_filter_accepts_empty_or_real_ids_only() {
        assert_eq!(optional_uuid("  "), Some(None));
        assert!(optional_uuid("00000000-0000-0000-0000-000000000001").is_some());
        assert_eq!(optional_uuid("00000000-0000-0000-0000-000000000000"), None);
        assert_eq!(optional_uuid("not-a-uuid"), None);
    }
}
