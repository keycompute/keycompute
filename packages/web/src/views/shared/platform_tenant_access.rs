use crate::{
    hooks::use_i18n::use_i18n,
    router::Route,
    services::api_client::{get_client, user_error_message, with_auto_refresh},
    stores::{auth_store::AuthStore, ui_store::UiStore, user_store::UserStore},
    utils::{
        resource::{KeyedResourceValue, current_keyed_value},
        time::format_time,
    },
    views::shared::accounts::NoPermissionView,
    views::tenant::{
        PlatformTenantAudit, PlatformTenantFinance, PlatformTenantFinancialControls,
        PlatformTenantKeys, PlatformTenantSettings,
        common::{Pager, WorkspaceScope},
    },
};
use client_api::{
    MembershipStatus, TenantRole, UserStatus,
    api::tenant_control::{
        CreateInvitation, InvitationCreateResponse, MemberPatch, Page, TenantContext,
        TenantControlApi, TenantInvitation, TenantMember,
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
    context: TenantContext,
    active_members: i64,
    members: Page<TenantMember>,
    invitations: Page<TenantInvitation>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AccessTab {
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

#[component]
pub fn PlatformTenantAccess(tenant_id: String) -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut ui = use_context::<UiStore>();
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

    let mut tab = use_signal(AccessTab::default);
    let mut email = use_signal(String::new);
    let mut role = use_signal(|| TenantRole::Admin);
    let mut hours = use_signal(|| "24".to_owned());
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut created = use_signal(|| None::<InvitationCreateResponse>);
    let mut pending_member = use_signal(|| None::<(TenantMember, MemberAction)>);
    let mut member_search_input = use_signal(String::new);
    let mut query = use_signal(AccessQuery::default);
    let mut data = use_resource(move || async move {
        let request = query();
        let key = (target, request.clone());
        let result = with_auto_refresh(auth, move |token| {
            let request = request.clone();
            async move {
                let api = TenantControlApi::platform_tenant(&get_client(), target)?;
                let context = api.context(&token).await?;
                let active_members = api
                    .members_filtered(1, 1, None, Some(MembershipStatus::Active), &token)
                    .await?
                    .total;
                let members = api
                    .members(
                        request.member_page,
                        PAGE_SIZE,
                        Some(&request.member_search),
                        &token,
                    )
                    .await?;
                let invitations = api
                    .invitations(request.invitation_page, PAGE_SIZE, &token)
                    .await?;
                Ok(AccessData {
                    context,
                    active_members,
                    members,
                    invitations,
                })
            }
        })
        .await;
        KeyedResourceValue::new(key, result)
    });
    let current_key = (target, query());
    let loaded = current_keyed_value(&current_key, data.state().cloned(), data());

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
        if email_value.is_empty() || email_value.len() > 255 {
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
            let result = with_auto_refresh(auth, move |token| {
                let body = body.clone();
                async move {
                    TenantControlApi::platform_tenant(&get_client(), target)?
                        .create_invitation(&body, &token)
                        .await
                }
            })
            .await;
            busy.set(false);
            match result {
                Ok(value) => {
                    created.set(Some(value));
                    email.set(String::new());
                    role.set(TenantRole::Admin);
                    query.write().set_invitation_page(1);
                    data.restart();
                    ui.show_success(i18n.t("tenant.saved"));
                }
                Err(value) => error.set(user_error_message(i18n, &value)),
            }
        });
    };

    let mut revoke_invitation = move |invitation_id: Uuid| {
        if busy() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        created.set(None);
        spawn(async move {
            let result = with_auto_refresh(auth, move |token| async move {
                TenantControlApi::platform_tenant(&get_client(), target)?
                    .revoke_invitation(invitation_id, &token)
                    .await
            })
            .await;
            busy.set(false);
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
            let result = with_auto_refresh(auth, move |token| {
                let member = member.clone();
                async move {
                    let api = TenantControlApi::platform_tenant(&get_client(), target)?;
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
            busy.set(false);
            pending_member.set(None);
            match result {
                Ok(()) => {
                    data.restart();
                    ui.show_success(i18n.t("tenant.saved"));
                }
                Err(value) => {
                    error.set(user_error_message(i18n, &value));
                    data.restart();
                }
            }
        });
    };

    rsx! {
        div { class: "page-container platform-tenant-access",
            ui::PageHeader {
                title: loaded.as_ref().and_then(|value| value.as_ref().ok()).map(|value| value.context.name.clone()).unwrap_or_else(|| i18n.t("tenants.access_title").to_owned()),
                description: i18n.t("tenants.access_hint").to_owned(),
                actions: rsx! { Link { class: "btn btn-secondary", to: Route::Tenants {}, {i18n.t("common.back")} } },
            }
            nav { class: "segmented-control platform-tenant-tabs", aria_label: i18n.t("tenants.access_title"),
                button { class: "btn btn-secondary", aria_pressed: tab() == AccessTab::Access, onclick: move |_| tab.set(AccessTab::Access), {i18n.t("tenants.access_title")} }
                button { class: "btn btn-secondary", aria_pressed: tab() == AccessTab::Settings, onclick: move |_| tab.set(AccessTab::Settings), {i18n.t("tenant.settings")} }
                button { class: "btn btn-secondary", aria_pressed: tab() == AccessTab::Keys, onclick: move |_| tab.set(AccessTab::Keys), {i18n.t("tenant_keys.title")} }
                button { class: "btn btn-secondary", aria_pressed: tab() == AccessTab::Finance, onclick: move |_| tab.set(AccessTab::Finance), {i18n.t("tenant_finance.title")} }
                button { class: "btn btn-secondary", aria_pressed: tab() == AccessTab::FinancialControls, onclick: move |_| tab.set(AccessTab::FinancialControls), {i18n.t("tenant_financial_controls.title")} }
                button { class: "btn btn-secondary", aria_pressed: tab() == AccessTab::Audit, onclick: move |_| tab.set(AccessTab::Audit), {i18n.t("tenant.audit")} }
            }
            if !error().is_empty() { div { class: "alert alert-error", role: "alert", "{error}" } }
            if tab() == AccessTab::Access { match loaded {
                None => rsx! { div { class: "content-loading", role: "status", span { class: "spinner", aria_hidden: "true" } span { {i18n.t("common.loading")} } } },
                Some(Err(value)) => rsx! { div { class: "alert alert-error", role: "alert", {user_error_message(i18n, &value)} } },
                Some(Ok(value)) => rsx! {
                    section { class: "section",
                        h2 { class: "section-title", {i18n.t("tenants.access_summary")} }
                        div { class: "section-body",
                            dl { class: "tenant-overview-facts",
                                div { dt { {i18n.t("tenants.tenant_id")} } dd { code { "{value.context.id}" } } }
                                div { dt { {i18n.t("tenant.owner_id")} } dd {
                                    if let Some(owner) = value.context.owner_user_id { code { "{owner}" } }
                                    else { span { class: "text-secondary", {i18n.t("tenant.owner_unassigned")} } }
                                } }
                                div { dt { {i18n.t("tenants.users")} } dd { "{value.active_members}" } }
                            }
                        }
                    }
                    section { class: "section tenant-invite-card",
                        h2 { class: "section-title", {i18n.t("tenant.invite")} }
                        div { class: "section-body tenant-invite-form",
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
                            button { class: "btn btn-primary", disabled: busy() || email().trim().is_empty(), onclick: create_invitation, {i18n.t("tenant.invite")} }
                        }
                    }
                    if let Some(result) = created() {
                        section { class: "alert alert-info", role: "status",
                            if let Some(link) = result.acceptance_link {
                                label { class: "form-label", r#for: "platform-invite-recovery", {i18n.t("tenant.recovery_link")} }
                                input { id: "platform-invite-recovery", class: "input-field", readonly: true, value: link, autocomplete: "off" }
                                p { {i18n.t("tenant.recovery_hint")} }
                            } else { p { {i18n.t("tenant.already_pending")} } }
                        }
                    }
                    section { class: "section",
                        h2 { class: "section-title", {i18n.t("tenant.members")} }
                        div { class: "card filter-panel tenant-member-filter",
                            div { class: "filter-grid filter-grid-health",
                                div { class: "form-field filter-field-grow",
                                    label { class: "form-label", r#for: "platform-member-search", {i18n.t("tenant.search")} }
                                    input { id: "platform-member-search", class: "input-field", r#type: "search", maxlength: "255", value: "{member_search_input}", disabled: busy(), oninput: move |event| member_search_input.set(event.value()) }
                                }
                                div { class: "filter-actions",
                                    button { class: "btn btn-primary", disabled: busy(), onclick: move |_| query.write().set_member_search(&member_search_input()), {i18n.t("tenant.search")} }
                                    button { class: "btn btn-secondary", disabled: busy(), onclick: move |_| data.restart(), {i18n.t("tenant.reload")} }
                                }
                            }
                        }
                        div { class: "table-pagination-panel table-pagination-frame",
                            div { class: "table-container", table { class: "table",
                                thead { tr { th { {i18n.t("tenant.user")} } th { {i18n.t("tenant.role")} } th { {i18n.t("tenant.membership_status")} } th { {i18n.t("tenant.actions")} } } }
                                tbody { for member in value.members.items.iter() {
                                    {let role_member=member.clone();let status_member=member.clone();let owner_member=member.clone();let remove_member=member.clone();let is_owner=value.context.owner_user_id==Some(member.user_id);let removed=member.membership_status==MembershipStatus::Removed;rsx!{
                                        tr { key: "{member.user_id}",
                                            td { "{member.email}" if is_owner { span { class: "badge", {i18n.t("tenant.owner")} } } }
                                            td { "{member.tenant_role.as_str()}" }
                                            td { "{member.membership_status.as_str()}" }
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
                            if value.members.items.is_empty() { p { class: "text-secondary", {i18n.t("tenants.no_members_yet")} } }
                        }
                        Pager { page: query().member_page, total_pages: value.members.total_pages, total: value.members.total, on_page: move |page| query.write().set_member_page(page) }
                    }
                    section { class: "section",
                        h2 { class: "section-title", {i18n.t("tenant.invitations")} }
                        div { class: "table-pagination-panel table-pagination-frame",
                            div { class: "table-container", table { class: "table",
                                thead { tr { th { {i18n.t("tenant.email")} } th { {i18n.t("tenant.role")} } th { {i18n.t("table.status")} } th { {i18n.t("tenant.expires")} } th { {i18n.t("tenant.actions")} } } }
                                tbody { for invitation in value.invitations.items.iter() { tr { key: "{invitation.id}", td { "{invitation.email}" } td { "{invitation.tenant_role.as_str()}" } td { "{invitation.status}" } td { {format_time(&invitation.expires_at)} } td { if invitation.status == "pending" { button { class: "btn btn-danger btn-sm", disabled: busy(), onclick: { let id = invitation.id; move |_| revoke_invitation(id) }, {i18n.t("tenant.revoke")} } } } } } }
                            } }
                        }
                        Pager { page: query().invitation_page, total_pages: value.invitations.total_pages, total: value.invitations.total, on_page: move |page| query.write().set_invitation_page(page) }
                    }
                },
            } } else if tab() == AccessTab::Settings {
                PlatformTenantSettings { scope }
            } else if tab() == AccessTab::Keys {
                PlatformTenantKeys { scope }
            } else if tab() == AccessTab::Finance {
                PlatformTenantFinance { scope }
            } else if tab() == AccessTab::FinancialControls {
                PlatformTenantFinancialControls { scope }
            } else if tab() == AccessTab::Audit {
                PlatformTenantAudit { scope }
            }
            if let Some((member, action)) = pending_member() {
                ui::ConfirmModal {
                    open: true,
                    title: i18n.t(action.label()).to_owned(),
                    message: member.email,
                    confirm_text: i18n.t(action.label()).to_owned(),
                    cancel_text: i18n.t("form.cancel").to_owned(),
                    danger: action == MemberAction::Remove,
                    busy: busy(),
                    oncancel: move |_| if !busy() { pending_member.set(None); },
                    onconfirm: confirm_member,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AccessQuery, PAGE_SIZE};

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
}
