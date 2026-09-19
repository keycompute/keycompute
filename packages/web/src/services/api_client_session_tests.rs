use super::*;
use dioxus::prelude::*;
use futures::{channel::oneshot, executor::block_on};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

fn harness(f: impl FnOnce(AuthStore)) {
    let mut dom = VirtualDom::new(|| rsx! { div {} });
    dom.rebuild_in_place();
    dom.in_scope(ScopeId::ROOT, || {
        let mut state = AuthState::logged_in("A".into());
        state.persistent = true;
        f(AuthStore::new(Signal::new(state)));
    });
}

fn client() -> ApiClient {
    let client =
        ApiClient::new(ClientConfig::new("http://127.0.0.1:1").with_no_proxy(true)).unwrap();
    client.set_token("A");
    client
}

#[test]
fn old_command_is_never_replayed_as_a_new_login() {
    harness(|mut auth| {
        block_on(async {
            let client = client();
            let (tx, rx) = oneshot::channel();
            let gate = Rc::new(RefCell::new(Some(rx)));
            let tokens = Rc::new(RefCell::new(Vec::new()));
            let request = with_auto_refresh_using(
                auth,
                client.clone(),
                {
                    let tokens = tokens.clone();
                    move |token| {
                        tokens.borrow_mut().push(token);
                        let gate = gate.borrow_mut().take();
                        async move { gate.expect("no second execution permitted").await.unwrap() }
                    }
                },
                |_| panic!("old login must not initiate refresh"),
            );
            let switch = async {
                auth.login_with_persist("B".into(), false);
                client.set_token("B");
                tx.send(Err::<(), _>(ClientError::Unauthorized("expired A".into())))
                    .unwrap();
            };
            let (result, _) = futures::join!(request, switch);
            assert!(matches!(result, Err(ClientError::Other(_))));
            assert_eq!(&*tokens.borrow(), &["A"]);
            assert_eq!(auth.token().as_deref(), Some("B"));
            assert!(!auth.state.peek().persistent);
        })
    });
}

#[test]
fn successful_old_response_is_not_delivered_to_a_new_login() {
    harness(|mut auth| {
        block_on(async {
            let (tx, rx) = oneshot::channel();
            let gate = RefCell::new(Some(rx));
            let request = with_auto_refresh_using(
                auth,
                client(),
                |_| {
                    let rx = gate.borrow_mut().take().unwrap();
                    async move { rx.await.unwrap() }
                },
                |_| panic!("success cannot refresh"),
            );
            let switch = async {
                auth.login_with_persist("B".into(), false);
                tx.send(Ok("private A data")).unwrap();
            };
            let (result, _) = futures::join!(request, switch);
            assert!(matches!(result, Err(ClientError::Other(_))));
            assert_eq!(auth.token().as_deref(), Some("B"));
        })
    });
}

#[test]
fn concurrent_401s_share_refresh_and_replay_only_within_one_login() {
    harness(|auth| {
        block_on(async {
            let initial = auth.state.peek().clone();
            let client = client();
            let refreshes = Arc::new(AtomicUsize::new(0));
            let attempts = Arc::new(AtomicUsize::new(0));
            let (tx, rx) = oneshot::channel::<()>();
            let gate = Arc::new(Mutex::new(Some(rx)));
            let coordinator = RefreshCoordinator::default();
            let calls = (0..8).map(|_| {
                let attempts = attempts.clone();
                let refreshes = refreshes.clone();
                let gate = gate.clone();
                let coordinator = &coordinator;
                with_auto_refresh_using(
                    auth,
                    client.clone(),
                    move |token| {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        async move {
                            if token == "A" {
                                Err(ClientError::Unauthorized("expired".into()))
                            } else {
                                assert_eq!(token, "A-refreshed");
                                Ok(())
                            }
                        }
                    },
                    move |observed| {
                        let key = RefreshKey {
                            login: observed.session_id,
                            revision: observed.token_revision,
                            token: observed.access_token.unwrap(),
                        };
                        let gate = gate.clone();
                        let refreshes = refreshes.clone();
                        coordinator.run(
                            key,
                            box_refresh_future(async move {
                                refreshes.fetch_add(1, Ordering::SeqCst);
                                let rx = gate.lock().unwrap().take().unwrap();
                                rx.await.unwrap();
                                Ok("A-refreshed".into())
                            }),
                        )
                    },
                )
            });
            let release = async {
                assert_eq!(attempts.load(Ordering::SeqCst), 8);
                tx.send(()).unwrap();
            };
            let (results, _) = futures::join!(futures::future::join_all(calls), release);
            assert!(results.iter().all(|result| result.is_ok()));
            assert_eq!(refreshes.load(Ordering::SeqCst), 1);
            assert_eq!(attempts.load(Ordering::SeqCst), 16);
            assert_eq!(auth.state.peek().session_id, initial.session_id);
            assert_eq!(auth.state.peek().token_revision, initial.token_revision + 1);
            assert!(auth.state.peek().persistent);
            assert_eq!(auth.token().as_deref(), Some("A-refreshed"));
        })
    });
}

#[test]
fn transient_refresh_failure_preserves_session_without_replay() {
    harness(|auth| {
        block_on(async {
            let initial = auth.state.peek().clone();
            let calls = Rc::new(RefCell::new(0));
            let result = with_auto_refresh_using(
                auth,
                client(),
                |_| {
                    *calls.borrow_mut() += 1;
                    async { Err::<(), _>(ClientError::Unauthorized("expired".into())) }
                },
                |_| box_refresh_future(async { Err(ClientError::Network("offline".into())) }),
            )
            .await;
            assert!(matches!(result, Err(ClientError::Network(_))));
            assert_eq!(*calls.borrow(), 1);
            assert!(auth.matches(&initial));
        })
    });
}

#[test]
fn late_refresh_success_and_failure_cannot_replace_or_logout_new_login() {
    for success in [true, false] {
        harness(|mut auth| {
            block_on(async {
                let (tx, rx) = oneshot::channel();
                let gate = Mutex::new(Some(rx));
                let result = with_auto_refresh_using(
                    auth,
                    client(),
                    |_| async { Err::<(), _>(ClientError::Unauthorized("expired".into())) },
                    |_| {
                        let rx = gate.lock().unwrap().take().unwrap();
                        box_refresh_future(async move { rx.await.unwrap() })
                    },
                );
                let switch = async {
                    auth.login_with_persist("B".into(), false);
                    let value = if success {
                        Ok("old-A-refresh".into())
                    } else {
                        Err(ClientError::Unauthorized("A invalid".into()))
                    };
                    tx.send(value).unwrap();
                };
                let (result, _) = futures::join!(result, switch);
                assert!(matches!(result, Err(ClientError::Other(_))));
                assert_eq!(auth.token().as_deref(), Some("B"));
            })
        });
    }
}

#[test]
fn logout_also_fences_inflight_success() {
    harness(|mut auth| {
        block_on(async {
            let (tx, rx) = oneshot::channel();
            let gate = RefCell::new(Some(rx));
            let pending = with_auto_refresh_using(
                auth,
                client(),
                |_| {
                    let rx = gate.borrow_mut().take().unwrap();
                    async move { rx.await.unwrap() }
                },
                |_| panic!("no refresh"),
            );
            let logout = async {
                auth.logout();
                tx.send(Ok(())).unwrap();
            };
            let (result, _) = futures::join!(pending, logout);
            assert!(result.is_err());
            assert!(!auth.is_authenticated());
        })
    });
}
