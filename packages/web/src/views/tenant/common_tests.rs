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
