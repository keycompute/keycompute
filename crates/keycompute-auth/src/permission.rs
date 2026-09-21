//! Pure, fail-closed authorization primitives.

use keycompute_types::{AuthorizationSubject, CredentialKind, PlatformRole, TenantRole};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permission {
    AccessConsole,
    UseApi,
    ViewUsage,
    ManageApiKeys,
    ManageUsers,
    ManageTenant,
    ViewBilling,
    ManageOwnBilling,
    ManageBilling,
    ManagePricing,
    ManageProviders,
    ManageSystemSettings,
    ManageProtectedUsers,
    PlatformTenantHealth,
    PlatformDiagnostics,
    PlatformAggregateStats,
    PlatformNodeOperations,
}
impl Permission {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AccessConsole => "console:access",
            Self::UseApi => "api:use",
            Self::ViewUsage => "usage:view",
            Self::ManageApiKeys => "api_keys:manage",
            Self::ManageUsers => "users:manage",
            Self::ManageTenant => "tenant:manage",
            Self::ViewBilling => "billing:view",
            Self::ManageOwnBilling => "billing:self_manage",
            Self::ManageBilling => "billing:manage",
            Self::ManagePricing => "pricing:manage",
            Self::ManageProviders => "providers:manage",
            Self::ManageSystemSettings => "system_settings:manage",
            Self::ManageProtectedUsers => "protected_users:manage",
            Self::PlatformTenantHealth => "platform:tenant_health",
            Self::PlatformDiagnostics => "platform:diagnostics",
            Self::PlatformAggregateStats => "platform:aggregate_stats",
            Self::PlatformNodeOperations => "platform:node_operations",
        }
    }
    pub const fn all() -> &'static [Self] {
        &[
            Self::AccessConsole,
            Self::UseApi,
            Self::ViewUsage,
            Self::ManageApiKeys,
            Self::ManageUsers,
            Self::ManageTenant,
            Self::ViewBilling,
            Self::ManageOwnBilling,
            Self::ManageBilling,
            Self::ManagePricing,
            Self::ManageProviders,
            Self::ManageSystemSettings,
            Self::ManageProtectedUsers,
            Self::PlatformTenantHealth,
            Self::PlatformDiagnostics,
            Self::PlatformAggregateStats,
            Self::PlatformNodeOperations,
        ]
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::all().iter().copied().find(|p| p.as_str() == value)
    }
}
impl FromStr for Permission {
    type Err = String;
    fn from_str(v: &str) -> Result<Self, Self::Err> {
        Self::parse(v).ok_or_else(|| format!("unknown permission: {v}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceScope {
    Platform,
    Global,
    Tenant {
        tenant_id: Uuid,
    },
    UserOwned {
        tenant_id: Uuid,
        owner_user_id: Uuid,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationAction {
    View,
    Use,
    Manage,
    ManageMembers,
    InviteMembers,
    ManagePlatform,
    ReadTenantHealth,
    Diagnostics,
    AggregateStats,
    NodeOperations,
    ManageTenantResource,
    ReadPersonalResource,
    ManagePersonalResource,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationDecision {
    Allow,
    Deny,
}

pub struct PermissionChecker;
impl PermissionChecker {
    pub fn check(
        credential: CredentialKind,
        permissions: &[Permission],
        required: &Permission,
    ) -> bool {
        let permitted_credential = match credential {
            CredentialKind::Jwt => true,
            CredentialKind::ApiKey => *required == Permission::UseApi,
            CredentialKind::Node | CredentialKind::System => false,
        };
        permitted_credential && permissions.contains(required)
    }
    pub fn requires_tenant_isolation(permission: &Permission) -> bool {
        matches!(
            permission,
            Permission::UseApi
                | Permission::ViewUsage
                | Permission::ManageApiKeys
                | Permission::ViewBilling
                | Permission::ManageOwnBilling
        )
    }
}

/// Build capabilities from verified typed identity. No metadata or role-string input exists.
pub fn permissions_for(
    credential: CredentialKind,
    platform: PlatformRole,
    tenant: Option<TenantRole>,
) -> Vec<Permission> {
    // API keys are inference credentials only.  They receive one capability
    // after the validator has proved their fixed tenant membership; a global
    // API-key identity never receives a capability at all.
    if credential == CredentialKind::ApiKey {
        return tenant.map(|_| vec![Permission::UseApi]).unwrap_or_default();
    }
    if matches!(credential, CredentialKind::Node | CredentialKind::System) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if credential == CredentialKind::Jwt {
        out.push(Permission::AccessConsole);
    }
    if tenant.is_some() && credential == CredentialKind::Jwt {
        out.extend([
            Permission::UseApi,
            Permission::ViewUsage,
            Permission::ManageOwnBilling,
        ]);
    }
    if credential == CredentialKind::Jwt && tenant == Some(TenantRole::Admin) {
        out.extend([
            Permission::ManageApiKeys,
            Permission::ManageTenant,
            Permission::ViewBilling,
            Permission::ManageBilling,
            Permission::ManageProviders,
            Permission::ManagePricing,
        ]);
    }
    if credential == CredentialKind::Jwt {
        match platform {
            PlatformRole::Root => out.extend([
                Permission::ManageUsers,
                Permission::ManagePricing,
                Permission::ManageProviders,
                Permission::ManageSystemSettings,
                Permission::ManageProtectedUsers,
                Permission::PlatformTenantHealth,
                Permission::PlatformDiagnostics,
                Permission::PlatformAggregateStats,
                Permission::PlatformNodeOperations,
            ]),
            PlatformRole::Operator => out.extend([
                Permission::PlatformTenantHealth,
                Permission::PlatformDiagnostics,
                Permission::PlatformAggregateStats,
                Permission::PlatformNodeOperations,
            ]),
            PlatformRole::None => {}
        }
    }
    out
}

fn platform_allowed(role: PlatformRole, action: AuthorizationAction) -> bool {
    match role {
        PlatformRole::Root => matches!(
            action,
            AuthorizationAction::View
                | AuthorizationAction::ManagePlatform
                | AuthorizationAction::ReadTenantHealth
                | AuthorizationAction::Diagnostics
                | AuthorizationAction::AggregateStats
                | AuthorizationAction::NodeOperations
        ),
        PlatformRole::Operator => matches!(
            action,
            AuthorizationAction::ReadTenantHealth
                | AuthorizationAction::Diagnostics
                | AuthorizationAction::AggregateStats
                | AuthorizationAction::NodeOperations
        ),
        PlatformRole::None => false,
    }
}

/// Central decision function. Credential restrictions are checked first.
pub fn authorize(
    credential: CredentialKind,
    subject: AuthorizationSubject,
    action: AuthorizationAction,
    resource: ResourceScope,
) -> AuthorizationDecision {
    if subject.user_id.is_nil() || subject.tenant_id.is_some_and(|id| id.is_nil()) {
        return AuthorizationDecision::Deny;
    }
    // A malformed identifier is never a platform/global scope or an anonymous
    // resource owner. Check it before either credential or role can allow it.
    match resource {
        ResourceScope::Tenant { tenant_id } if tenant_id.is_nil() => {
            return AuthorizationDecision::Deny;
        }
        ResourceScope::UserOwned {
            tenant_id,
            owner_user_id,
        } if tenant_id.is_nil() || owner_user_id.is_nil() => {
            return AuthorizationDecision::Deny;
        }
        _ => {}
    }
    if credential != CredentialKind::Jwt {
        if credential != CredentialKind::ApiKey || !matches!(action, AuthorizationAction::Use) {
            return AuthorizationDecision::Deny;
        }
        return match resource {
            ResourceScope::Tenant { tenant_id }
                if subject.tenant_id == Some(tenant_id) && subject.tenant_role.is_some() =>
            {
                AuthorizationDecision::Allow
            }
            ResourceScope::UserOwned {
                tenant_id,
                owner_user_id,
            } if subject.tenant_id == Some(tenant_id)
                && subject.tenant_role.is_some()
                && owner_user_id == subject.user_id =>
            {
                AuthorizationDecision::Allow
            }
            _ => AuthorizationDecision::Deny,
        };
    }
    match resource {
        ResourceScope::Platform | ResourceScope::Global => {
            if platform_allowed(subject.platform_role, action) {
                AuthorizationDecision::Allow
            } else {
                AuthorizationDecision::Deny
            }
        }
        ResourceScope::Tenant { tenant_id } => {
            if subject.tenant_id != Some(tenant_id) || subject.tenant_role.is_none() {
                return AuthorizationDecision::Deny;
            }
            match subject.tenant_role {
                Some(TenantRole::Admin)
                    if matches!(
                        action,
                        AuthorizationAction::View
                            | AuthorizationAction::Use
                            | AuthorizationAction::Manage
                            | AuthorizationAction::ManageMembers
                            | AuthorizationAction::InviteMembers
                            | AuthorizationAction::ManageTenantResource
                    ) =>
                {
                    AuthorizationDecision::Allow
                }
                Some(TenantRole::Member)
                    if matches!(action, AuthorizationAction::View | AuthorizationAction::Use) =>
                {
                    AuthorizationDecision::Allow
                }
                _ => AuthorizationDecision::Deny,
            }
        }
        ResourceScope::UserOwned {
            tenant_id,
            owner_user_id,
        } => {
            if subject.tenant_id != Some(tenant_id) || subject.tenant_role.is_none() {
                return AuthorizationDecision::Deny;
            }
            if owner_user_id == subject.user_id
                && matches!(
                    action,
                    AuthorizationAction::View
                        | AuthorizationAction::Use
                        | AuthorizationAction::ReadPersonalResource
                        | AuthorizationAction::ManagePersonalResource
                )
            {
                return AuthorizationDecision::Allow;
            }
            if subject.tenant_role == Some(TenantRole::Admin)
                && matches!(
                    action,
                    AuthorizationAction::Manage | AuthorizationAction::ManageTenantResource
                )
            {
                return AuthorizationDecision::Allow;
            }
            AuthorizationDecision::Deny
        }
    }
}

#[cfg(test)]
#[path = "permission_boundary_tests.rs"]
mod boundary_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exhaustive_orthogonal_matrix() {
        let uid = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let other = Uuid::new_v4();
        for credential in [
            CredentialKind::Jwt,
            CredentialKind::ApiKey,
            CredentialKind::Node,
            CredentialKind::System,
        ] {
            for platform in [
                PlatformRole::Root,
                PlatformRole::Operator,
                PlatformRole::None,
            ] {
                for tr in [None, Some(TenantRole::Admin), Some(TenantRole::Member)] {
                    let s = AuthorizationSubject {
                        user_id: uid,
                        platform_role: platform,
                        tenant_id: Some(tenant),
                        tenant_role: tr,
                    };
                    let tv = authorize(
                        credential,
                        s,
                        AuthorizationAction::View,
                        ResourceScope::Tenant { tenant_id: tenant },
                    );
                    let po = authorize(
                        credential,
                        s,
                        AuthorizationAction::ReadPersonalResource,
                        ResourceScope::UserOwned {
                            tenant_id: tenant,
                            owner_user_id: uid,
                        },
                    );
                    let oo = authorize(
                        credential,
                        s,
                        AuthorizationAction::Manage,
                        ResourceScope::UserOwned {
                            tenant_id: tenant,
                            owner_user_id: other,
                        },
                    );
                    if credential == CredentialKind::Jwt && tr.is_some() {
                        assert_eq!(tv, AuthorizationDecision::Allow);
                        assert_eq!(po, AuthorizationDecision::Allow);
                    } else {
                        assert_eq!(tv, AuthorizationDecision::Deny);
                        assert_eq!(po, AuthorizationDecision::Deny);
                    }
                    assert_eq!(
                        oo,
                        if credential == CredentialKind::Jwt && tr == Some(TenantRole::Admin) {
                            AuthorizationDecision::Allow
                        } else {
                            AuthorizationDecision::Deny
                        }
                    );
                    let ph = authorize(
                        credential,
                        s,
                        AuthorizationAction::ReadTenantHealth,
                        ResourceScope::Platform,
                    );
                    let expected = if credential == CredentialKind::Jwt
                        && matches!(platform, PlatformRole::Root | PlatformRole::Operator)
                    {
                        AuthorizationDecision::Allow
                    } else {
                        AuthorizationDecision::Deny
                    };
                    assert_eq!(ph, expected);
                }
            }
        }
    }
    #[test]
    fn api_key_root_denied_console_and_membership() {
        let s = AuthorizationSubject::tenant(
            Uuid::new_v4(),
            Uuid::new_v4(),
            TenantRole::Admin,
            PlatformRole::Root,
        );
        assert_eq!(
            authorize(
                CredentialKind::ApiKey,
                s,
                AuthorizationAction::Manage,
                ResourceScope::Platform
            ),
            AuthorizationDecision::Deny
        );
        assert_eq!(
            authorize(
                CredentialKind::ApiKey,
                s,
                AuthorizationAction::ManageMembers,
                ResourceScope::Tenant {
                    tenant_id: s.tenant_id.unwrap()
                }
            ),
            AuthorizationDecision::Deny
        );
    }
    #[test]
    fn unknown_roles_fail_closed() {
        assert!("superuser".parse::<PlatformRole>().is_err());
        assert!("owner".parse::<TenantRole>().is_err());
        assert!("magic".parse::<CredentialKind>().is_err());
    }
}
