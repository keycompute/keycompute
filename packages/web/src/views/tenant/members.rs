use super::common::{self, CommandDialog, Pager, WorkspaceLinks, WorkspaceScope};
use crate::{
    hooks::use_i18n::use_i18n,
    router::Route,
    services::api_client::user_error_message,
    stores::{auth_store::AuthStore, ui_store::UiStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::{
    ClientError, MembershipStatus, TenantRole, UserStatus,
    api::tenant_control::{MemberPatch, Page, TenantContext, TenantMember},
};
use dioxus::prelude::*;
const PAGE_SIZE: u32 = 20;
#[derive(Clone, Copy, PartialEq)]
enum Action {
    Role,
    Suspend,
    Remove,
    Transfer,
}
impl Action {
    fn label(self) -> &'static str {
        match self {
            Self::Role => "tenant.change_role",
            Self::Suspend => "tenant.change_status",
            Self::Remove => "tenant.remove",
            Self::Transfer => "tenant.transfer",
        }
    }
}
#[derive(Clone)]
struct Pending {
    member: TenantMember,
    action: Action,
}
#[derive(Clone)]
struct MemberData {
    context: TenantContext,
    page: Page<TenantMember>,
}
/// The router may reuse an Outlet VNode across AppShell remounts. A page-local
/// key must discard drafts, confirmation dialogs and one-time secrets as well.
#[component]
pub fn TenantMembers() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let epoch = (auth.state)().session_id;
    let scope = WorkspaceScope::from_stores(auth, users);
    let key = format!("{epoch}:{scope:?}");
    // A keyed fragment is required: a key on one static component alone
    // does not engage Dioxus's keyed child reconciliation.
    rsx! { for identity in [key] { TenantMembersPage { key: "{identity}" } } }
}

#[component]
fn TenantMembersPage() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut ui = use_context::<UiStore>();
    let nav = use_navigator();
    let i18n = use_i18n();
    let scope = WorkspaceScope::from_stores(auth, users);
    let mut page = use_signal(|| 1u32);
    let mut search_input = use_signal(String::new);
    let mut search = use_signal(String::new);
    let mut pending = use_signal(|| None::<Pending>);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut data = use_resource(move || {
        let scope = WorkspaceScope::from_stores(auth, users);
        let page = page();
        let search = search();
        let key = (scope, page, search.clone());
        async move {
            let result = if let Some(scope) = scope {
                common::read(auth, users, scope, move |token| {
                    let search = search.clone();
                    async move {
                        let api = scope.api()?;
                        let context = api.context(&token).await?;
                        let page = api.members(page, PAGE_SIZE, Some(&search), &token).await?;
                        Ok(MemberData { context, page })
                    }
                })
                .await
            } else {
                Err(ClientError::Forbidden(
                    "selected membership required".into(),
                ))
            };
            KeyedResourceValue::new(key, result)
        }
    });
    let key = (scope, page(), search());
    let loaded = current_keyed_value(&key, data.state().cloned(), data());
    let confirm = move |_| {
        if busy() {
            return;
        }
        let (Some(scope), Some(command)) = (WorkspaceScope::from_stores(auth, users), pending())
        else {
            return;
        };
        busy.set(true);
        error.set(String::new());
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| async move {
                let api = scope.api()?;
                let before = command.member;
                let changed = match command.action {
                    Action::Transfer => {
                        api.transfer_ownership(before.user_id, &token).await?;
                        true
                    }
                    Action::Remove => {
                        let row = api
                            .remove_member(before.user_id, before.authz_version, &token)
                            .await?;
                        row.user_id == scope.user_id && row.authz_version != before.authz_version
                    }
                    Action::Role | Action::Suspend => {
                        let patch = MemberPatch {
                            expected_authz_version: before.authz_version,
                            tenant_role: (command.action == Action::Role).then_some(
                                if before.tenant_role == TenantRole::Admin {
                                    TenantRole::Member
                                } else {
                                    TenantRole::Admin
                                },
                            ),
                            status: (command.action == Action::Suspend).then_some(
                                if before.membership_status == MembershipStatus::Active {
                                    MembershipStatus::Suspended
                                } else {
                                    MembershipStatus::Active
                                },
                            ),
                        };
                        let row = api.patch_member(before.user_id, &patch, &token).await?;
                        row.user_id == scope.user_id && row.authz_version != before.authz_version
                    }
                };
                Ok(changed)
            })
            .await;
            if !scope.is_current(auth, users) {
                return;
            }
            busy.set(false);
            pending.set(None);
            match result {
                Ok(true) => {
                    common::invalidate_own_session(
                        auth,
                        users,
                        scope,
                        ui,
                        i18n.t("tenant.saved_relogin"),
                    );
                    nav.replace(Route::Login {});
                }
                Ok(false) => {
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
    rsx! {div {class:"page-container tenant-members",
        ui::PageHeader {title:i18n.t("tenant.members").to_string(),description:i18n.t("tenant.members_hint").to_string()}
        WorkspaceLinks {}
        if !error().is_empty(){div {class:"alert alert-error",role:"alert","{error}"}}
        div {class:"toolbar",
            label {r#for:"member-search",{i18n.t("tenant.search")}}
            input {id:"member-search",class:"input-field",value:"{search_input}",maxlength:"255",oninput:move |e|search_input.set(e.value())}
            button {class:"btn btn-secondary",onclick:move |_|{search.set(search_input().trim().into());page.set(1);},{i18n.t("tenant.search")}}
            button {class:"btn btn-secondary",disabled:busy(),onclick:move |_|data.restart(),{i18n.t("tenant.reload")}}
        }
        match loaded {
            None=>rsx!{p {role:"status",{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{div {class:"alert alert-error",role:"alert",{user_error_message(&e)}}},
            Some(Ok(value))=>rsx!{
                div {class:"table-pagination-panel",
                    table {class:"table",thead {tr {th {{i18n.t("tenant.user")}} th {{i18n.t("tenant.role")}} th {{i18n.t("tenant.membership_status")}} th {{i18n.t("tenant.user_status")}} th {{i18n.t("tenant.actions")}}}}
                        tbody {
                            for member in value.page.items.iter() {
                                {let role_member=member.clone();let suspend_member=member.clone();let remove_member=member.clone();let transfer_member=member.clone();
                                    let owner=member.user_id==value.context.owner_user_id;
                                    let removed=member.membership_status==MembershipStatus::Removed;
                                    let can_transfer=scope.is_some_and(|s|s.user_id==value.context.owner_user_id) && !owner && member.membership_status==MembershipStatus::Active && member.user_status==UserStatus::Active && member.tenant_role==TenantRole::Admin;
                                    rsx!{tr {key:"{member.user_id}",
                                        td {"{member.email}" if owner {span {class:"badge",{i18n.t("tenant.owner")}}}}
                                        td {"{member.tenant_role.as_str()}"}
                                        td {"{member.membership_status.as_str()}"}
                                        td {"{member.user_status.as_str()}"}
                                        td {
                                            if !owner && !removed {
                                                button {class:"btn btn-secondary btn-sm",disabled:busy(),onclick:move |_|pending.set(Some(Pending{member:role_member.clone(),action:Action::Role})),{i18n.t("tenant.change_role")}}
                                                button {class:"btn btn-secondary btn-sm",disabled:busy(),onclick:move |_|pending.set(Some(Pending{member:suspend_member.clone(),action:Action::Suspend})),{i18n.t("tenant.change_status")}}
                                                button {class:"btn btn-danger btn-sm",disabled:busy(),onclick:move |_|pending.set(Some(Pending{member:remove_member.clone(),action:Action::Remove})),{i18n.t("tenant.remove")}}
                                            }
                                            if can_transfer {button {class:"btn btn-secondary btn-sm",disabled:busy(),onclick:move |_|pending.set(Some(Pending{member:transfer_member.clone(),action:Action::Transfer})),{i18n.t("tenant.transfer")}}}
                                            if removed {span {class:"text-secondary",{i18n.t("tenant.reinvite_required")}}}
                                        }
                                    }}
                                }
                            }
                        }
                    }
                    if value.page.items.is_empty(){p {{i18n.t("tenant.empty")}}}
                }
                Pager {page:page(),total_pages:value.page.total_pages,total:value.page.total,on_page:move |p|page.set(p)}
            },
        }
        if let Some(command)=pending(){CommandDialog {title:i18n.t(command.action.label()).to_string(),target:command.member.email.clone(),busy:busy(),on_cancel:move |_|pending.set(None),on_confirm:confirm}}
    }}
}
