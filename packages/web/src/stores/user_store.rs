use client_api::api::auth::{SelectedTenant, SessionCapabilities, TenantMembership};
use client_api::{PlatformRole, TenantRole, UserStatus};
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

    pub fn has_tenant_permission(&self, permission: &str) -> bool {
        self.selected_tenant.is_some()
            && self
                .capabilities
                .tenant
                .iter()
                .any(|value| value == permission)
    }

    /// Gate the existing root business-console pages using the platform vector.
    /// Tenant capabilities and role labels never imply this capability.
    pub fn can_manage_platform(&self) -> bool {
        self.has_platform_permission("users:manage")
    }

    /// Verified tenant capabilities never imply any platform grant.
    pub fn can_manage_tenant(&self) -> bool {
        self.has_tenant_permission("tenant:manage")
    }

    pub fn can_manage_members(&self) -> bool {
        self.has_tenant_permission("members:manage")
    }

    pub fn can_invite_members(&self) -> bool {
        self.has_tenant_permission("invitations:manage")
    }

    pub fn can_manage_providers(&self) -> bool {
        self.has_tenant_permission("providers:manage")
    }

    pub fn can_manage_api_keys(&self) -> bool {
        self.has_tenant_permission("api_keys:manage")
    }

    pub fn can_manage_billing(&self) -> bool {
        self.has_tenant_permission("billing:manage")
    }

    pub fn can_manage_pricing(&self) -> bool {
        self.has_tenant_permission("pricing:manage")
    }

    /// Read-only platform operations are independent of root business management.
    pub fn can_view_operations(&self) -> bool {
        [
            "platform:tenant_health",
            "platform:aggregate_stats",
            "platform:diagnostics",
        ]
        .iter()
        .any(|permission| self.has_platform_permission(permission))
    }

    /// Cross-tenant audit is a dedicated read capability shared by root and operator.
    pub fn can_view_platform_audit(&self) -> bool {
        self.has_platform_permission("platform:audit_read")
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

    /// Keep session-scoped workspace labels coherent after an in-place
    /// configuration edit. Authorization-bearing fields are deliberately
    /// untouched; only server-returned display metadata is synchronized.
    pub fn sync_tenant_display(&mut self, tenant_id: &str, name: &str, slug: &str) -> bool {
        let mut changed = false;
        if let Some(selected) = self
            .selected_tenant
            .as_mut()
            .filter(|selected| selected.id == tenant_id)
        {
            selected.name = Some(name.to_owned());
            selected.slug = Some(slug.to_owned());
            changed = true;
        }
        for membership in self
            .memberships
            .iter_mut()
            .filter(|membership| membership.tenant_id == tenant_id)
        {
            membership.tenant_name = Some(name.to_owned());
            changed = true;
        }
        changed
    }

    /// Make a just-created, currently-owned workspace immediately available
    /// in the header. This is presentation state only: entering it still calls
    /// the server selection endpoint, which returns the authoritative role,
    /// capabilities and authorization revisions. The profile bootstrap then
    /// refreshes this optimistic row in the background.
    pub fn add_created_owned_tenant(&mut self, tenant_id: &str, name: &str) -> bool {
        if self
            .memberships
            .iter()
            .any(|membership| membership.tenant_id == tenant_id)
        {
            return false;
        }
        self.memberships.push(TenantMembership {
            tenant_id: tenant_id.to_owned(),
            tenant_name: Some(name.to_owned()),
            tenant_role: TenantRole::Admin,
            status: Some("active".into()),
            invited_by: None,
            joined_at: None,
            removed_at: None,
            authz_version: None,
        });
        self.memberships.sort_by(|left, right| {
            left.tenant_name
                .as_deref()
                .unwrap_or(&left.tenant_id)
                .to_lowercase()
                .cmp(
                    &right
                        .tenant_name
                        .as_deref()
                        .unwrap_or(&right.tenant_id)
                        .to_lowercase(),
                )
        });
        true
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
    fn audit_read_is_independent_of_root_business_management() {
        let operator = user(PlatformRole::Operator, &["platform:audit_read"], &[]);
        assert!(operator.can_view_platform_audit());
        assert!(!operator.can_manage_platform());
        let tenant_admin = user(
            PlatformRole::None,
            &[],
            &["tenant:manage", "members:manage"],
        );
        assert!(!tenant_admin.can_view_platform_audit());
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
    fn tenant_navigation_capabilities_remain_independent() {
        let mut tenant_user = user(PlatformRole::None, &[], &["providers:manage"]);
        tenant_user.selected_tenant = Some(SelectedTenant {
            id: uuid::Uuid::new_v4().to_string(),
            name: Some("Workspace".into()),
            slug: Some("workspace".into()),
            tenant_role: client_api::TenantRole::Admin,
            authz_version: Some(1),
            membership_authz_version: Some(1),
        });
        assert!(tenant_user.can_manage_providers());
        assert!(!tenant_user.can_manage_tenant());
        assert!(!tenant_user.can_manage_members());
        assert!(!tenant_user.can_invite_members());
        assert!(!tenant_user.can_manage_billing());
        assert!(!tenant_user.can_manage_pricing());
        assert!(!tenant_user.can_manage_api_keys());
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

    #[test]
    fn configuration_save_synchronizes_workspace_labels_without_touching_authority() {
        let tenant_id = uuid::Uuid::new_v4().to_string();
        let mut value = user(PlatformRole::None, &[], &["tenant:manage"]);
        value.selected_tenant = Some(SelectedTenant {
            id: tenant_id.clone(),
            name: Some("Before".into()),
            slug: Some("before".into()),
            tenant_role: client_api::TenantRole::Admin,
            authz_version: Some(7),
            membership_authz_version: Some(9),
        });
        value
            .memberships
            .push(client_api::api::auth::TenantMembership {
                tenant_id: tenant_id.clone(),
                tenant_name: Some("Before".into()),
                tenant_role: client_api::TenantRole::Admin,
                status: Some("active".into()),
                invited_by: None,
                joined_at: None,
                removed_at: None,
                authz_version: Some(9),
            });

        assert!(value.sync_tenant_display(&tenant_id, "After", "after"));
        let selected = value.selected_tenant.as_ref().unwrap();
        assert_eq!(selected.name.as_deref(), Some("After"));
        assert_eq!(selected.slug.as_deref(), Some("after"));
        assert_eq!(selected.authz_version, Some(7));
        assert_eq!(selected.membership_authz_version, Some(9));
        assert_eq!(value.memberships[0].tenant_name.as_deref(), Some("After"));
        assert_eq!(value.memberships[0].authz_version, Some(9));
    }

    #[test]
    fn a_created_owned_tenant_is_immediately_visible_without_granting_authority() {
        let tenant_id = uuid::Uuid::new_v4().to_string();
        let mut value = UserInfo::default();

        assert!(value.add_created_owned_tenant(&tenant_id, "Default"));
        assert!(!value.add_created_owned_tenant(&tenant_id, "Duplicate"));
        assert_eq!(value.memberships.len(), 1);
        assert_eq!(value.memberships[0].tenant_id, tenant_id);
        assert_eq!(value.memberships[0].tenant_name.as_deref(), Some("Default"));
        assert_eq!(value.memberships[0].tenant_role, TenantRole::Admin);
        assert_eq!(value.memberships[0].status.as_deref(), Some("active"));
        assert_eq!(value.memberships[0].authz_version, None);
        assert!(value.selected_tenant.is_none());
        assert!(value.capabilities.tenant.is_empty());
    }
}
