use super::*;
use crate::stores::auth_store::AuthState;

fn harness(f: impl FnOnce(AuthStore, UserStore)) {
    let mut dom = VirtualDom::new(|| rsx! { div {} });
    dom.rebuild_in_place();
    dom.in_scope(ScopeId::ROOT, || {
        let auth = AuthStore::new(Signal::new(AuthState::logged_in("A".into())));
        let users = UserStore::new(
            Signal::new(None),
            Signal::new(false),
            Signal::new(uuid::Uuid::nil()),
        );
        f(auth, users);
    });
}

fn profile(name: &str) -> UserInfo {
    UserInfo {
        id: name.into(),
        name: Some(name.into()),
        email: format!("{name}@example.test"),
        platform_role: None,
        status: None,
        memberships: Vec::new(),
        selected_tenant: None,
        capabilities: Default::default(),
    }
}

#[test]
fn root_profile_loader_discards_stale_success_and_failure() {
    harness(|mut auth, users| {
        let old = auth.state.peek().clone();
        auth.login_with_persist("B".into(), false);
        let current = auth.state.peek().clone();
        assert!(complete_user_load(auth, users, &current, Ok(profile("B"))));
        assert!(!complete_user_load(auth, users, &old, Ok(profile("A"))));
        assert!(!complete_user_load(
            auth,
            users,
            &old,
            Err(client_api::ClientError::Unauthorized("old A".into()))
        ));
        assert_eq!(users.info.peek().as_ref().unwrap().id, "B");
        assert_eq!(*users.loaded_session_id.peek(), current.session_id);
        assert_eq!(auth.token().as_deref(), Some("B"));
        assert!(!*users.load_failed.peek());
    });
}

#[test]
fn root_loader_discards_old_token_revision_without_resetting_login() {
    harness(|mut auth, users| {
        let old = auth.state.peek().clone();
        assert!(auth.refresh_if_current(&old, "A-new".into()));
        let new = auth.state.peek().clone();
        assert_eq!(new.session_id, old.session_id);
        assert!(complete_user_load(
            auth,
            users,
            &new,
            Ok(profile("current"))
        ));
        assert!(!complete_user_load(
            auth,
            users,
            &old,
            Err(client_api::ClientError::Unauthorized("expired".into()))
        ));
        assert!(!complete_user_load(
            auth,
            users,
            &old,
            Ok(profile("obsolete"))
        ));
        assert_eq!(users.info.peek().as_ref().unwrap().id, "current");
        assert_eq!(auth.token().as_deref(), Some("A-new"));
    });
}

#[test]
fn current_auth_failure_clears_profile_but_transient_failure_does_not_logout() {
    harness(|auth, users| {
        let observed = auth.state.peek().clone();
        assert!(complete_user_load(
            auth,
            users,
            &observed,
            Err(client_api::ClientError::Network("offline".into()))
        ));
        assert!(auth.is_authenticated());
        assert!(*users.load_failed.peek());
        assert!(complete_user_load(
            auth,
            users,
            &observed,
            Err(client_api::ClientError::Unauthorized("revoked".into()))
        ));
        assert!(!auth.is_authenticated());
        assert!(users.info.peek().is_none());
        assert!(users.loaded_session_id.peek().is_nil());
    });
}

#[test]
fn terminal_failure_cas_cannot_logout_a_same_session_refresh() {
    harness(|mut auth, _users| {
        let observed = auth.state.peek().clone();
        assert!(auth.refresh_if_current(&observed, "A-refreshed".into()));
        assert!(!auth.logout_if_current(&observed));
        assert!(auth.is_authenticated());
        assert_eq!(auth.token().as_deref(), Some("A-refreshed"));
    });
}

#[test]
fn explicit_login_with_the_same_token_gets_a_new_private_resource_identity() {
    harness(|mut auth, users| {
        let old = auth.state.peek().clone();
        assert!(complete_user_load(auth, users, &old, Ok(profile("A"))));
        auth.login_with_persist("A".into(), true);
        let new = auth.state.peek().clone();
        assert_ne!(old.session_id, new.session_id);
        assert_ne!(*users.loaded_session_id.peek(), new.session_id);
        assert!(!complete_user_load(auth, users, &old, Ok(profile("stale"))));
    });
}
use futures::{FutureExt, channel::oneshot};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
type BootstrapReply = client_api::Result<(String, UserInfo)>;
#[derive(Default)]
struct BootstrapState {
    auth: Option<AuthStore>,
    users: Option<UserStore>,
    resource: Option<Resource<()>>,
    replies: VecDeque<oneshot::Receiver<BootstrapReply>>,
    calls: usize,
    active: usize,
}
#[derive(Clone)]
struct SharedBootstrap(Rc<RefCell<BootstrapState>>);
impl PartialEq for SharedBootstrap {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}
struct BootstrapActive(SharedBootstrap);
impl Drop for BootstrapActive {
    fn drop(&mut self) {
        self.0.0.borrow_mut().active -= 1;
    }
}
#[component]
fn BootstrapFixture(shared: SharedBootstrap) -> Element {
    let auth = AuthStore::new(use_signal(|| AuthState::logged_in("A".into())));
    let users = UserStore::new(
        use_signal(|| None),
        use_signal(|| false),
        use_signal(uuid::Uuid::nil),
    );
    let transport = shared.clone();
    let resource = use_user_bootstrap(auth, users, move |_| {
        let shared = transport.clone();
        async move {
            let rx = {
                let mut state = shared.0.borrow_mut();
                state.calls += 1;
                state.active += 1;
                state
                    .replies
                    .pop_front()
                    .expect("unexpected extra bootstrap")
            };
            let _active = BootstrapActive(shared);
            rx.await.expect("test reply not dropped")
        }
    });
    {
        let mut state = shared.0.borrow_mut();
        state.auth = Some(auth);
        state.users = Some(users);
        state.resource = Some(resource);
    }
    rsx! { div {} }
}
fn flush_bootstrap(dom: &mut VirtualDom) {
    for _ in 0..20 {
        let _ = dom.wait_for_work().now_or_never();
        dom.render_immediate(&mut dioxus::prelude::dioxus_core::NoOpMutations);
    }
}
fn bootstrap_fixture() -> (
    VirtualDom,
    SharedBootstrap,
    Vec<oneshot::Sender<BootstrapReply>>,
) {
    let state = SharedBootstrap(Rc::new(RefCell::new(BootstrapState::default())));
    let mut senders = Vec::new();
    for _ in 0..3 {
        let (tx, rx) = oneshot::channel();
        state.0.borrow_mut().replies.push_back(rx);
        senders.push(tx);
    }
    let mut dom = VirtualDom::new_with_props(
        BootstrapFixture,
        BootstrapFixtureProps {
            shared: state.clone(),
        },
    );
    dom.rebuild_in_place();
    flush_bootstrap(&mut dom);
    (dom, state, senders)
}
#[test]
fn bootstrap_resource_cancels_previous_session_instead_of_leaking_spawned_tasks() {
    let (mut dom, shared, mut replies) = bootstrap_fixture();
    assert_eq!(shared.0.borrow().calls, 1);
    assert_eq!(shared.0.borrow().active, 1);
    dom.in_scope(ScopeId::ROOT, || {
        shared
            .0
            .borrow()
            .auth
            .unwrap()
            .login_with_persist("B".into(), false)
    });
    flush_bootstrap(&mut dom);
    assert_eq!(shared.0.borrow().calls, 2);
    assert_eq!(shared.0.borrow().active, 1);
    assert!(
        replies
            .remove(0)
            .send(Ok(("A".into(), profile("A"))))
            .is_err(),
        "old receiver must be canceled"
    );
    replies
        .remove(0)
        .send(Ok(("B".into(), profile("B"))))
        .unwrap_or_else(|_| panic!("current reply must have a receiver"));
    flush_bootstrap(&mut dom);
    assert_eq!(shared.0.borrow().active, 0);
    dom.in_scope(ScopeId::ROOT, || {
        assert_eq!(
            shared
                .0
                .borrow()
                .users
                .unwrap()
                .info
                .peek()
                .as_ref()
                .unwrap()
                .id,
            "B"
        )
    });
}
#[test]
fn bootstrap_resource_retry_recovers_without_reloading_or_changing_login() {
    let (mut dom, shared, mut replies) = bootstrap_fixture();
    replies
        .remove(0)
        .send(Err(client_api::ClientError::Network("offline".into())))
        .unwrap_or_else(|_| panic!("first reply must have a receiver"));
    flush_bootstrap(&mut dom);
    let initial = dom.in_scope(ScopeId::ROOT, || {
        let state = shared.0.borrow();
        assert!(*state.users.unwrap().load_failed.peek());
        assert!(state.auth.unwrap().is_authenticated());
        state.auth.unwrap().state.peek().session_id
    });
    dom.in_scope(ScopeId::ROOT, || {
        shared.0.borrow().resource.unwrap().restart()
    });
    flush_bootstrap(&mut dom);
    assert_eq!(shared.0.borrow().calls, 2);
    replies
        .remove(0)
        .send(Ok(("A".into(), profile("A"))))
        .unwrap_or_else(|_| panic!("retry must have a receiver"));
    flush_bootstrap(&mut dom);
    dom.in_scope(ScopeId::ROOT, || {
        let state = shared.0.borrow();
        assert!(!*state.users.unwrap().load_failed.peek());
        assert_eq!(*state.users.unwrap().loaded_session_id.peek(), initial);
        assert_eq!(state.users.unwrap().info.peek().as_ref().unwrap().id, "A");
    });
}
