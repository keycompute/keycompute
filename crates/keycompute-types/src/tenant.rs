//! Final tenant and platform role model shared by every service.

use serde::{Deserialize, Serialize};
use std::str::FromStr;

macro_rules! role_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "lowercase")]
        pub enum $name {
            $(#[serde(rename = $value)] $variant),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }

            pub fn parse(value: &str) -> crate::Result<Self> {
                value.parse().map_err(crate::KeyComputeError::ValidationError)
            }

            pub const fn allowed_values() -> &'static [&'static str] {
                &[$($value),+]
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(format!("invalid {}: {value}; allowed: {}", stringify!($name), Self::allowed_values().join(", "))),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

role_enum!(UserStatus { Active => "active", Suspended => "suspended" });
role_enum!(TenantStatus { Active => "active", Inactive => "inactive" });
role_enum!(AuditScopeType { Platform => "platform", Tenant => "tenant" });
role_enum!(AuditResult { Success => "success", Denied => "denied", Failure => "failure" });
role_enum!(PlatformRole { Root => "root", Operator => "operator", None => "none" });
role_enum!(CredentialKind { Jwt => "jwt", ApiKey => "api_key", Node => "node", System => "system" });
role_enum!(TenantRole { Admin => "admin", Member => "member" });
role_enum!(MembershipStatus { Active => "active", Suspended => "suspended", Removed => "removed" });
role_enum!(TenantInvitationStatus { Pending => "pending", Accepted => "accepted", Expired => "expired", Revoked => "revoked" });

impl PlatformRole {
    pub const fn is_platform_admin(self) -> bool {
        matches!(self, Self::Root | Self::Operator)
    }
}

impl TenantRole {
    pub const fn can_manage_tenant(self) -> bool {
        matches!(self, Self::Admin)
    }
}

/// A principal's final authorization input. The tenant role is always read
/// from the membership for the requested tenant; it is never inferred from a
/// global user record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizationSubject {
    pub user_id: uuid::Uuid,
    pub platform_role: PlatformRole,
    pub tenant_id: Option<uuid::Uuid>,
    pub tenant_role: Option<TenantRole>,
}

impl AuthorizationSubject {
    pub const fn platform(user_id: uuid::Uuid, platform_role: PlatformRole) -> Self {
        Self {
            user_id,
            platform_role,
            tenant_id: None,
            tenant_role: None,
        }
    }

    pub const fn tenant(
        user_id: uuid::Uuid,
        tenant_id: uuid::Uuid,
        tenant_role: TenantRole,
        platform_role: PlatformRole,
    ) -> Self {
        Self {
            user_id,
            platform_role,
            tenant_id: Some(tenant_id),
            tenant_role: Some(tenant_role),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_round_trip() {
        assert_eq!("root".parse::<PlatformRole>().unwrap(), PlatformRole::Root);
        assert_eq!(
            "operator".parse::<PlatformRole>().unwrap(),
            PlatformRole::Operator
        );
        assert_eq!("none".parse::<PlatformRole>().unwrap(), PlatformRole::None);
        assert_eq!("admin".parse::<TenantRole>().unwrap(), TenantRole::Admin);
        assert_eq!("member".parse::<TenantRole>().unwrap(), TenantRole::Member);
    }

    #[test]
    fn platform_and_tenant_scopes_are_independent() {
        let id = uuid::Uuid::new_v4();
        let subject = AuthorizationSubject::tenant(
            id,
            uuid::Uuid::new_v4(),
            TenantRole::Admin,
            PlatformRole::None,
        );
        assert!(!subject.platform_role.is_platform_admin());
        assert!(subject.tenant_role.unwrap().can_manage_tenant());
    }
}

/// Explicit platform access. Construct only after primary-DB authentication and
/// operation-specific authorization; an operator is not an unrestricted root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformScope {
    user_id: uuid::Uuid,
    platform_role: PlatformRole,
}
impl PlatformScope {
    pub fn checked(user_id: uuid::Uuid, role: PlatformRole) -> Result<Self, String> {
        if user_id.is_nil() || role == PlatformRole::None {
            return Err("platform scope requires a real privileged user".into());
        }
        Ok(Self {
            user_id,
            platform_role: role,
        })
    }
    pub const fn user_id(self) -> uuid::Uuid {
        self.user_id
    }
    pub const fn platform_role(self) -> PlatformRole {
        self.platform_role
    }
}

/// Explicit tenant access. The authorization layer must verify an active user,
/// tenant and membership before construction, including for root/operator.
/// Not deserializable: a client cannot supply an authorization scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TenantScope {
    tenant_id: uuid::Uuid,
    user_id: uuid::Uuid,
    tenant_role: TenantRole,
}
impl TenantScope {
    pub fn checked(
        tenant_id: uuid::Uuid,
        user_id: uuid::Uuid,
        role: TenantRole,
    ) -> Result<Self, String> {
        if tenant_id.is_nil() || user_id.is_nil() {
            return Err("tenant scope requires real tenant and user IDs".into());
        }
        Ok(Self {
            tenant_id,
            user_id,
            tenant_role: role,
        })
    }
    pub const fn tenant_id(self) -> uuid::Uuid {
        self.tenant_id
    }
    pub const fn user_id(self) -> uuid::Uuid {
        self.user_id
    }
    pub const fn tenant_role(self) -> TenantRole {
        self.tenant_role
    }
}
