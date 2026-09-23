//! Presentation identity only; every platform request is independently authorized by the server.
use crate::{
    i18n::I18n,
    stores::{
        auth_store::{AuthState, AuthStore},
        user_store::{UserInfo, UserStore},
    },
};
use client_api::{ClientError, Result, UserStatus, api::admin::PricingTarget};
use dioxus::prelude::*;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PricingIdentity {
    epoch: Uuid,
    user: Uuid,
    selected: Option<(Uuid, i64, i64)>,
}
impl PricingIdentity {
    pub(super) fn from_profile(
        state: &AuthState,
        loaded: Uuid,
        user: Option<&UserInfo>,
    ) -> Option<Self> {
        if !state.is_authenticated || loaded != state.session_id {
            return None;
        }
        let user = user?;
        if user.status != Some(UserStatus::Active) || !user.can_manage_platform() {
            return None;
        }
        let id = |raw: &str| Uuid::parse_str(raw).ok().filter(|v| !v.is_nil());
        let selected = if let Some(tenant) = &user.selected_tenant {
            if state
                .selected_tenant_id
                .as_deref()
                .is_some_and(|v| v != tenant.id)
            {
                return None;
            }
            Some((
                id(&tenant.id)?,
                tenant.authz_version.filter(|v| *v > 0)?,
                tenant.membership_authz_version.filter(|v| *v > 0)?,
            ))
        } else {
            if state.selected_tenant_id.is_some() {
                return None;
            }
            None
        };
        Some(Self {
            epoch: state.session_id,
            user: id(&user.id)?,
            selected,
        })
    }
    pub fn from_stores(auth: AuthStore, users: UserStore) -> Option<Self> {
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
}
pub(super) fn parse_target(mode: &str, tenant: &str) -> Result<PricingTarget> {
    match mode {
        "platform" => Ok(PricingTarget::Platform),
        "tenant" => {
            let tenant_id = Uuid::parse_str(tenant.trim())
                .ok()
                .filter(|v| !v.is_nil())
                .ok_or_else(|| {
                    ClientError::Config("An explicit nonzero tenant UUID is required".into())
                })?;
            Ok(PricingTarget::Tenant { tenant_id })
        }
        _ => Err(ClientError::Config(
            "Choose a supported pricing scope".into(),
        )),
    }
}
pub(super) fn target_label(target: PricingTarget, i18n: &I18n) -> String {
    match target {
        PricingTarget::Platform => i18n.t("platform_pricing.global").into(),
        PricingTarget::Tenant { tenant_id } => tenant_id.to_string(),
    }
}
/// PostgreSQL may render tiny decimals in scientific notation. Never round through f64.
pub(super) fn display_decimal(raw: &str) -> String {
    raw.parse::<rust_decimal::Decimal>()
        .or_else(|_| rust_decimal::Decimal::from_scientific(raw))
        .map(|v| v.normalize().to_string())
        .unwrap_or_else(|_| raw.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use client_api::{
        PlatformRole, TenantRole,
        api::auth::{SelectedTenant, SessionCapabilities},
    };
    fn user() -> UserInfo {
        UserInfo {
            id: Uuid::new_v4().to_string(),
            status: Some(UserStatus::Active),
            platform_role: Some(PlatformRole::None),
            capabilities: SessionCapabilities {
                platform: vec!["users:manage".into()],
                tenant: vec![],
            },
            ..Default::default()
        }
    }
    #[test]
    fn current_platform_capability_not_role_or_tenant_grant_controls_pricing() {
        let auth = AuthState::logged_in("fixture".into());
        let mut u = user();
        assert!(PricingIdentity::from_profile(&auth, auth.session_id, Some(&u)).is_some());
        u.platform_role = Some(PlatformRole::Root);
        u.capabilities.platform.clear();
        u.capabilities.tenant = vec!["tenant:manage".into()];
        assert!(PricingIdentity::from_profile(&auth, auth.session_id, Some(&u)).is_none());
        u.capabilities.platform = vec![
            "platform:tenant_health".into(),
            "platform:diagnostics".into(),
        ];
        assert!(PricingIdentity::from_profile(&auth, auth.session_id, Some(&u)).is_none());
    }
    #[test]
    fn selected_versions_are_required_and_refresh_does_not_create_a_new_identity() {
        let mut auth = AuthState::logged_in("fixture".into());
        let mut u = user();
        let tid = Uuid::new_v4();
        u.selected_tenant = Some(SelectedTenant {
            id: tid.to_string(),
            name: None,
            slug: None,
            tenant_role: TenantRole::Admin,
            authz_version: Some(2),
            membership_authz_version: Some(3),
        });
        let initial = PricingIdentity::from_profile(&auth, auth.session_id, Some(&u)).unwrap();
        auth.token_revision += 1;
        auth.access_token = Some("refreshed".into());
        assert_eq!(
            PricingIdentity::from_profile(&auth, auth.session_id, Some(&u)),
            Some(initial)
        );
        u.selected_tenant.as_mut().unwrap().membership_authz_version = Some(4);
        assert_ne!(
            PricingIdentity::from_profile(&auth, auth.session_id, Some(&u)),
            Some(initial)
        );
        u.selected_tenant.as_mut().unwrap().authz_version = None;
        assert!(PricingIdentity::from_profile(&auth, auth.session_id, Some(&u)).is_none());
        assert!(PricingIdentity::from_profile(&auth, Uuid::new_v4(), Some(&user())).is_none());
    }
    #[test]
    fn invalid_target_never_defaults_to_platform_and_amounts_are_not_rounded() {
        for value in [
            "",
            "all",
            "00000000-0000-0000-0000-000000000000",
            "x&scope_type=platform",
        ] {
            assert!(parse_target("tenant", value).is_err());
        }
        assert!(parse_target("unexpected", "").is_err());
        assert_eq!(
            parse_target("platform", "").unwrap(),
            PricingTarget::Platform
        );
        assert_eq!(display_decimal("1E-10"), "0.0000000001");
        assert_eq!(
            display_decimal("9999999999.9999999999"),
            "9999999999.9999999999"
        );
    }
}
