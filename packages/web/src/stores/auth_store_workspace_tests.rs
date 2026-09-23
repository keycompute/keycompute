use super::*;
use client_api::api::auth::AuthResponse;
use serde_json::json;

fn session(user: Uuid, tenant: Option<Uuid>, token: &str) -> AuthResponse {
    serde_json::from_value(json!({
        "user_id": user, "email": "workspace@example.test", "name": null,
        "status": "active", "platform_role": "none",
        "selected_tenant": tenant.map(|id| json!({
            "id": id, "name": "Selected tenant", "slug": "selected",
            "tenant_role": "member", "authz_version": 1, "membership_authz_version": 1
        })),
        "memberships": [], "capabilities": {"platform": [], "tenant": ["api:use"]},
        "access_token": token, "token_type": "Bearer", "expires_in": 3600
    }))
    .unwrap()
}
fn harness(f: impl FnOnce(AuthStore, Uuid, Uuid)) {
    let mut dom = VirtualDom::new(|| rsx! {div {}});
    dom.rebuild_in_place();
    dom.in_scope(ScopeId::ROOT, || {
        let user = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let mut state = AuthState::logged_in("old-token".into());
        state.selected_tenant_id = Some(tenant.to_string());
        state.persistent = true;
        f(AuthStore::new(Signal::new(state)), user, tenant);
    });
}
#[test]
fn verified_tenant_switch_starts_a_new_epoch_and_preserves_remember_me() {
    harness(|mut auth, user, old_tenant| {
        let old = auth.state.peek().clone();
        let next_tenant = Uuid::new_v4();
        assert!(auth.select_session_if_current(
            &old,
            &user.to_string(),
            Some(&next_tenant.to_string()),
            &session(user, Some(next_tenant), "next-token")
        ));
        let current = auth.state.peek().clone();
        assert_ne!(current.session_id, old.session_id);
        assert!(current.persistent);
        assert_eq!(current.selected_tenant_id, Some(next_tenant.to_string()));
        assert_ne!(current.selected_tenant_id, Some(old_tenant.to_string()));
        assert!(!auth.same_session(&old));
        assert!(!auth.refresh_if_current(&old, "late-refresh".into()));
        assert!(!auth.logout_if_current(&old));
        assert_eq!(auth.token().as_deref(), Some("next-token"));
    });
}
#[test]
fn same_workspace_refresh_does_not_discard_a_successful_selection() {
    harness(|mut auth, user, _| {
        let observed = auth.state.peek().clone();
        assert!(auth.refresh_if_current(&observed, "refreshed".into()));
        assert!(auth.same_session(&observed));
        assert!(!auth.matches(&observed));
        let target = Uuid::new_v4();
        assert!(auth.select_session_if_current(
            &observed,
            &user.to_string(),
            Some(&target.to_string()),
            &session(user, Some(target), "selected")
        ));
        assert_eq!(auth.token().as_deref(), Some("selected"));
    });
}
#[test]
fn another_selection_or_login_never_gets_overwritten_by_a_late_reply() {
    harness(|mut auth, user, _| {
        let observed = auth.state.peek().clone();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        assert!(auth.select_session_if_current(
            &observed,
            &user.to_string(),
            Some(&first.to_string()),
            &session(user, Some(first), "first")
        ));
        assert!(!auth.select_session_if_current(
            &observed,
            &user.to_string(),
            Some(&second.to_string()),
            &session(user, Some(second), "late")
        ));
        assert_eq!(auth.token().as_deref(), Some("first"));
        let observed = auth.state.peek().clone();
        auth.login_with_persist("other-login".into(), false);
        assert!(!auth.select_session_if_current(
            &observed,
            &user.to_string(),
            None,
            &session(user, None, "late-global")
        ));
        assert_eq!(auth.token().as_deref(), Some("other-login"));
        assert!(!auth.state.peek().persistent);
    });
}
#[test]
fn wrong_subject_selector_empty_token_or_nil_identity_never_changes_state() {
    harness(|mut auth, user, target| {
        let observed = auth.state.peek().clone();
        for response in [
            session(Uuid::new_v4(), Some(target), "other-user"),
            session(user, Some(Uuid::new_v4()), "other-tenant"),
            session(user, None, "wrong-global"),
            session(user, Some(target), "   "),
        ] {
            assert!(!auth.select_session_if_current(
                &observed,
                &user.to_string(),
                Some(&target.to_string()),
                &response
            ));
            assert!(*auth.state.peek() == observed);
        }
        assert!(!auth.select_session_if_current(
            &observed,
            &Uuid::nil().to_string(),
            None,
            &session(Uuid::nil(), None, "nil-user")
        ));
        assert!(!auth.select_session_if_current(
            &observed,
            &user.to_string(),
            Some(&Uuid::nil().to_string()),
            &session(user, Some(Uuid::nil()), "nil-tenant")
        ));
        assert!(*auth.state.peek() == observed);
    });
}
#[test]
fn selecting_global_context_never_invents_a_default_tenant() {
    harness(|mut auth, user, _| {
        let observed = auth.state.peek().clone();
        assert!(auth.select_session_if_current(
            &observed,
            &user.to_string(),
            None,
            &session(user, None, "global")
        ));
        assert_eq!(auth.state.peek().selected_tenant_id, None);
        assert_ne!(auth.state.peek().session_id, observed.session_id);
        assert!(auth.state.peek().persistent);
    });
}
#[test]
fn changing_selected_context_alone_also_fences_old_commands_and_refreshes() {
    harness(|mut auth, _, _| {
        let observed = auth.state.peek().clone();
        auth.state.write().selected_tenant_id = Some(Uuid::new_v4().to_string());
        assert_eq!(auth.state.peek().session_id, observed.session_id);
        assert!(!auth.matches(&observed));
        assert!(!auth.same_session(&observed));
        assert!(!auth.refresh_if_current(&observed, "old-refresh".into()));
        assert!(!auth.logout_if_current(&observed));
        assert!(auth.is_authenticated());
    });
}
