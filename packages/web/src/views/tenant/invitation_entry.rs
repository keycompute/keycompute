//! The link token is captured before Router construction and never persisted.
use crate::{
    app::UserBootstrap,
    hooks::use_i18n::use_i18n,
    router::Route,
    services::api_client::{get_client, user_error_message},
    stores::{
        auth_store::{AuthState, AuthStore},
        user_store::UserStore,
    },
};
use client_api::api::tenant_control::{InvitationToken, accept_invitation};
use dioxus::prelude::*;
use uuid::Uuid;
#[derive(Clone, Default, PartialEq)]
pub struct PendingState {
    token: Option<InvitationToken>,
    bound_session: Option<Uuid>,
    invalid: bool,
    pub(crate) scrub_failed: bool,
}
impl PendingState {
    #[cfg(any(test, target_arch = "wasm32"))]
    pub fn from_fragment(fragment: &str) -> Self {
        match InvitationToken::from_fragment(fragment) {
            Ok(token) => Self {
                token: Some(token),
                ..Default::default()
            },
            Err(_) => Self {
                invalid: true,
                ..Default::default()
            },
        }
    }
    pub fn bind(&mut self, auth: &AuthState, verified: bool) {
        if self
            .bound_session
            .is_some_and(|id| !auth.is_authenticated || auth.session_id != id)
        {
            self.token = None;
            self.bound_session = None;
        } else if verified
            && auth.is_authenticated
            && self.token.is_some()
            && self.bound_session.is_none()
        {
            self.bound_session = Some(auth.session_id);
        }
    }
    pub fn take(&mut self, auth: &AuthState) -> Option<InvitationToken> {
        if !auth.is_authenticated || self.bound_session.is_some_and(|id| id != auth.session_id) {
            return None;
        }
        self.bound_session = Some(auth.session_id);
        self.token.take()
    }
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }
}
#[derive(Clone, Copy)]
pub struct PendingInvitation(pub Signal<PendingState>);
pub fn capture_before_router() -> PendingState {
    #[cfg(all(target_arch = "wasm32", not(test)))]
    {
        let Some(window) = web_sys::window() else {
            return PendingState::default();
        };
        let Ok(path) = window.location().pathname() else {
            return PendingState::default();
        };
        if path.trim_end_matches('/') != "/invite" {
            return PendingState::default();
        }
        let fragment = window.location().hash().unwrap_or_default();
        // No secret is handed to components unless browser-history scrubbing succeeds.
        let scrubbed = window.history().ok().is_some_and(|history| {
            history
                .replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&path))
                .is_ok()
        });
        if !scrubbed {
            return PendingState {
                scrub_failed: true,
                invalid: true,
                ..Default::default()
            };
        }
        return PendingState::from_fragment(&fragment);
    }
    #[cfg(any(not(target_arch = "wasm32"), test))]
    {
        PendingState::default()
    }
}
/// Login resumes a captured invitation without accepting it automatically.
/// A capability bound to another UI identity is never adopted after login.
pub fn post_login_route(auth: AuthStore) -> Route {
    let state = auth.state.peek();
    let pending = try_consume_context::<PendingInvitation>();
    if state.is_authenticated
        && pending.is_some_and(|p| {
            let value = p.0.peek();
            value.has_token() && value.bound_session.is_none_or(|id| id == state.session_id)
        })
    {
        Route::TenantInvitationAccept {}
    } else {
        Route::Dashboard {}
    }
}

#[component]
pub fn TenantInvitationAccept() -> Element {
    let auth = use_context::<AuthStore>();
    let epoch = (auth.state)().session_id;
    rsx! {
        ui::ThemeStyles {}
        for identity in [epoch] { InvitationAcceptPage { key: "{identity}" } }
    }
}

#[component]
fn InvitationAcceptPage() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut invitation = use_context::<PendingInvitation>();
    let mut bootstrap = use_context::<UserBootstrap>();
    let i18n = use_i18n();
    let mut busy = use_signal(|| false);
    let mut message = use_signal(String::new);
    let mut failed = use_signal(|| false);
    let observed = (auth.state)();
    let ready = observed.is_authenticated && (users.loaded_session_id)() == observed.session_id;
    let user = if ready { (users.info)() } else { None };
    let accept = move |_| {
        if busy() {
            return;
        }
        let observed = auth.state.peek().clone();
        if !observed.is_authenticated || *users.loaded_session_id.peek() != observed.session_id {
            return;
        }
        let Some(user) = users.info.peek().clone() else {
            return;
        };
        let Some(token) = observed.access_token.clone() else {
            return;
        };
        let Some(secret) = invitation.0.write().take(&observed) else {
            return;
        };
        busy.set(true);
        message.set(String::new());
        failed.set(false);
        spawn(async move {
            // Acceptance is never automatically replayed, including authentication failures.
            let result = accept_invitation(&get_client(), &secret, &token).await;
            if !auth.same_session(&observed) {
                return;
            }
            busy.set(false);
            match result {
                Ok(result) if result.membership.user_id.to_string() == user.id => {
                    message.set(i18n.t("tenant.accepted").into());
                    bootstrap.0.restart();
                }
                Ok(_) => {
                    failed.set(true);
                    message.set(i18n.t("tenant.session_changed").into());
                }
                Err(e) => {
                    failed.set(true);
                    message.set(user_error_message(&e));
                }
            }
        });
    };
    rsx! {main {class:"page-container tenant-invitation-accept",
        ui::PageHeader {title:i18n.t("tenant.accept_title").to_string(),description:i18n.t("tenant.accept_hint").to_string()}
        if !message().is_empty(){div {class:if failed(){"alert alert-error"}else{"alert alert-success"},role:"status","{message}"}}
        if invitation.0.read().invalid {p {role:"alert",{i18n.t("tenant.invalid_invitation")}}}
        if !observed.is_authenticated {
            p {{i18n.t("tenant.invite_login")}}
            Link {class:"btn btn-primary",to:Route::Login {},{i18n.t("auth.login")}}
        } else if !ready {p {role:"status",{i18n.t("common.loading")}}}
        else {
            if let Some(user)=user {p {{i18n.t("tenant.accept_as")} ": {user.email}"}}
            button {class:"btn btn-primary",disabled:busy()||!invitation.0.read().has_token(),onclick:accept,{i18n.t("tenant.accept")}}
            Link {class:"btn btn-secondary",to:Route::TenantWorkspace {},{i18n.t("tenant.workspace")}}
            button {class:"btn btn-secondary",disabled:busy(),onclick:move |_|bootstrap.0.restart(),{i18n.t("tenant.reload_memberships")}}
        }
        p {class:"text-secondary",{i18n.t("tenant.accept_no_retry")}}
    }}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invitation_survives_first_login_but_not_account_or_workspace_changes() {
        let mut pending = PendingState::from_fragment(&format!("#token={}", "a".repeat(64)));
        pending.bind(&AuthState::default(), false);
        assert!(pending.has_token());
        let first = AuthState::logged_in("one".into());
        pending.bind(&first, true);
        assert!(pending.has_token());
        let mut refreshed = first.clone();
        refreshed.token_revision += 1;
        pending.bind(&refreshed, true);
        assert!(pending.has_token());
        let other = AuthState::logged_in("two".into());
        pending.bind(&other, true);
        assert!(!pending.has_token());
    }
    #[test]
    fn expired_unverified_restoration_preserves_invitation_until_verified_login() {
        let mut pending = PendingState::from_fragment(&format!("#token={}", "a".repeat(64)));
        let restored = AuthState::logged_in("expired-restored-session".into());
        pending.bind(&restored, false);
        assert!(pending.bound_session.is_none());
        pending.bind(&AuthState::default(), false);
        assert!(pending.has_token());
        let verified = AuthState::logged_in("verified-login".into());
        pending.bind(&verified, true);
        assert_eq!(pending.bound_session, Some(verified.session_id));
        assert!(pending.take(&verified).is_some());
        assert!(pending.take(&verified).is_none());
    }

    #[test]
    fn invitation_take_is_one_shot_and_requires_an_authenticated_bound_session() {
        let mut pending = PendingState::from_fragment(&format!("#token={}", "a".repeat(64)));
        assert!(pending.take(&AuthState::default()).is_none());
        let first = AuthState::logged_in("one".into());
        pending.bind(&first, true);
        assert!(pending.take(&AuthState::logged_in("two".into())).is_none());
        assert!(pending.take(&first).is_some());
        assert!(pending.take(&first).is_none());
        assert!(PendingState::from_fragment("#token=invalid").invalid);
    }
}
