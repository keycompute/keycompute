use crate::{
    hooks::use_i18n::use_i18n,
    router::Route,
    services::api_client::{get_client, with_auto_refresh},
    stores::{auth_store::AuthStore, ui_store::UiStore, user_store::UserStore},
};
use client_api::{ApiClient, ClientError, Result, TenantControlApi};
use dioxus::prelude::*;
use std::future::Future;
use uuid::Uuid;

/// Identity of a rendered result, never an authorization grant.
/// Revisions invalidate data on same-session membership changes; token refresh alone does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceScope {
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub tenant_version: i64,
    pub member_version: i64,
}
impl WorkspaceScope {
    fn from_profile(
        state: &crate::stores::auth_store::AuthState,
        loaded_session: Uuid,
        info: Option<&crate::stores::user_store::UserInfo>,
    ) -> Option<Self> {
        if !state.is_authenticated || loaded_session != state.session_id {
            return None;
        }
        let info = info?;
        let tenant = info.selected_tenant.as_ref()?;
        // Restored opaque credentials have no locally decoded tenant. The verified
        // server profile is authoritative; an explicit conflicting selection is not.
        if state
            .selected_tenant_id
            .as_deref()
            .is_some_and(|id| id != tenant.id)
        {
            return None;
        }
        let real = |value: &str| Uuid::parse_str(value).ok().filter(|id| !id.is_nil());
        let tenant_version = tenant.authz_version.filter(|v| *v > 0)?;
        let member_version = tenant.membership_authz_version.filter(|v| *v > 0)?;
        Some(Self {
            session_id: state.session_id,
            tenant_id: real(&tenant.id)?,
            user_id: real(&info.id)?,
            tenant_version,
            member_version,
        })
    }
    pub fn from_stores(auth: AuthStore, users: UserStore) -> Option<Self> {
        Self::from_profile(
            &(auth.state)(),
            (users.loaded_session_id)(),
            (users.info)().as_ref(),
        )
    }
    pub fn is_current(self, auth: AuthStore, users: UserStore) -> bool {
        Self::from_profile(
            &auth.state.peek(),
            *users.loaded_session_id.peek(),
            users.info.peek().as_ref(),
        ) == Some(self)
    }
    pub fn api(self) -> Result<TenantControlApi> {
        self.api_with(&get_client())
    }
    fn api_with(self, client: &ApiClient) -> Result<TenantControlApi> {
        TenantControlApi::new(client, self.tenant_id)
    }
}
fn changed() -> ClientError {
    ClientError::Other("Workspace changed; the old result was discarded".into())
}
pub async fn read<T, F, Fut>(
    auth: AuthStore,
    users: UserStore,
    scope: WorkspaceScope,
    f: F,
) -> Result<T>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    if !scope.is_current(auth, users) {
        return Err(changed());
    }
    let result = with_auto_refresh(auth, f).await;
    if !scope.is_current(auth, users) {
        return Err(changed());
    }
    result
}
/// No auth-refresh or network replay for a control mutation with an unknown outcome.
pub async fn command<T, F, Fut>(
    auth: AuthStore,
    users: UserStore,
    scope: WorkspaceScope,
    f: F,
) -> Result<T>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    if !scope.is_current(auth, users) {
        return Err(changed());
    }
    let token = auth.state.peek().access_token.clone().ok_or_else(changed)?;
    let result = f(token).await;
    if !scope.is_current(auth, users) {
        return Err(changed());
    }
    result
}
pub fn invalidate_own_session(
    mut auth: AuthStore,
    mut users: UserStore,
    scope: WorkspaceScope,
    mut ui: UiStore,
    message: &str,
) {
    if scope.is_current(auth, users) {
        auth.logout();
        users.clear();
        ui.show_success(message);
    }
}
#[component]
pub fn WorkspaceLinks() -> Element {
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let admin = users
        .info
        .read()
        .as_ref()
        .is_some_and(|u| u.can_manage_tenant());
    rsx! {nav {class:"toolbar tenant-workspace-links", aria_label:i18n.t("tenant.workspace"),
        Link {class:"btn btn-secondary",to:Route::TenantWorkspace {},{i18n.t("tenant.workspace")}}
        Link {class:"btn btn-secondary",to:Route::OwnerKeyIssuance {},{i18n.t("tenant_keys.my_requests")}}
        if admin {
            Link {class:"btn btn-secondary",to:Route::TenantKeys {},{i18n.t("tenant_keys.title")}}
            Link {class:"btn btn-secondary",to:Route::TenantResponses {},{i18n.t("tenant_responses.title")}}
            Link {class:"btn btn-secondary",to:Route::TenantPricing {},{i18n.t("tenant_pricing.title")}}
            Link {class:"btn btn-secondary",to:Route::TenantNodes {},{i18n.t("tenant_nodes.title")}}
            Link {class:"btn btn-secondary",to:Route::TenantMembers {},{i18n.t("tenant.members")}}
            Link {class:"btn btn-secondary",to:Route::TenantInvitations {},{i18n.t("tenant.invitations")}}
            Link {class:"btn btn-secondary",to:Route::TenantAudit {},{i18n.t("tenant.audit")}}
        }
    }}
}
#[component]
pub fn Pager(page: u32, total_pages: i64, total: i64, on_page: EventHandler<u32>) -> Element {
    let i18n = use_i18n();
    rsx! {div {class:"table-pagination-footer",
        button {class:"btn btn-secondary",r#type:"button",disabled:page<=1,onclick:move |_|on_page.call(page-1),{i18n.t("tenant.previous")}}
        span { "{page} / {total_pages.max(1)} · {total} " {i18n.t("tenant.records")} }
        button {class:"btn btn-secondary",r#type:"button",disabled:i64::from(page)>=total_pages,onclick:move |_|on_page.call(page.saturating_add(1)),{i18n.t("tenant.next")}}
    }}
}
#[component]
pub fn CommandDialog(
    title: String,
    target: String,
    busy: bool,
    on_cancel: EventHandler<()>,
    on_confirm: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    rsx! {div {class:"modal-backdrop",
        div {class:"modal",role:"dialog",aria_modal:"true",aria_label:title.clone(),
            div {class:"modal-header",h2 {"{title}"}}
            div {class:"modal-body",p {"{target}"} p {class:"text-secondary",{i18n.t("tenant.command_hint")}}}
            div {class:"modal-footer",
                button {class:"btn btn-secondary",disabled:busy,onclick:move |_|on_cancel.call(()),{i18n.t("form.cancel")}}
                button {class:"btn btn-primary",disabled:busy,onclick:move |_|on_confirm.call(()),{i18n.t("tenant.confirm")}}
            }
        }
    }}
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "common_tests.rs"]
mod tests;
