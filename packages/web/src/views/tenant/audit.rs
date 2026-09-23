use super::common::{self, Pager, WorkspaceLinks, WorkspaceScope};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::user_error_message,
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::ClientError;
use dioxus::prelude::*;
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
        button {class:"btn btn-secondary",onclick:move |_|data.restart(),{i18n.t("tenant.reload")}}
        match loaded {
            None=>rsx!{p {role:"status",{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{div {class:"alert alert-error",role:"alert",{user_error_message(&e)}}},
            Some(Ok(value))=>rsx!{
                div {class:"table-pagination-panel",
                    table {class:"table",thead {tr {th {{i18n.t("tenant.time")}} th {{i18n.t("tenant.actor")}} th {{i18n.t("tenant.action")}} th {{i18n.t("tenant.resource")}} th {{i18n.t("tenant.result")}}}}
                        tbody {for event in value.items.iter(){tr {key:"{event.id}",
                            td {"{event.created_at}"}
                            td {"{event.actor_user_id}"}
                            td {"{event.action}"}
                            td {"{event.resource_type}" p {{event.resource_id.as_deref().unwrap_or("—")}}}
                            td {"{event.result}" details {summary {"Request ID"} code {"{event.request_id.map(|id|id.to_string()).unwrap_or_default()}"} pre {"{event.metadata}"}}}
                        }}}
                    }
                    if value.items.is_empty(){p {{i18n.t("tenant.empty")}}}
                }
                Pager {page:page(),total_pages:value.total_pages,total:value.total,on_page:move |p|page.set(p)}
            }
        }
    }}
}
