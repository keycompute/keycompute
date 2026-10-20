use crate::{
    hooks::use_i18n::use_i18n,
    router::Route,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, ui_store::UiStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
    views::shared::accounts::NoPermissionView,
    views::tenant::{
        PlatformTenantAudit as TenantAuditPanel, PlatformTenantFinance as TenantFinancePanel,
        PlatformTenantFinancialControls as TenantFinancialControlsPanel,
        PlatformTenantKeys as TenantKeysPanel, PlatformTenantSettings as TenantSettingsPanel,
        common::{self, Pager, TechnicalId, WorkspaceScope},
        workspace_switcher::use_workspace_dirty_blocker,
    },
};
use client_api::{
    AdminApi, MembershipStatus, PlatformRole, TenantRole, UserStatus,
    api::admin::{UserDetail, UserListResponse, UserQueryParams},
    api::tenant_control::{GrantMember, MemberPatch, Page, TenantMember},
};
use dioxus::prelude::*;
use uuid::Uuid;

const PAGE_SIZE: u32 = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
struct AccessQuery {
    member_page: u32,
    member_search: String,
}

impl Default for AccessQuery {
    fn default() -> Self {
        Self {
            member_page: 1,
            member_search: String::new(),
        }
    }
}

impl AccessQuery {
    fn set_member_page(&mut self, page: u32) {
        self.member_page = page.max(1);
    }

    fn set_member_search(&mut self, search: &str) {
        self.member_search = search.trim().to_owned();
        self.member_page = 1;
    }
}

#[derive(Clone)]
struct AccessData {
    active_members: i64,
    members: Page<TenantMember>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PlatformTenantSection {
    #[default]
    Access,
    Settings,
    Keys,
    Finance,
    FinancialControls,
    Audit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MemberAction {
    Role,
    Status,
    Remove,
}

impl MemberAction {
    fn label(self) -> &'static str {
        match self {
            Self::Role => "tenant.change_role",
            Self::Status => "tenant.change_status",
            Self::Remove => "tenant.remove",
        }
    }
}

fn tenant_role_label(i18n: crate::i18n::I18n, role: TenantRole) -> &'static str {
    i18n.t(match role {
        TenantRole::Admin => "tenant.role_admin",
        TenantRole::Member => "tenant.role_member",
    })
}

fn membership_status_label(i18n: crate::i18n::I18n, status: MembershipStatus) -> &'static str {
    i18n.t(match status {
        MembershipStatus::Active => "tenant.status_active",
        MembershipStatus::Suspended => "tenant.status_suspended",
        MembershipStatus::Removed => "tenant.status_removed",
    })
}

fn user_status_label(i18n: crate::i18n::I18n, status: UserStatus) -> &'static str {
    i18n.t(match status {
        UserStatus::Active => "tenant.status_active",
        UserStatus::Suspended => "tenant.status_suspended",
    })
}

#[component]
pub fn PlatformTenantAccess(tenant_id: String) -> Element {
    rsx! { PlatformTenantRoutePage { tenant_id, section: PlatformTenantSection::Access } }
}

#[component]
pub fn PlatformTenantSettings(tenant_id: String) -> Element {
    rsx! { PlatformTenantRoutePage { tenant_id, section: PlatformTenantSection::Settings } }
}

#[component]
pub fn PlatformTenantKeys(tenant_id: String) -> Element {
    rsx! { PlatformTenantRoutePage { tenant_id, section: PlatformTenantSection::Keys } }
}

#[component]
pub fn PlatformTenantFinance(tenant_id: String) -> Element {
    rsx! { PlatformTenantRoutePage { tenant_id, section: PlatformTenantSection::Finance } }
}

#[component]
pub fn PlatformTenantFinancialControls(tenant_id: String) -> Element {
    rsx! { PlatformTenantRoutePage { tenant_id, section: PlatformTenantSection::FinancialControls } }
}

#[component]
pub fn PlatformTenantAudit(tenant_id: String) -> Element {
    rsx! { PlatformTenantRoutePage { tenant_id, section: PlatformTenantSection::Audit } }
}

#[component]
fn PlatformTenantRoutePage(tenant_id: String, section: PlatformTenantSection) -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let can_manage = users
        .info
        .read()
        .as_ref()
        .is_some_and(|user| user.can_manage_platform());
    if !can_manage {
        return rsx! { NoPermissionView { resource: i18n.t("tenants.access_title").to_string() } };
    }
    let Ok(target) = Uuid::parse_str(&tenant_id) else {
        return rsx! { div { class: "page-container", div { class: "alert alert-error", {i18n.t("common.invalid_request")} } } };
    };
    let Some(scope) = WorkspaceScope::platform(auth, users, target) else {
        return rsx! { NoPermissionView { resource: i18n.t("tenants.access_title").to_string() } };
    };
    let identity = format!("{}:{scope:?}", scope.session_id);
    rsx! {
        for key in [identity] {
            PlatformTenantScopedPage { key: "{key}", scope, section }
        }
    }
}

#[component]
fn PlatformTenantScopedPage(scope: WorkspaceScope, section: PlatformTenantSection) -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut context = use_resource(move || async move {
        let result = common::read(auth, users, scope, move |token| async move {
            scope.api()?.context(&token).await
        })
        .await;
        KeyedResourceValue::new(scope, result)
    });
    let loaded_context = current_keyed_value(&scope, context.state().cloned(), context());
    let tenant_id = scope.tenant_id.to_string();
    let title = loaded_context
        .as_ref()
        .and_then(|value| value.as_ref().ok())
        .map(|value| value.name.clone())
        .unwrap_or_else(|| i18n.t("tenants.access_title").to_owned());
    let tab_class = |active| {
        if active {
            "btn btn-secondary active"
        } else {
            "btn btn-secondary"
        }
    };
    rsx! {
        div { class: "page-container platform-tenant-access",
            ui::PageHeader {
                title,
                description: i18n.t("tenants.access_hint").to_owned(),
                actions: rsx! { Link { class: "btn btn-secondary", to: Route::Tenants {}, {i18n.t("common.back")} } },
            }
            div { class: "scope-banner platform-tenant-scope",
                span { class: "scope-banner-label", {i18n.t("tenants.platform_managed_mode")} }
                if let Some(Ok(value)) = loaded_context.as_ref() {
                    strong { "{value.name}" }
                    TechnicalId { value: value.id.to_string() }
                    span { class: "badge", {if value.status == "active" { i18n.t("tenant.status_active") } else { i18n.t("common.disabled") }} }
                } else {
                    TechnicalId { value: tenant_id.clone() }
                }
            }
            nav { class: "segmented-control platform-tenant-tabs", aria_label: i18n.t("tenants.access_title"),
                Link { class: tab_class(section == PlatformTenantSection::Access), aria_current: if section == PlatformTenantSection::Access { "page" } else { "false" }, to: Route::PlatformTenantAccess { tenant_id: tenant_id.clone() }, {i18n.t("tenants.access_title")} }
                Link { class: tab_class(section == PlatformTenantSection::Settings), aria_current: if section == PlatformTenantSection::Settings { "page" } else { "false" }, to: Route::PlatformTenantSettings { tenant_id: tenant_id.clone() }, {i18n.t("tenants.settings_title")} }
                Link { class: tab_class(section == PlatformTenantSection::Keys), aria_current: if section == PlatformTenantSection::Keys { "page" } else { "false" }, to: Route::PlatformTenantKeys { tenant_id: tenant_id.clone() }, {i18n.t("tenant_keys.title")} }
                Link { class: tab_class(section == PlatformTenantSection::Finance), aria_current: if section == PlatformTenantSection::Finance { "page" } else { "false" }, to: Route::PlatformTenantFinance { tenant_id: tenant_id.clone() }, {i18n.t("tenant_finance.title")} }
                Link { class: tab_class(section == PlatformTenantSection::FinancialControls), aria_current: if section == PlatformTenantSection::FinancialControls { "page" } else { "false" }, to: Route::PlatformTenantFinancialControls { tenant_id: tenant_id.clone() }, {i18n.t("tenant_financial_controls.title")} }
                Link { class: tab_class(section == PlatformTenantSection::Audit), aria_current: if section == PlatformTenantSection::Audit { "page" } else { "false" }, to: Route::PlatformTenantAudit { tenant_id }, {i18n.t("tenant.audit")} }
            }
            match loaded_context {
                None => rsx! { div { class: "content-loading", role: "status", span { class: "spinner", aria_hidden: "true" } span { {i18n.t("common.loading")} } } },
                Some(Err(value)) => rsx! { div { class: "alert alert-error", role: "alert", {user_error_message(i18n, &value)} } },
                Some(Ok(value)) => match section {
                    PlatformTenantSection::Access => rsx! { PlatformTenantAccessPanel { scope, tenant_id: value.id, on_context_changed: move |_| context.restart() } },
                    PlatformTenantSection::Settings => rsx! { TenantSettingsPanel { scope, on_context_changed: move |_| context.restart() } },
                    PlatformTenantSection::Keys => rsx! { TenantKeysPanel { scope } },
                    PlatformTenantSection::Finance => rsx! { TenantFinancePanel { scope } },
                    PlatformTenantSection::FinancialControls => rsx! { TenantFinancialControlsPanel { scope } },
                    PlatformTenantSection::Audit => rsx! { TenantAuditPanel { scope } },
                },
            }
        }
    }
}

#[component]
fn PlatformTenantAccessPanel(
    scope: WorkspaceScope,
    tenant_id: Uuid,
    on_context_changed: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut ui = use_context::<UiStore>();
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut pending_member = use_signal(|| None::<(TenantMember, MemberAction)>);
    let mut grant_search_input = use_signal(String::new);
    let mut grant_search = use_signal(String::new);
    let mut grant_selected = use_signal(|| None::<UserDetail>);
    let mut member_search_input = use_signal(String::new);
    let mut query = use_signal(AccessQuery::default);
    let mut data = use_resource(move || async move {
        let request = query();
        let key = (scope, request.clone());
        let result = common::read(auth, users, scope, move |token| {
            let request = request.clone();
            async move {
                let api = scope.api()?;
                let (active_page, members) = futures::try_join!(
                    api.members_filtered(1, 1, None, Some(MembershipStatus::Active), &token),
                    api.members(
                        request.member_page,
                        PAGE_SIZE,
                        Some(&request.member_search),
                        &token,
                    ),
                )?;
                Ok(AccessData {
                    active_members: active_page.total,
                    members,
                })
            }
        })
        .await;
        KeyedResourceValue::new(key, result)
    });
    let current_key = (scope, query());
    let loaded = current_keyed_value(&current_key, data.state().cloned(), data());
    let grant_request_key = grant_search();
    let mut grant_candidates = use_resource(move || {
        let search = grant_search();
        async move {
            if search.is_empty() {
                return KeyedResourceValue::new(search, Ok(None::<UserListResponse>));
            }
            let request_key = search.clone();
            let result = common::read(auth, users, scope, move |token| {
                let params = UserQueryParams::new()
                    .with_platform_role(PlatformRole::None)
                    .with_status(UserStatus::Active)
                    .only_tenant_unassigned()
                    .with_search(search.clone())
                    .with_page(1)
                    .with_page_size(10);
                async move {
                    AdminApi::new(&get_client())
                        .list_all_users(Some(&params), &token)
                        .await
                        .map(Some)
                }
            })
            .await;
            KeyedResourceValue::new(request_key, result)
        }
    });
    let loaded_candidates = current_keyed_value(
        &grant_request_key,
        grant_candidates.state().cloned(),
        grant_candidates(),
    );
    use_workspace_dirty_blocker(move || {
        busy() || pending_member().is_some() || grant_selected().is_some()
    });

    let grant_existing_user = move |_| {
        if busy() {
            return;
        }
        let Some(selected) = grant_selected() else {
            error.set(i18n.t("tenants.select_user_first").to_owned());
            return;
        };
        let Ok(user_id) = Uuid::parse_str(&selected.id) else {
            error.set(i18n.t("common.invalid_request").to_owned());
            return;
        };
        let body = GrantMember { user_id };
        busy.set(true);
        error.set(String::new());
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| {
                let body = body.clone();
                async move { scope.api()?.grant_member(&body, &token).await }
            })
            .await;
            if !scope.is_current(auth, users) {
                return;
            }
            busy.set(false);
            match result {
                Ok(_) => {
                    grant_selected.set(None);
                    grant_search_input.set(String::new());
                    grant_search.set(String::new());
                    data.restart();
                    on_context_changed.call(());
                    ui.show_success(i18n.t("tenant.saved"));
                }
                Err(value) => {
                    error.set(user_error_message(i18n, &value));
                    data.restart();
                }
            }
        });
    };

    let confirm_member = move |_| {
        if busy() {
            return;
        }
        let Some((member, action)) = pending_member() else {
            return;
        };
        busy.set(true);
        error.set(String::new());
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| {
                let member = member.clone();
                async move {
                    let api = scope.api()?;
                    match action {
                        MemberAction::Remove => {
                            api.remove_member(member.user_id, member.authz_version, &token)
                                .await?;
                        }
                        MemberAction::Role => {
                            api.patch_member(
                                member.user_id,
                                &MemberPatch {
                                    expected_authz_version: member.authz_version,
                                    tenant_role: Some(if member.tenant_role == TenantRole::Admin {
                                        TenantRole::Member
                                    } else {
                                        TenantRole::Admin
                                    }),
                                    status: None,
                                },
                                &token,
                            )
                            .await?;
                        }
                        MemberAction::Status => {
                            api.patch_member(
                                member.user_id,
                                &MemberPatch {
                                    expected_authz_version: member.authz_version,
                                    tenant_role: None,
                                    status: Some(
                                        if member.membership_status == MembershipStatus::Active {
                                            MembershipStatus::Suspended
                                        } else {
                                            MembershipStatus::Active
                                        },
                                    ),
                                },
                                &token,
                            )
                            .await?;
                        }
                    }
                    Ok(())
                }
            })
            .await;
            if !scope.is_current(auth, users) {
                return;
            }
            busy.set(false);
            pending_member.set(None);
            match result {
                Ok(()) => {
                    data.restart();
                    on_context_changed.call(());
                    ui.show_success(i18n.t("tenant.saved"));
                }
                Err(value) => {
                    error.set(user_error_message(i18n, &value));
                    data.restart();
                }
            }
        });
    };

    rsx! {div {class:"platform-tenant-access-panel",
            if !error().is_empty() { div { class: "alert alert-error", role: "alert", "{error}" p { {i18n.t("tenant.command_hint")} } } }
            match loaded {
                None => rsx! { div { class: "content-loading", role: "status", span { class: "spinner", aria_hidden: "true" } span { {i18n.t("common.loading")} } } },
                Some(Err(value)) => rsx! { div { class: "alert alert-error", role: "alert", {user_error_message(i18n, &value)} } },
                Some(Ok(value)) => rsx! {
                    section { class: "section",
                        h2 { class: "section-title", {i18n.t("tenants.access_summary")} }
                        div { class: "section-body",
                            dl { class: "tenant-overview-facts",
                                div { dt { {i18n.t("tenants.tenant_id")} } dd { TechnicalId { value: tenant_id.to_string() } } }
                                div { dt { {i18n.t("tenants.active_members")} } dd { "{value.active_members}" } }
                            }
                        }
                    }
                    section { class: "section tenant-direct-grant-card",
                        h2 { class: "section-title", {i18n.t("tenants.direct_grant_title")} }
                        div { class: "section-body",
                            p { class: "text-secondary", {i18n.t("tenants.direct_grant_hint")} }
                            form { class: "tenant-user-search", onsubmit: move |event: Event<FormData>| {
                                event.prevent_default();
                                grant_selected.set(None);
                                let next = grant_search_input().trim().to_owned();
                                if grant_search() == next { grant_candidates.restart(); } else { grant_search.set(next); }
                            },
                                div { class: "form-field filter-field-grow",
                                    label { class: "form-label", r#for: "platform-grant-search", {i18n.t("tenants.search_global_user")} }
                                    input { id: "platform-grant-search", class: "input-field", r#type: "search", maxlength: "255", value: "{grant_search_input}", disabled: busy(), placeholder: i18n.t("users.search_placeholder"), oninput: move |event| grant_search_input.set(event.value()) }
                                }
                                button { class: "btn btn-secondary", r#type: "submit", disabled: busy() || grant_search_input().trim().is_empty(), {i18n.t("tenant.search")} }
                            }
                            match loaded_candidates.as_ref() {
                                None if !grant_request_key.is_empty() => rsx! { div { class: "content-loading", role: "status", span { class: "spinner", aria_hidden: "true" } span { {i18n.t("common.loading")} } } },
                                Some(Err(value)) => rsx! { div { class: "alert alert-error", role: "alert", {user_error_message(i18n, value)} } },
                                Some(Ok(Some(result))) if result.users.is_empty() => rsx! { div { class: "empty-state compact-empty-state", h3 { class: "empty-title", {i18n.t("tenants.no_global_user_matches")} } } },
                                Some(Ok(Some(result))) => rsx! { div { class: "tenant-user-results", role: "listbox", aria_label: i18n.t("tenants.search_global_user"),
                                    for candidate in result.users.iter() {
                                        {let selected=candidate.clone();let is_selected=grant_selected().as_ref().is_some_and(|value|value.id==candidate.id);rsx!{
                                            button { key: "{candidate.id}", class: if is_selected { "tenant-user-option selected" } else { "tenant-user-option" }, r#type: "button", role: "option", aria_selected: is_selected, disabled: busy(), onclick: move |_| grant_selected.set(Some(selected.clone())),
                                                span { strong { "{candidate.name.as_deref().unwrap_or(&candidate.email)}" } if candidate.name.is_some() { span { class: "text-secondary", "{candidate.email}" } } }
                                                TechnicalId { value: candidate.id.clone() }
                                            }
                                        }}
                                    }
                                } },
                                _ => rsx! {},
                            }
                            if let Some(selected) = grant_selected() {
                                div { class: "tenant-grant-editor",
                                    div { class: "selected-identity",
                                        strong { "{selected.name.as_deref().unwrap_or(&selected.email)}" }
                                        if selected.name.is_some() { span { class: "text-secondary", "{selected.email}" } }
                                    }
                                    span { class: "badge", {i18n.t("tenant.role_admin")} }
                                    button { class: "btn btn-primary", r#type: "button", disabled: busy(), onclick: grant_existing_user, {i18n.t("tenants.direct_grant_action")} }
                                }
                            }
                        }
                    }
                    section { class: "section",
                        h2 { class: "section-title", {i18n.t("tenant.members")} }
                        form { class: "card filter-panel tenant-member-filter", onsubmit: move |event: Event<FormData>| {
                            event.prevent_default();
                            let next = member_search_input().trim().to_owned();
                            let current = query();
                            if current.member_search == next && current.member_page == 1 {
                                data.restart();
                            } else {
                                query.write().set_member_search(&next);
                            }
                        },
                            div { class: "filter-grid filter-grid-health",
                                div { class: "form-field filter-field-grow",
                                    label { class: "form-label", r#for: "platform-member-search", {i18n.t("tenant.search")} }
                                    input { id: "platform-member-search", class: "input-field", r#type: "search", maxlength: "255", value: "{member_search_input}", disabled: busy(), oninput: move |event| member_search_input.set(event.value()) }
                                }
                                div { class: "filter-actions",
                                    button { class: "btn btn-primary", r#type: "submit", disabled: busy(), {i18n.t("tenant.search")} }
                                    button { class: "btn btn-secondary", r#type: "button", disabled: busy(), onclick: move |_| { data.restart(); on_context_changed.call(()); }, {i18n.t("tenant.reload")} }
                                }
                            }
                        }
                        div { class: "table-pagination-panel table-pagination-frame",
                            div { class: "table-container", table { class: "table",
                                thead { tr { th { {i18n.t("tenant.user")} } th { {i18n.t("tenant.role")} } th { {i18n.t("tenant.membership_status")} } th { {i18n.t("tenant.user_status")} } th { {i18n.t("tenant.actions")} } } }
                                tbody { for member in value.members.items.iter() {
                                    {let role_member=member.clone();let status_member=member.clone();let remove_member=member.clone();let removed=member.membership_status==MembershipStatus::Removed;rsx!{
                                        tr { key: "{member.user_id}",
                                            td { "{member.email}" }
                                            td { {tenant_role_label(i18n, member.tenant_role)} }
                                            td { {membership_status_label(i18n, member.membership_status)} }
                                            td { {user_status_label(i18n, member.user_status)} }
                                            td { div { class: "table-actions",
                                                if !removed {
                                                    button { class: "btn btn-secondary btn-sm", disabled: busy(), onclick: move |_| pending_member.set(Some((role_member.clone(), MemberAction::Role))), {i18n.t("tenant.change_role")} }
                                                    button { class: "btn btn-secondary btn-sm", disabled: busy(), onclick: move |_| pending_member.set(Some((status_member.clone(), MemberAction::Status))), {i18n.t("tenant.change_status")} }
                                                    button { class: "btn btn-danger btn-sm", disabled: busy(), onclick: move |_| pending_member.set(Some((remove_member.clone(), MemberAction::Remove))), {i18n.t("tenant.remove")} }
                                                }
                                            } }
                                        }
                                    }}
                                } }
                            } }
                            if value.members.items.is_empty() { div { class: "empty-state compact-empty-state", h3 { class: "empty-title", {if query().member_search.is_empty() { i18n.t("tenants.no_members_yet") } else { i18n.t("tenants.no_member_matches") }} } } }
                        }
                        Pager { page: query().member_page, total_pages: value.members.total_pages, total: value.members.total, on_page: move |page| query.write().set_member_page(page) }
                    }
                },
            }
            if let Some((member, action)) = pending_member() {
                {let current=match action {MemberAction::Role=>tenant_role_label(i18n,member.tenant_role),MemberAction::Status=>membership_status_label(i18n,member.membership_status),_=>""};
                 let next=match action {MemberAction::Role=>tenant_role_label(i18n,if member.tenant_role==TenantRole::Admin{TenantRole::Member}else{TenantRole::Admin}),MemberAction::Status=>membership_status_label(i18n,if member.membership_status==MembershipStatus::Active{MembershipStatus::Suspended}else{MembershipStatus::Active}),_=>""};
                 let target=if matches!(action,MemberAction::Role|MemberAction::Status){format!("{} · {current} → {next}",member.email)}else{member.email.clone()};
                 let danger=action==MemberAction::Remove||(action==MemberAction::Status&&member.membership_status==MembershipStatus::Active)||(action==MemberAction::Role&&member.tenant_role==TenantRole::Admin);
                 rsx!{
                ui::ConfirmModal {
                    open: true,
                    title: i18n.t(action.label()).to_owned(),
                    message: target,
                    confirm_text: i18n.t(action.label()).to_owned(),
                    cancel_text: i18n.t("form.cancel").to_owned(),
                    danger,
                    busy: busy(),
                    oncancel: move |_| if !busy() { pending_member.set(None); },
                    onconfirm: confirm_member,
                }
                 }}
            }
    }}
}

#[cfg(test)]
mod tests {
    use super::{AccessQuery, PAGE_SIZE};
    use crate::router::Route;

    #[test]
    fn member_search_resets_only_the_member_page() {
        let mut query = AccessQuery::default();
        query.set_member_page(4);

        query.set_member_search("  admin@example.com  ");

        assert_eq!(query.member_search, "admin@example.com");
        assert_eq!(query.member_page, 1);
        assert_eq!(PAGE_SIZE, 20);
    }

    #[test]
    fn page_numbers_never_fall_below_one() {
        let mut query = AccessQuery::default();
        query.set_member_page(0);

        assert_eq!(query.member_page, 1);
    }

    #[test]
    fn platform_tenant_sections_have_stable_deep_links() {
        let tenant_id = "00000000-0000-0000-0000-000000000001".to_owned();
        assert_eq!(
            Route::PlatformTenantSettings {
                tenant_id: tenant_id.clone()
            }
            .to_string(),
            format!("/admin/tenants/{tenant_id}/settings")
        );
        assert_eq!(
            Route::PlatformTenantAudit {
                tenant_id: tenant_id.clone()
            }
            .to_string(),
            format!("/admin/tenants/{tenant_id}/audit")
        );
    }
}
