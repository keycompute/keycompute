use super::{capacity::projection, health, scope::OperationsScope, usage};
use crate::stores::{
    auth_store::{AuthState, AuthStore},
    user_store::{UserInfo, UserStore},
};
use client_api::{PlatformRole, TenantRole, UserStatus, api::auth::SelectedTenant};
use dioxus::prelude::*;
use futures::{channel::oneshot, executor::block_on};
use serde_json::json;
use std::cell::RefCell;
use uuid::Uuid;
fn profile() -> UserInfo {
    let mut u = UserInfo {
        id: Uuid::new_v4().to_string(),
        status: Some(UserStatus::Active),
        ..Default::default()
    };
    u.capabilities.platform = vec![
        "platform:tenant_health".into(),
        "platform:diagnostics".into(),
        "platform:aggregate_stats".into(),
    ];
    u
}
fn harness(f: impl FnOnce(AuthStore, UserStore, OperationsScope)) {
    let mut dom = VirtualDom::new(|| rsx! {div{}});
    dom.rebuild_in_place();
    dom.in_scope(ScopeId::ROOT, || {
        let state = AuthState::logged_in("test-operator".into());
        let id = state.session_id;
        let auth = AuthStore::new(Signal::new(state));
        let users = UserStore::new(
            Signal::new(Some(profile())),
            Signal::new(false),
            Signal::new(id),
        );
        f(auth, users, OperationsScope::current(auth, users).unwrap());
    });
}
#[test]
fn platform_capabilities_not_role_labels_or_memberships_select_the_operations_panels() {
    let state = AuthState::logged_in("fixture".into());
    let mut u = profile();
    for role in [
        PlatformRole::Root,
        PlatformRole::Operator,
        PlatformRole::None,
    ] {
        u.platform_role = Some(role);
        assert!(OperationsScope::from_profile(&state, state.session_id, Some(&u)).is_some());
    }
    u.capabilities.tenant = u.capabilities.platform.clone();
    u.capabilities.platform.clear();
    assert!(!u.can_view_operations());
    assert!(OperationsScope::from_profile(&state, state.session_id, Some(&u)).is_none());
    for (cap, flags) in [
        ("platform:tenant_health", (true, false, false)),
        ("platform:aggregate_stats", (false, true, false)),
        ("platform:diagnostics", (false, false, true)),
    ] {
        u.capabilities.platform = vec![cap.into()];
        let s = OperationsScope::from_profile(&state, state.session_id, Some(&u)).unwrap();
        assert_eq!((s.health, s.usage, s.capacity), flags);
    }
}
#[test]
fn operations_accept_global_identity_but_not_stale_or_inconsistent_selected_profiles() {
    let mut state = AuthState::logged_in("fixture".into());
    let mut u = profile();
    assert!(OperationsScope::from_profile(&state, Uuid::new_v4(), Some(&u)).is_none());
    assert!(OperationsScope::from_profile(&state, state.session_id, None).is_none());
    u.status = Some(UserStatus::Suspended);
    assert!(OperationsScope::from_profile(&state, state.session_id, Some(&u)).is_none());
    u.status = Some(UserStatus::Active);
    u.id = Uuid::nil().to_string();
    assert!(OperationsScope::from_profile(&state, state.session_id, Some(&u)).is_none());
    u.id = Uuid::new_v4().to_string();
    let t = Uuid::new_v4();
    u.selected_tenant = Some(SelectedTenant {
        id: t.to_string(),
        name: None,
        slug: None,
        tenant_role: TenantRole::Member,
        authz_version: Some(1),
        membership_authz_version: Some(2),
    });
    assert_eq!(
        OperationsScope::from_profile(&state, state.session_id, Some(&u))
            .unwrap()
            .selected,
        Some((t, 1, 2))
    );
    state.selected_tenant_id = Some(Uuid::new_v4().to_string());
    assert!(OperationsScope::from_profile(&state, state.session_id, Some(&u)).is_none());
    state.selected_tenant_id = Some(t.to_string());
    u.selected_tenant.as_mut().unwrap().membership_authz_version = None;
    assert!(OperationsScope::from_profile(&state, state.session_id, Some(&u)).is_none());
}
#[test]
fn delayed_operation_data_is_discarded_on_capability_loss_or_new_login() {
    for new_login in [false, true] {
        harness(|mut auth, mut users, scope| {
            block_on(async {
                let (tx, rx) = oneshot::channel();
                let rx = RefCell::new(Some(rx));
                let read = scope.read(auth, users, |_| {
                    let rx = rx.borrow_mut().take().unwrap();
                    async move { rx.await.unwrap() }
                });
                let change = async {
                    if new_login {
                        auth.login_with_persist("different".into(), false);
                    } else {
                        users
                            .info
                            .write()
                            .as_mut()
                            .unwrap()
                            .capabilities
                            .platform
                            .clear();
                    }
                    tx.send(Ok("old private result")).unwrap();
                };
                let (result, ()) = futures::join!(read, change);
                assert!(result.is_err());
            })
        });
    }
}
#[test]
fn same_workspace_refresh_preserves_platform_read_identity() {
    harness(|mut auth, users, scope| {
        block_on(async {
            let old = auth.state.peek().clone();
            let (tx, rx) = oneshot::channel();
            let rx = RefCell::new(Some(rx));
            let read = scope.read(auth, users, |_| {
                let rx = rx.borrow_mut().take().unwrap();
                async move { rx.await.unwrap() }
            });
            let refresh = async {
                assert!(auth.refresh_if_current(&old, "refreshed".into()));
                tx.send(Ok(7)).unwrap();
            };
            let (result, ()) = futures::join!(read, refresh);
            assert_eq!(result.unwrap(), 7);
        })
    });
}
#[test]
fn operational_search_and_window_validation_cannot_turn_bad_tenant_into_global_query() {
    let from = "2026-09-01T00:00";
    let to = "2026-10-02T00:00";
    let id = Uuid::new_v4().to_string();
    assert!(usage::Filter::parse("tenant", &id, from, to).is_some());
    for tenant in ["", "not-a-uuid", "00000000-0000-0000-0000-000000000000"] {
        assert!(usage::Filter::parse("tenant", tenant, from, to).is_none());
    }
    assert!(usage::Filter::parse("platform", "", from, "2026-10-02T00:01").is_none());
    assert!(usage::Filter::parse("platform", "", from, from).is_none());
    assert!(usage::Filter::parse("platform", "", to, from).is_none());
    let f = usage::Filter::parse(
        "platform",
        "ignored",
        "2026-09-01T00:00",
        "2026-09-02T00:00",
    )
    .unwrap();
    assert_eq!(f.from, "2026-09-01T00:00:00+00:00");
    for s in ["a\nb".into(), "中".repeat(43)] {
        assert!(
            health::Filter {
                search: s,
                ..Default::default()
            }
            .query()
            .is_none()
        );
    }
    assert!(
        health::Filter {
            status: "suspended".into(),
            ..Default::default()
        }
        .query()
        .is_none()
    );
    assert!(
        health::Filter {
            offset: 1_000_001,
            ..Default::default()
        }
        .query()
        .is_none()
    );
    let literal = "%_&tenant_id=foreign";
    assert_eq!(
        health::Filter {
            search: literal.into(),
            ..Default::default()
        }
        .query()
        .unwrap()
        .search
        .as_deref(),
        Some(literal)
    );
}
#[test]
fn capacity_projection_never_dumps_unexpected_fields_or_coerces_strings_to_numbers() {
    let v = json!({"writer_pool":{"connections":7,"idle":null,"url":"secret"},"redis_cache":{"connections":"secret"},"shutdown":"secret","stages":[{"tenant":"secret"}],"managed_payload_bytes":{"limit":18446744073709551615u64}});
    let rows = projection(&v);
    assert_eq!(rows.len(), 17);
    assert!(rows.contains(&("writer_pool.connections".into(), "7".into())));
    assert!(rows.contains(&("writer_pool.idle".into(), "—".into())));
    assert!(rows.contains(&(
        "managed_payload_bytes.limit".into(),
        "18446744073709551615".into()
    )));
    assert!(!format!("{rows:?}").contains("secret"));
}
#[test]
fn operations_route_is_independent_from_root_business_guard_and_translations_exist() {
    use crate::router::Route;
    use std::str::FromStr;
    assert_eq!(
        Route::from_str("/platform/operations").unwrap(),
        Route::PlatformOperations {}
    );
    assert_eq!(
        Route::PlatformOperations {}.to_string(),
        "/platform/operations"
    );
    for &(key, _, _) in crate::i18n::operations::TEXT {
        assert!(crate::i18n::ZH.contains_key(key));
        assert!(crate::i18n::EN.contains_key(key));
    }
}
