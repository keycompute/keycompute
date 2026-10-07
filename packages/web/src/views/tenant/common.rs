use crate::{
    hooks::use_i18n::use_i18n,
    router::Route,
    services::api_client::{get_client, with_auto_refresh},
    stores::{
        auth_store::AuthStore,
        ui_store::UiStore,
        user_store::{UserInfo, UserStore},
    },
    utils::display::short_id,
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

/// Gate pages whose APIs require a verified selected tenant. Global sessions
/// remain valid and are sent to the existing workspace/operations entrypoints;
/// the child page is not mounted, so no tenant-scoped resource is started.
#[component]
pub fn TenantRequiredPage(children: Element) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let state = (auth.state)();

    if state.is_authenticated && *users.loaded_session_id.peek() != state.session_id {
        return rsx! {
            div { class: "auth-redirect-loading", role: "status",
                {i18n.t("common.loading")}
            }
        };
    }

    if let Some(scope) = WorkspaceScope::from_stores(auth, users) {
        // A workspace switch must unmount every child resource and local form
        // state. Dioxus otherwise retains the previous resource value while the
        // new request is pending, which can expose stale tenant data briefly.
        let scope_key = format!("{scope:?}");
        return rsx! {
            TenantScopedChildren { key: "{scope_key}", children }
        };
    }

    let can_view_operations = users
        .info
        .read()
        .as_ref()
        .is_some_and(UserInfo::can_view_operations);
    rsx! {
        div { class: "page-container", role: "alert",
            h1 { class: "page-title", {i18n.t("tenant.workspace")} }
            p { class: "alert alert-info", {i18n.t("tenant.selection_required")} }
            nav { class: "toolbar", aria_label: i18n.t("tenant.workspace"),
                Link { class: "btn btn-primary", to: Route::TenantWorkspace {}, {i18n.t("tenant.workspace")} }
                if can_view_operations {
                    Link { class: "btn btn-secondary", to: Route::PlatformOperations {}, {i18n.t("operations.title")} }
                }
            }
        }
    }
}

/// Key-only boundary used by [`TenantRequiredPage`] without adding a DOM node.
#[component]
fn TenantScopedChildren(children: Element) -> Element {
    children
}

#[component]
pub fn WorkspaceLinks() -> Element {
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let route = use_route::<Route>();
    let admin = users
        .info
        .read()
        .as_ref()
        .is_some_and(|u| u.can_manage_tenant());
    let overview_active = route == (Route::TenantWorkspace {});
    let issuance_active = route == (Route::OwnerKeyIssuance {});
    let settings_active = route == (Route::TenantSettings {});
    rsx! {nav {class:"tenant-workspace-links", aria_label:i18n.t("tenant.quick_links"),
        span {class:"tenant-workspace-links-label",{i18n.t("tenant.quick_links")}}
        Link {
            class: if overview_active {"tenant-workspace-link active"} else {"tenant-workspace-link"},
            aria_current: if overview_active {"page"} else {"false"},
            to:Route::TenantWorkspace {},
            {i18n.t("tenant.workspace")}
        }
        Link {
            class: if issuance_active {"tenant-workspace-link active"} else {"tenant-workspace-link"},
            aria_current: if issuance_active {"page"} else {"false"},
            to:Route::OwnerKeyIssuance {},
            {i18n.t("tenant_keys.my_requests")}
        }
        if admin {
            Link {
                class: if settings_active {"tenant-workspace-link active"} else {"tenant-workspace-link"},
                aria_current: if settings_active {"page"} else {"false"},
                to:Route::TenantSettings {},
                {i18n.t("tenant.settings")}
            }
        }
    }}
}

/// Compact, human-readable tenant scope. The workspace name and role are the
/// primary information; the technical UUID stays available in a disclosure
/// for support and audit workflows without dominating every page.
#[component]
pub fn WorkspaceContext(#[props(default)] note: String) -> Element {
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let Some(tenant) = users
        .info
        .read()
        .as_ref()
        .and_then(|user| user.selected_tenant.clone())
    else {
        return rsx! {};
    };
    let name = tenant
        .name
        .as_deref()
        .or(tenant.slug.as_deref())
        .unwrap_or(tenant.id.as_str())
        .to_owned();
    let role = match tenant.tenant_role {
        client_api::TenantRole::Admin => i18n.t("tenant.role_admin"),
        client_api::TenantRole::Member => i18n.t("tenant.role_member"),
    };
    let abbreviated = short_id(&tenant.id);

    rsx! {
        aside { class: "tenant-context-bar", aria_label: i18n.t("tenant.current_context"),
            div { class: "tenant-context-identity",
                span { class: "tenant-context-label", {i18n.t("tenant.current")} }
                strong { "{name}" }
                span { class: "badge tenant-context-role", "{role}" }
                details { class: "tenant-context-id",
                    summary { "{i18n.t(\"tenant.workspace_id\")} · {abbreviated}" }
                    code { title: "{tenant.id}", "{tenant.id}" }
                }
            }
            if !note.trim().is_empty() {
                p { "{note}" }
            }
        }
    }
}

pub fn workspace_name(users: UserStore, scope: WorkspaceScope) -> String {
    users
        .info
        .peek()
        .as_ref()
        .and_then(|user| user.selected_tenant.as_ref())
        .filter(|tenant| tenant.id == scope.tenant_id.to_string())
        .and_then(|tenant| tenant.name.as_deref().or(tenant.slug.as_deref()))
        .map(str::to_owned)
        .unwrap_or_else(|| short_id(&scope.tenant_id.to_string()))
}

/// Keep opaque identifiers available for support workflows without allowing
/// them to dominate ordinary tables and summaries.
#[component]
pub fn TechnicalId(value: String) -> Element {
    rsx! {
        code {
            class: "technical-id",
            tabindex: "0",
            title: "{value}",
            aria_label: "{value}",
            "{value}"
        }
    }
}

/// Member ID input with tenant-scoped, active-member suggestions. UUID entry
/// remains available for large tenants whose member is outside the first page,
/// while the common case no longer requires copying an opaque identifier.
#[component]
pub fn MemberIdField(
    scope: WorkspaceScope,
    input_id: String,
    label: String,
    value: String,
    on_input: EventHandler<String>,
    #[props(default = false)] disabled: bool,
) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let suggestions = use_resource(move || {
        let can_list_members = users
            .info
            .read()
            .as_ref()
            .is_some_and(UserInfo::can_manage_members);
        async move {
            if !can_list_members {
                return Ok(None);
            }
            read(auth, users, scope, move |token| async move {
                scope.api()?.members(1, 100, None, &token).await
            })
            .await
            .map(Some)
        }
    });
    let list_id = format!("{input_id}-options");

    rsx! {
        div { class: "form-field member-id-field",
            label { class: "form-label", r#for: "{input_id}", "{label}" }
            input {
                id: "{input_id}",
                class: "input-field",
                list: "{list_id}",
                maxlength: "36",
                value: "{value}",
                disabled,
                placeholder: i18n.t("tenant.member_id_placeholder"),
                oninput: move |event| on_input.call(event.value()),
            }
            datalist { id: "{list_id}",
                if let Some(Ok(Some(page))) = suggestions() {
                    for member in page.items.iter().filter(|member| {
                        member.membership_status == client_api::MembershipStatus::Active
                            && member.user_status == client_api::UserStatus::Active
                    }) {
                        option { value: "{member.user_id}", "{member.email}" }
                    }
                }
            }
            small { class: "form-hint", {i18n.t("tenant.member_id_hint")} }
        }
    }
}
#[component]
pub fn Pager(page: u32, total_pages: i64, total: i64, on_page: EventHandler<u32>) -> Element {
    let i18n = use_i18n();
    if total <= 0 && page <= 1 {
        return rsx! {};
    }
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
