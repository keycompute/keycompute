use super::*;
use crate::stores::{auth_store::AuthState, user_store::UserInfo};
use client_api::api::auth::{SelectedTenant, SessionCapabilities};
use client_api::{PlatformRole, TenantRole};
use futures::{channel::oneshot, executor::block_on};
use std::cell::{Cell, RefCell};

fn profile() -> UserInfo {
    UserInfo {
        id: Uuid::new_v4().to_string(),
        selected_tenant: Some(SelectedTenant {
            id: Uuid::new_v4().to_string(),
            name: Some("fixture".into()),
            slug: None,
            tenant_role: TenantRole::Admin,
            authz_version: Some(4),
            membership_authz_version: Some(7),
        }),
        capabilities: SessionCapabilities {
            platform: vec![],
            tenant: vec!["tenant:manage".into()],
        },
        ..Default::default()
    }
}
fn harness(f: impl FnOnce(AuthStore, UserStore)) {
    let mut dom = VirtualDom::new(|| rsx! { div {} });
    dom.rebuild_in_place();
    dom.in_scope(ScopeId::ROOT, || {
        let state = AuthState::logged_in("fixture-session".into());
        let id = state.session_id;
        f(
            AuthStore::new(Signal::new(state)),
            UserStore::new(
                Signal::new(Some(profile())),
                Signal::new(false),
                Signal::new(id),
            ),
        );
    });
}
#[test]
fn restored_profile_is_authoritative_but_stale_or_conflicting_profiles_are_not() {
    harness(|mut auth, mut users| {
        assert!(auth.state.peek().selected_tenant_id.is_none());
        let scope = WorkspaceScope::from_stores(auth, users).unwrap();
        assert!(scope.is_current(auth, users));
        users.loaded_session_id.set(Uuid::new_v4());
        assert!(WorkspaceScope::from_stores(auth, users).is_none());
        users.loaded_session_id.set(auth.state.peek().session_id);
        auth.state.write().selected_tenant_id = Some(Uuid::new_v4().to_string());
        assert!(!scope.is_current(auth, users));
    });
}
#[test]
fn neither_global_roles_nor_platform_capabilities_create_a_tenant_membership() {
    for role in [
        PlatformRole::Root,
        PlatformRole::Operator,
        PlatformRole::None,
    ] {
        let user = UserInfo {
            platform_role: Some(role),
            capabilities: SessionCapabilities {
                platform: vec!["users:manage".into(), "tenant:manage".into()],
                tenant: vec![],
            },
            ..Default::default()
        };
        assert!(!user.can_manage_tenant());
        let state = AuthState::logged_in("fixture".into());
        assert!(WorkspaceScope::from_profile(&state, state.session_id, Some(&user)).is_none());
    }
    let mut user = profile();
    user.capabilities.tenant.clear();
    assert!(
        !user.can_manage_tenant(),
        "an admin role label alone is not a capability"
    );
}

#[test]
fn platform_target_scope_requires_a_current_global_platform_session() {
    harness(|mut auth, mut users| {
        let user_id = Uuid::new_v4();
        users.info.set(Some(UserInfo {
            id: user_id.to_string(),
            platform_role: Some(PlatformRole::Operator),
            capabilities: SessionCapabilities {
                platform: vec!["users:manage".into()],
                tenant: vec![],
            },
            ..Default::default()
        }));
        let tenant_id = Uuid::new_v4();
        let scope = WorkspaceScope::platform(auth, users, tenant_id).unwrap();
        assert!(scope.is_platform_target());
        assert_eq!(scope.tenant_id, tenant_id);
        assert_eq!(scope.user_id, user_id);
        assert!(scope.is_current(auth, users));

        auth.state.write().selected_tenant_id = Some(Uuid::new_v4().to_string());
        assert!(!scope.is_current(auth, users));
        auth.state.write().selected_tenant_id = None;
        users.info.write().as_mut().unwrap().selected_tenant = profile().selected_tenant;
        assert!(!scope.is_current(auth, users));
    });
}
#[test]
fn valid_scope_requires_real_user_tenant_and_positive_versions() {
    let state = AuthState::logged_in("fixture".into());
    for bad in 0..5 {
        let mut user = profile();
        match bad {
            0 => user.id = Uuid::nil().to_string(),
            1 => user.selected_tenant.as_mut().unwrap().id = Uuid::nil().to_string(),
            2 => user.selected_tenant.as_mut().unwrap().authz_version = None,
            3 => {
                user.selected_tenant
                    .as_mut()
                    .unwrap()
                    .membership_authz_version = Some(0)
            }
            _ => user.selected_tenant = None,
        }
        assert!(WorkspaceScope::from_profile(&state, state.session_id, Some(&user)).is_none());
    }
}
#[test]
fn tenant_commands_are_never_automatically_replayed_even_after_authentication_errors() {
    harness(|auth, users| {
        block_on(async {
            let scope = WorkspaceScope::from_stores(auth, users).unwrap();
            for error in [
                ClientError::Unauthorized("expired".into()),
                ClientError::Network("uncertain".into()),
                ClientError::ServiceUnavailable("unavailable".into()),
            ] {
                let calls = Cell::new(0);
                let result = command(auth, users, scope, |token| {
                    calls.set(calls.get() + 1);
                    assert_eq!(token, "fixture-session");
                    async move { Err::<(), _>(error) }
                })
                .await;
                assert!(result.is_err());
                assert_eq!(calls.get(), 1);
            }
        })
    });
}
#[test]
fn private_command_results_are_discarded_after_workspace_or_membership_changes() {
    for change_membership in [false, true] {
        harness(|mut auth, mut users| {
            block_on(async {
                let scope = WorkspaceScope::from_stores(auth, users).unwrap();
                let (tx, rx) = oneshot::channel();
                let request = command(auth, users, scope, |_| async { rx.await.unwrap() });
                let switch = async {
                    if change_membership {
                        users
                            .info
                            .write()
                            .as_mut()
                            .unwrap()
                            .selected_tenant
                            .as_mut()
                            .unwrap()
                            .membership_authz_version = Some(8);
                    } else {
                        auth.login_with_persist("other-login".into(), false);
                    }
                    tx.send(Ok("PRIVATE-INVITATION-LINK")).unwrap();
                };
                let (result, _) = futures::join!(request, switch);
                assert!(matches!(result, Err(ClientError::Other(_))));
            })
        });
    }
}
#[test]
fn same_workspace_token_refresh_keeps_the_original_command_result() {
    harness(|mut auth, users| {
        block_on(async {
            let scope = WorkspaceScope::from_stores(auth, users).unwrap();
            let (tx, rx) = oneshot::channel();
            let request = command(auth, users, scope, |_| async { rx.await.unwrap() });
            let refresh = async {
                let observed = auth.state.peek().clone();
                assert!(auth.refresh_if_current(&observed, "refreshed".into()));
                tx.send(Ok(42)).unwrap();
            };
            let (result, _) = futures::join!(request, refresh);
            assert_eq!(result.unwrap(), 42);
        })
    });
}
#[test]
fn read_completion_cannot_publish_rows_for_another_membership_revision() {
    harness(|auth, mut users| {
        block_on(async {
            let scope = WorkspaceScope::from_stores(auth, users).unwrap();
            let (tx, rx) = oneshot::channel();
            let rx = RefCell::new(Some(rx));
            let request = read(auth, users, scope, |_| {
                let gate = rx.borrow_mut().take().expect("one read");
                async { gate.await.unwrap() }
            });
            let revoke = async {
                users
                    .info
                    .write()
                    .as_mut()
                    .unwrap()
                    .selected_tenant
                    .as_mut()
                    .unwrap()
                    .authz_version = Some(5);
                tx.send(Ok("PRIVATE-MEMBER-ROWS")).unwrap();
            };
            let (result, _) = futures::join!(request, revoke);
            assert!(matches!(result, Err(ClientError::Other(_))));
        })
    });
}

#[test]
fn technical_ids_keep_the_full_value_in_the_accessible_dom() {
    let source = include_str!("common.rs");
    let component = source
        .split_once("pub fn TechnicalId")
        .expect("technical ID component")
        .1
        .split_once("pub fn MemberIdField")
        .expect("bounded component source")
        .0;
    assert!(component.contains("tabindex: \"0\""));
    assert!(component.contains("aria_label: \"{value}\""));
    assert!(component.contains("\"{value}\""));
    assert!(!component.contains("short_id"));

    let settings = include_str!("settings.rs");
    assert!(!settings.contains("owner_user_id"));
    assert!(!settings.contains("short_id"));
}

#[test]
fn member_picker_owns_enter_and_exposes_remote_search_states() {
    let source = include_str!("common.rs");
    let component = source
        .split_once("pub fn MemberIdField")
        .expect("member picker component")
        .1
        .split_once("#[component]")
        .expect("bounded member picker source")
        .0;
    assert!(component.contains("should_submit_member_search(event.key(), event.is_composing())"));
    assert!(component.contains("event.prevent_default()"));
    assert!(component.contains("user_error_message(i18n, error)"));
    assert!(component.contains("i18n.t(\"common.loading\")"));
}

#[test]
fn member_picker_does_not_submit_an_ime_composition_enter() {
    assert!(should_submit_member_search(Key::Enter, false));
    assert!(!should_submit_member_search(Key::Enter, true));
    assert!(!should_submit_member_search(Key::Escape, false));
}

#[test]
fn tenant_pager_delegates_to_the_shared_pagination_contract() {
    let source = include_str!("common.rs");
    let pager = source
        .split_once("pub fn Pager")
        .expect("tenant pager")
        .1
        .split_once("#[component]")
        .expect("pager boundary")
        .0;
    assert!(pager.contains("Pagination {"));
    assert!(pager.contains("show_page_size: false"));
    assert!(pager.contains("common.pagination_summary"));
    assert!(!pager.contains("table-pagination-footer"));
}

#[test]
fn tenant_dialog_sources_use_the_shared_backdrop_contract() {
    for (name, source) in [
        (
            "distribution command",
            include_str!("distribution_admin/command.rs"),
        ),
        ("finance inspector", include_str!("finance/inspector.rs")),
        ("finance controls", include_str!("finance_control/mod.rs")),
        ("key editor", include_str!("key_admin/editor.rs")),
        ("key owner", include_str!("key_admin/owner.rs")),
        ("pricing command", include_str!("pricing_admin/command.rs")),
        ("response editor", include_str!("response_admin/editor.rs")),
        (
            "response inspector",
            include_str!("response_admin/inspector.rs"),
        ),
        ("node command", include_str!("node_admin/command.rs")),
        ("provider admin", include_str!("provider_admin/mod.rs")),
    ] {
        assert!(
            source.contains("modal-backdrop"),
            "{name} lost shared backdrop"
        );
        assert!(
            !source.contains("modal-overlay"),
            "{name} still uses legacy overlay"
        );
    }
}

#[test]
fn custom_dialogs_capture_and_restore_focus() {
    for (name, source) in [
        (
            "platform tenant create",
            include_str!("../shared/tenants.rs"),
        ),
        ("platform pricing", include_str!("../shared/pricing.rs")),
        ("platform accounts", include_str!("../shared/accounts.rs")),
        (
            "passthrough bindings",
            include_str!("../shared/passthrough_bindings.rs"),
        ),
        ("platform users", include_str!("../shared/users.rs")),
        ("tenant dialogs", include_str!("common.rs")),
        (
            "tenant provider dialogs",
            include_str!("provider_admin/mod.rs"),
        ),
    ] {
        assert!(
            source.contains("use_dialog_focus_return"),
            "{name} does not restore focus after closing a custom dialog"
        );
    }
}

#[test]
fn shared_custom_dialogs_expose_escape_and_focus_trap() {
    for (name, source, minimum_escape_handlers) in [
        (
            "platform accounts",
            include_str!("../shared/accounts.rs"),
            3,
        ),
        ("platform pricing", include_str!("../shared/pricing.rs"), 2),
        (
            "passthrough bindings",
            include_str!("../shared/passthrough_bindings.rs"),
            1,
        ),
        ("platform tenants", include_str!("../shared/tenants.rs"), 1),
        ("platform users", include_str!("../shared/users.rs"), 4),
    ] {
        assert!(
            source.matches("Key::Escape").count() >= minimum_escape_handlers,
            "{name} does not expose Escape handling for every custom dialog"
        );
        assert!(
            source.contains("contain_modal_focus"),
            "{name} does not expose the shared focus trap"
        );
    }
}

#[test]
fn row_action_menus_close_after_an_action_is_chosen() {
    let utils = include_str!("../../utils/mod.rs");
    assert!(utils.contains("document.active_element()"));
    assert!(utils.contains("query_selector(\"summary\")"));
    for (name, source) in [
        ("platform tenants", include_str!("../shared/tenants.rs")),
        ("platform accounts", include_str!("../shared/accounts.rs")),
        ("platform users", include_str!("../shared/users.rs")),
        ("platform pricing", include_str!("../shared/pricing.rs")),
        ("tenant nodes", include_str!("node_admin/table.rs")),
        ("tenant pricing", include_str!("pricing_admin/mod.rs")),
        (
            "tenant distribution",
            include_str!("distribution_admin/mod.rs"),
        ),
        ("tenant providers", include_str!("provider_admin/mod.rs")),
        ("tenant responses", include_str!("response_admin/mod.rs")),
    ] {
        assert!(
            source.contains("close_row_action_menus"),
            "{name} leaves native row action details open"
        );
    }
}

#[test]
fn tenant_css_overrides_shared_grid_and_modal_rules_with_specific_selectors() {
    let css = include_str!("../../../assets/main.css");
    for selector in [
        ".filter-grid.tenant-finance-filter-grid",
        ".filter-grid.tenant-key-filter-grid",
        ".filter-grid.tenant-pricing-filter-grid",
    ] {
        assert!(css.contains(selector), "missing {selector}");
    }
    for selector in [
        ".modal-backdrop .modal.tenant-key-editor",
        ".modal-backdrop .modal.tenant-pricing-editor",
        ".modal-backdrop .modal.tenant-distribution-editor",
    ] {
        assert!(css.contains(selector), "missing {selector}");
    }
    assert!(css.contains(".tenant-provider-admin .modal-body > label:not(.checkbox-field)"));
    assert!(!css.contains(
        ".tenant-provider-admin .modal-body > label,\n.tenant-provider-admin .modal-body > label:not(.checkbox-field)"
    ));
}
