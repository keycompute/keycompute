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

    pub fn has_tenant_permission(&self, permission: &str) -> bool {
        self.capabilities
            .tenant
            .iter()
            .any(|value| value == permission)
    }

    pub fn can_manage_console(&self) -> bool {
        self.has_platform_permission("console:access")
            || self.has_platform_permission("users:manage")
            || self.has_tenant_permission("tenant:manage")
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

    pub fn can_manage_console(&self) -> bool {
        (self.info)()
            .as_ref()
            .map(UserInfo::can_manage_console)
            .unwrap_or(false)
    }
}
