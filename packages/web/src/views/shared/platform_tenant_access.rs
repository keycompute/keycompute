use crate::{
    hooks::use_i18n::use_i18n,
    router::Route,
    services::api_client::user_error_message,
    stores::{auth_store::AuthStore, ui_store::UiStore, user_store::UserStore},
    utils::{
        resource::{KeyedResourceValue, current_keyed_value},
        time::format_time,
    },
    views::shared::accounts::NoPermissionView,
    views::tenant::{
        PlatformTenantAudit as TenantAuditPanel, PlatformTenantFinance as TenantFinancePanel,
        PlatformTenantFinancialControls as TenantFinancialControlsPanel,
        PlatformTenantKeys as TenantKeysPanel, PlatformTenantSettings as TenantSettingsPanel,
        common::{self, OneTimeRecoveryLink, Pager, TechnicalId, WorkspaceScope},
        workspace_switcher::use_workspace_dirty_blocker,
    },
};
use client_api::{
    MembershipStatus, TenantRole, UserStatus,
    api::tenant_control::{
        CreateInvitation, InvitationCreateResponse, InvitationOutcome, MemberPatch,
        NotificationStatus, Page, TenantInvitation, TenantMember,
    },
};
use dioxus::prelude::*;
use uuid::Uuid;

const PAGE_SIZE: u32 = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
struct AccessQuery {
    member_page: u32,
    invitation_page: u32,
    member_search: String,
}

impl Default for AccessQuery {
    fn default() -> Self {
        Self {
            member_page: 1,
            invitation_page: 1,
            member_search: String::new(),
        }
    }
}

impl AccessQuery {
    fn set_member_page(&mut self, page: u32) {
        self.member_page = page.max(1);
    }

    fn set_invitation_page(&mut self, page: u32) {
        self.invitation_page = page.max(1);
    }

    fn set_member_search(&mut self, search: &str) {
        self.member_search = search.trim().to_owned();
        self.member_page = 1;
    }
}

#[derive(Clone)]
struct AccessData {
    context: client_api::api::tenant_control::TenantContext,
    active_members: i64,
    members: Page<TenantMember>,
    invitations: Page<TenantInvitation>,
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
    Owner,
    Remove,
}

impl MemberAction {
    fn label(self) -> &'static str {
        match self {
            Self::Role => "tenant.change_role",
            Self::Status => "tenant.change_status",
            Self::Owner => "tenant.transfer",
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

fn invitation_status_label(i18n: crate::i18n::I18n, status: &str) -> String {
    let key = match status {
        "pending" => Some("tenant.status_pending"),
        "accepted" => Some("tenant.status_accepted"),
        "revoked" => Some("tenant.status_revoked"),
        "expired" => Some("tenant.status_expired"),
        _ => None,
    };
    key.map(|key| i18n.t(key).to_owned())
        .unwrap_or_else(|| status.to_owned())
}

fn notification_label(value: &NotificationStatus) -> &'static str {
    match value {
        NotificationStatus::Sent => "tenant.notification_sent",
        NotificationStatus::Failed => "tenant.notification_failed",
        NotificationStatus::Unconfigured => "tenant.notification_unconfigured",
        NotificationStatus::NotApplicable => "tenant.notification_duplicate",
    }
}

fn valid_email(value: &str) -> bool {
    value.len() <= 255
        && !value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        && value.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && !domain.is_empty() && !domain.contains('@')
        })
}

fn default_invitation_role(owner_user_id: Option<Uuid>) -> TenantRole {
    if owner_user_id.is_none() {
        TenantRole::Admin
    } else {
        TenantRole::Member
    }
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
            if let Some(Err(value)) = loaded_context.as_ref() {
                div { class: "alert alert-error", role: "alert", {user_error_message(i18n, value)} }
            }
            match section {
                PlatformTenantSection::Access => match loaded_context {
                    None => rsx! { div { class: "content-loading", role: "status", span { class: "spinner", aria_hidden: "true" } span { {i18n.t("common.loading")} } } },
                    Some(Err(_)) => rsx! {},
                    Some(Ok(value)) => rsx! { PlatformTenantAccessPanel { scope, tenant_id: value.id, owner_user_id: value.owner_user_id, on_context_changed: move |_| context.restart() } },
                },
                PlatformTenantSection::Settings => rsx! { TenantSettingsPanel { scope, on_context_changed: move |_| context.restart() } },
                PlatformTenantSection::Keys => rsx! { TenantKeysPanel { scope } },
                PlatformTenantSection::Finance => rsx! { TenantFinancePanel { scope } },
                PlatformTenantSection::FinancialControls => rsx! { TenantFinancialControlsPanel { scope } },
                PlatformTenantSection::Audit => rsx! { TenantAuditPanel { scope } },
            }
        }
    }
}

#[component]
fn PlatformTenantAccessPanel(
    scope: WorkspaceScope,
    tenant_id: Uuid,
    owner_user_id: Option<Uuid>,
    on_context_changed: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut ui = use_context::<UiStore>();
    let initial_role = default_invitation_role(owner_user_id);
    let mut email = use_signal(String::new);
    let mut default_role = use_signal(move || initial_role);
    let mut role = use_signal(move || initial_role);
    let mut hours = use_signal(|| "24".to_owned());
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut created = use_signal(|| None::<InvitationCreateResponse>);
    let mut pending_member = use_signal(|| None::<(TenantMember, MemberAction)>);
    let mut pending_invitation = use_signal(|| None::<TenantInvitation>);
    let mut member_search_input = use_signal(String::new);
    let mut query = use_signal(AccessQuery::default);
    let mut data = use_resource(move || async move {
        let request = query();
        let key = (scope, request.clone());
        let result = common::read(auth, users, scope, move |token| {
            let request = request.clone();
            async move {
                let api = scope.api()?;
                let (context, active_page, members, invitations) = futures::try_join!(
                    api.context(&token),
                    api.members_filtered(1, 1, None, Some(MembershipStatus::Active), &token),
                    api.members(
                        request.member_page,
                        PAGE_SIZE,
                        Some(&request.member_search),
                        &token,
                    ),
                    api.invitations(request.invitation_page, PAGE_SIZE, &token),
                )?;
                Ok(AccessData {
                    context,
                    active_members: active_page.total,
                    members,
                    invitations,
                })
            }
        })
        .await;
        KeyedResourceValue::new(key, result)
    });
    let current_key = (scope, query());
    let loaded = current_keyed_value(&current_key, data.state().cloned(), data());
    let current_owner = loaded
        .as_ref()
        .and_then(|value| value.as_ref().ok())
        .and_then(|value| value.context.owner_user_id);
    use_effect(move || {
        let loaded = current_keyed_value(&current_key, data.state().cloned(), data());
        let Some(Ok(value)) = loaded else {
            return;
        };
        let next_default = default_invitation_role(value.context.owner_user_id);
        let previous_default = default_role();
        if next_default != previous_default {
            default_role.set(next_default);
            if email().trim().is_empty() && !busy() && role() == previous_default {
                role.set(next_default);
            }
        }
    });
    use_workspace_dirty_blocker(move || {
        busy()
            || !email().trim().is_empty()
            || role() != default_role()
            || hours() != "24"
            || pending_member().is_some()
            || pending_invitation().is_some()
    });

    let create_invitation = move |_| {
        if busy() {
            return;
        }
        let email_value = email().trim().to_owned();
        let Some(expires_in_seconds) = hours()
            .parse::<i64>()
            .ok()
            .filter(|value| (1..=168).contains(value))
            .and_then(|value| value.checked_mul(3600))
        else {
            error.set(i18n.t("tenant.invalid_duration").to_owned());
            return;
        };
        if !valid_email(&email_value) {
            error.set(i18n.t("tenant.invalid_email").to_owned());
            return;
        }
        let body = CreateInvitation {
            email: email_value,
            tenant_role: role(),
            expires_in_seconds,
        };
        busy.set(true);
        error.set(String::new());
        created.set(None);
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| {
                let body = body.clone();
                async move { scope.api()?.create_invitation(&body, &token).await }
            })
            .await;
            if !scope.is_current(auth, users) {
                return;
            }
            busy.set(false);
            match result {
                Ok(value) => {
                    created.set(Some(value));
                    email.set(String::new());
                    role.set(default_role());
                    hours.set("24".to_owned());
                    query.write().set_invitation_page(1);
                    data.restart();
                    ui.show_success(i18n.t("tenant.saved"));
                }
                Err(value) => error.set(user_error_message(i18n, &value)),
            }
        });
    };

    let revoke_invitation = move |_| {
        if busy() {
            return;
        }
        let Some(invitation) = pending_invitation() else {
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
            pending_invitation.set(None);
            match result {
                Ok(_) => {
                    data.restart();
                    ui.show_success(i18n.t("tenant.saved"));
                }
                Err(value) => error.set(user_error_message(i18n, &value)),
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
                        MemberAction::Owner => {
                            api.transfer_ownership(member.user_id, &token).await?;
                        }
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
                            dl { class: "tenant-overview-facts tenant-overview-facts-three",
                                div { dt { {i18n.t("tenants.tenant_id")} } dd { TechnicalId { value: tenant_id.to_string() } } }
                                div { dt { {i18n.t("tenant.owner")} } dd {
                                    if let Some(owner) = value.context.owner_user_id { TechnicalId { value: owner.to_string() } }
                                    else { span { class: "text-secondary", {i18n.t("tenant.owner_unassigned")} } }
                                } }
                                div { dt { {i18n.t("tenants.active_members")} } dd { "{value.active_members}" } }
                            }
                        }
                    }
                    section { class: "section tenant-invite-card",
                        h2 { class: "section-title", {i18n.t("tenant.invite")} }
                        if value.context.owner_user_id.is_none() { p { class: "alert alert-info", {i18n.t("tenants.first_admin_owner_hint")} } }
                        form { class: "section-body tenant-invite-form", onsubmit: create_invitation,
                            div { class: "form-field",
                                label { class: "form-label", r#for: "platform-invite-email", {i18n.t("tenant.email")} }
                                input { id: "platform-invite-email", class: "input-field", r#type: "email", maxlength: "255", value: "{email}", disabled: busy(), oninput: move |event| email.set(event.value()) }
                            }
                            div { class: "form-field",
                                label { class: "form-label", r#for: "platform-invite-role", {i18n.t("tenant.role")} }
                                select { id: "platform-invite-role", class: "input-field", value: "{role().as_str()}", disabled: busy(), onchange: move |event| if let Ok(value) = event.value().parse::<TenantRole>() { role.set(value); },
                                    option { value: "admin", {i18n.t("tenant.role_admin")} }
                                    option { value: "member", {i18n.t("tenant.role_member")} }
                                }
                            }
                            div { class: "form-field",
                                label { class: "form-label", r#for: "platform-invite-duration", {i18n.t("tenant.duration_hours")} }
                                input { id: "platform-invite-duration", class: "input-field", r#type: "number", min: "1", max: "168", value: "{hours}", disabled: busy(), oninput: move |event| hours.set(event.value()) }
                            }
                            button { class: "btn btn-primary", r#type: "submit", disabled: busy() || email().trim().is_empty(), {i18n.t("tenant.invite")} }
                        }
                    }
                    if let Some(result) = created() {
                        section { class: "alert alert-info", role: "status",
                            p { {i18n.t(notification_label(&result.notification))} }
                            if result.outcome == InvitationOutcome::AlreadyPending { p { {i18n.t("tenant.already_pending")} } }
                            if let Some(link) = result.acceptance_link {
                                OneTimeRecoveryLink { link, input_id: "platform-invite-recovery".to_string() }
                            }
                            button { class: "btn btn-secondary", r#type: "button", onclick: move |_| created.set(None), {i18n.t("tenant.hide")} }
                        }
                    }
                    section { class: "section",
                        h2 { class: "section-title", {i18n.t("tenant.members")} }
                        form { class: "card filter-panel tenant-member-filter", onsubmit: move |_| query.write().set_member_search(&member_search_input()),
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
                                    {let role_member=member.clone();let status_member=member.clone();let owner_member=member.clone();let remove_member=member.clone();let is_owner=value.context.owner_user_id==Some(member.user_id);let removed=member.membership_status==MembershipStatus::Removed;rsx!{
                                        tr { key: "{member.user_id}",
                                            td { "{member.email}" if is_owner { span { class: "badge", {i18n.t("tenant.owner")} } } }
                                            td { {tenant_role_label(i18n, member.tenant_role)} }
                                            td { {membership_status_label(i18n, member.membership_status)} }
                                            td { {user_status_label(i18n, member.user_status)} }
                                            td { div { class: "table-actions",
                                                if !removed {
                                                    button { class: "btn btn-secondary btn-sm", disabled: busy(), onclick: move |_| pending_member.set(Some((role_member.clone(), MemberAction::Role))), {i18n.t("tenant.change_role")} }
                                                    button { class: "btn btn-secondary btn-sm", disabled: busy(), onclick: move |_| pending_member.set(Some((status_member.clone(), MemberAction::Status))), {i18n.t("tenant.change_status")} }
                                                    if !is_owner && member.tenant_role == TenantRole::Admin && member.membership_status == MembershipStatus::Active && member.user_status == UserStatus::Active {
                                                        button { class: "btn btn-secondary btn-sm", disabled: busy(), onclick: move |_| pending_member.set(Some((owner_member.clone(), MemberAction::Owner))), {i18n.t("tenant.transfer")} }
                                                    }
                                                    button { class: "btn btn-danger btn-sm", disabled: busy(), onclick: move |_| pending_member.set(Some((remove_member.clone(), MemberAction::Remove))), {i18n.t("tenant.remove")} }
                                                }
                                            } }
                                        }
                                    }}
                                } }
                            } }
                            if value.members.items.is_empty() { div { class: "empty-state compact-empty-state", h3 { class: "empty-title", {i18n.t("tenants.no_members_yet")} } } }
                        }
                        Pager { page: query().member_page, total_pages: value.members.total_pages, total: value.members.total, on_page: move |page| query.write().set_member_page(page) }
                    }
                    section { class: "section",
                        h2 { class: "section-title", {i18n.t("tenant.invitations")} }
                        div { class: "table-pagination-panel table-pagination-frame",
                            div { class: "table-container", table { class: "table",
                                thead { tr { th { {i18n.t("tenant.email")} } th { {i18n.t("tenant.role")} } th { {i18n.t("table.status")} } th { {i18n.t("tenant.expires")} } th { {i18n.t("tenant.actions")} } } }
                                tbody { for invitation in value.invitations.items.iter() { {let selected=invitation.clone();rsx!{tr { key: "{invitation.id}", td { "{invitation.email}" } td { {tenant_role_label(i18n, invitation.tenant_role)} } td { {invitation_status_label(i18n, &invitation.status)} } td { {format_time(&invitation.expires_at)} } td { if invitation.status == "pending" { button { class: "btn btn-danger btn-sm", disabled: busy(), onclick: move |_| pending_invitation.set(Some(selected.clone())), {i18n.t("tenant.revoke")} } } } }}} } }
                            } }
                            if value.invitations.items.is_empty() { div { class: "empty-state compact-empty-state", h3 { class: "empty-title", {i18n.t("tenant.empty")} } } }
                        }
                        Pager { page: query().invitation_page, total_pages: value.invitations.total_pages, total: value.invitations.total, on_page: move |page| query.write().set_invitation_page(page) }
                    }
                },
            }
            if let Some((member, action)) = pending_member() {
                {let owner_impact=current_owner==Some(member.user_id)&&action!=MemberAction::Owner;
                 let current=match action {MemberAction::Role=>tenant_role_label(i18n,member.tenant_role),MemberAction::Status=>membership_status_label(i18n,member.membership_status),_=>""};
                 let next=match action {MemberAction::Role=>tenant_role_label(i18n,if member.tenant_role==TenantRole::Admin{TenantRole::Member}else{TenantRole::Admin}),MemberAction::Status=>membership_status_label(i18n,if member.membership_status==MembershipStatus::Active{MembershipStatus::Suspended}else{MembershipStatus::Active}),_=>""};
                 let target=if matches!(action,MemberAction::Role|MemberAction::Status){format!("{} · {current} → {next}",member.email)}else{member.email.clone()};
                 let target=if owner_impact{format!("{target} · {}",i18n.t("tenants.owner_clear_warning"))}else{target};
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
            if let Some(invitation) = pending_invitation() {
                ui::ConfirmModal {
                    open: true,
                    title: i18n.t("tenant.revoke").to_owned(),
                    message: invitation.email,
                    confirm_text: i18n.t("tenant.revoke").to_owned(),
                    cancel_text: i18n.t("form.cancel").to_owned(),
                    danger: true,
                    busy: busy(),
                    oncancel: move |_| if !busy() { pending_invitation.set(None); },
                    onconfirm: revoke_invitation,
                }
            }
    }}
}

#[cfg(test)]
mod tests {
    use super::{AccessQuery, PAGE_SIZE, default_invitation_role, valid_email};
    use crate::router::Route;
    use client_api::TenantRole;
    use uuid::Uuid;

    #[test]
    fn member_search_resets_only_the_member_page() {
        let mut query = AccessQuery::default();
        query.set_member_page(4);
        query.set_invitation_page(3);

        query.set_member_search("  admin@example.com  ");

        assert_eq!(query.member_search, "admin@example.com");
        assert_eq!(query.member_page, 1);
        assert_eq!(query.invitation_page, 3);
        assert_eq!(PAGE_SIZE, 20);
    }

    #[test]
    fn page_numbers_never_fall_below_one() {
        let mut query = AccessQuery::default();
        query.set_member_page(0);
        query.set_invitation_page(0);

        assert_eq!(query.member_page, 1);
        assert_eq!(query.invitation_page, 1);
    }

    #[test]
    fn invitation_email_validation_matches_the_server_shape() {
        assert!(valid_email("admin@keycompute.local"));
        assert!(valid_email("a@b"));
        assert!(!valid_email("missing-at.example"));
        assert!(!valid_email("a@@b.example"));
        assert!(!valid_email("a @b.example"));
        assert!(!valid_email("a@b.example\0"));
    }

    #[test]
    fn first_invitation_defaults_to_admin_only_until_an_owner_exists() {
        assert_eq!(default_invitation_role(None), TenantRole::Admin);
        assert_eq!(
            default_invitation_role(Some(Uuid::from_u128(1))),
            TenantRole::Member
        );
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
