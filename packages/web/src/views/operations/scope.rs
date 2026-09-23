//! Presentation scope only. The server independently validates every request.
use crate::{
    services::api_client::with_auto_refresh,
    stores::{
        auth_store::{AuthState, AuthStore},
        user_store::{UserInfo, UserStore},
    },
};
use client_api::{ClientError, Result, UserStatus};
use dioxus::prelude::*;
use std::future::Future;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OperationsScope {
    pub epoch: Uuid,
    pub user: Uuid,
    pub selected: Option<(Uuid, i64, i64)>,
    pub health: bool,
    pub usage: bool,
    pub capacity: bool,
}
impl OperationsScope {
    pub(super) fn from_profile(
        state: &AuthState,
        loaded: Uuid,
        user: Option<&UserInfo>,
    ) -> Option<Self> {
        if !state.is_authenticated || loaded != state.session_id {
            return None;
        }
        let user = user?;
        if user.status != Some(UserStatus::Active) {
            return None;
        }
        let real_id = |value: &str| Uuid::parse_str(value).ok().filter(|id| !id.is_nil());
        let selected = if let Some(tenant) = &user.selected_tenant {
            if state
                .selected_tenant_id
                .as_deref()
                .is_some_and(|id| id != tenant.id)
            {
                return None;
            }
            Some((
                real_id(&tenant.id)?,
                tenant.authz_version.filter(|v| *v > 0)?,
                tenant.membership_authz_version.filter(|v| *v > 0)?,
            ))
        } else {
            if state.selected_tenant_id.is_some() {
                return None;
            }
            None
        };
        let scope = Self {
            epoch: state.session_id,
            user: real_id(&user.id)?,
            selected,
            health: user.has_platform_permission("platform:tenant_health"),
            usage: user.has_platform_permission("platform:aggregate_stats"),
            capacity: user.has_platform_permission("platform:diagnostics"),
        };
        (scope.health || scope.usage || scope.capacity).then_some(scope)
    }
    pub fn current(auth: AuthStore, users: UserStore) -> Option<Self> {
        Self::from_profile(
            &(auth.state)(),
            (users.loaded_session_id)(),
            (users.info)().as_ref(),
        )
    }
    pub fn is_current(self, auth: AuthStore, users: UserStore) -> bool {
        Self::from_profile(
            &auth.state.peek(),
            *users.loaded_session_id.peek(),
            users.info.peek().as_ref(),
        ) == Some(self)
    }
    pub async fn read<T, F, Fut>(self, auth: AuthStore, users: UserStore, fetch: F) -> Result<T>
    where
        F: Fn(String) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let changed =
            || ClientError::Other("Platform context changed; the old result was discarded".into());
        if !self.is_current(auth, users) {
            return Err(changed());
        }
        let result = with_auto_refresh(auth, fetch).await;
        if !self.is_current(auth, users) {
            return Err(changed());
        }
        result
    }
}
