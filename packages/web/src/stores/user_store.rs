use client_api::api::auth::{SelectedTenant, SessionCapabilities, TenantMembership};
use client_api::{PlatformRole, UserStatus};
use dioxus::prelude::*;

/// 当前用户信息
#[derive(Clone, PartialEq, Default)]
pub struct UserInfo {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    pub platform_role: Option<PlatformRole>,
    pub status: Option<UserStatus>,
    pub memberships: Vec<TenantMembership>,
    pub selected_tenant: Option<SelectedTenant>,
    pub capabilities: SessionCapabilities,
}

impl UserInfo {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.email)
    }

    pub fn has_platform_permission(&self, permission: &str) -> bool {
        self.capabilities
            .platform
            .iter()
            .any(|value| value == permission)
    }

    /// Gate the existing root business-console pages using the platform vector.
    /// Tenant capabilities and role labels never imply this capability.
    pub fn can_manage_platform(&self) -> bool {
        self.has_platform_permission("users:manage")
    }

    pub fn active_tenant_id(&self) -> Option<&str> {
        self.selected_tenant
            .as_ref()
            .map(|tenant| tenant.id.as_str())
    }

    pub fn avatar_char(&self) -> char {
        self.display_name()
            .chars()
            .next()
            .map(|c| c.to_uppercase().next().unwrap_or(c))
            .unwrap_or('U')
    }
}

/// 用户信息 Store
#[derive(Clone, Copy)]
pub struct UserStore {
    pub info: Signal<Option<UserInfo>>,
    pub load_failed: Signal<bool>,
    /// Identity that owns the loaded profile; never render it for another login.
    pub loaded_session_id: Signal<uuid::Uuid>,
}

impl UserStore {
    /// 创建新的 UserStore。
    /// 注意：Signal 必须在组件顶层创建后传入
    pub fn new(
        info: Signal<Option<UserInfo>>,
        load_failed: Signal<bool>,
        loaded_session_id: Signal<uuid::Uuid>,
    ) -> Self {
        Self {
            info,
            load_failed,
            loaded_session_id,
        }
    }

    #[allow(dead_code)]
    pub fn set(&mut self, user: UserInfo) {
        *self.info.write() = Some(user);
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        *self.info.write() = None;
        self.load_failed.set(false);
        self.loaded_session_id.set(uuid::Uuid::nil());
    }

    #[allow(dead_code)]
    pub fn get(&self) -> Option<UserInfo> {
        (self.info)()
    }

    pub fn can_manage_platform(&self) -> bool {
        (self.info)()
            .as_ref()
            .map(UserInfo::can_manage_platform)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;
    fn user(role: PlatformRole, platform: &[&str], tenant: &[&str]) -> UserInfo {
        UserInfo {
            platform_role: Some(role),
            capabilities: SessionCapabilities {
                platform: platform.iter().map(|v| (*v).into()).collect(),
                tenant: tenant.iter().map(|v| (*v).into()).collect(),
            },
            ..Default::default()
        }
    }
    #[test]
    fn tenant_admin_capabilities_never_enable_platform_business_pages() {
        for role in [
            PlatformRole::None,
            PlatformRole::Operator,
            PlatformRole::Root,
        ] {
            let tenant_admin = user(
                role,
                &[],
                &[
                    "tenant:manage",
                    "users:manage",
                    "providers:manage",
                    "billing:manage",
                    "pricing:manage",
                ],
            );
            assert!(!tenant_admin.can_manage_platform());
        }
    }
    #[test]
    fn console_access_and_operator_allowlist_are_not_root_business_capabilities() {
        for capabilities in [
            vec!["console:access"],
            vec![
                "platform:tenant_health",
                "platform:diagnostics",
                "platform:aggregate_stats",
                "platform:node_operations",
            ],
        ] {
            assert!(!user(PlatformRole::Operator, &capabilities, &[]).can_manage_platform());
        }
    }
    #[test]
    fn current_platform_capability_is_required_without_role_text_fallback() {
        let mut root = user(PlatformRole::Root, &["users:manage"], &[]);
        assert!(root.can_manage_platform());
        root.capabilities.platform.clear();
        assert!(!root.can_manage_platform());
        assert!(!UserInfo::default().can_manage_platform());
        // UI consumes the verified capabilities, not an independently interpreted role.
        assert!(user(PlatformRole::None, &["users:manage"], &[]).can_manage_platform());
    }
    #[test]
    fn store_gate_reacts_to_the_current_profile_and_denies_missing_profiles() {
        let mut dom = VirtualDom::new(|| rsx! {div{}});
        dom.rebuild_in_place();
        dom.in_scope(ScopeId::ROOT, || {
            let mut store = UserStore::new(
                Signal::new(None),
                Signal::new(false),
                Signal::new(uuid::Uuid::nil()),
            );
            assert!(!store.can_manage_platform());
            store.set(user(PlatformRole::Root, &["users:manage"], &[]));
            assert!(store.can_manage_platform());
            store.set(user(PlatformRole::None, &[], &["tenant:manage"]));
            assert!(!store.can_manage_platform());
            store.clear();
            assert!(!store.can_manage_platform());
        });
    }
}
