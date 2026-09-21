//! Global user identity status. Tenant roles belong exclusively to memberships.
pub use crate::tenant::UserStatus;

#[cfg(test)]
mod tests {
    use crate::{
        CredentialKind, MembershipStatus, PlatformRole, PlatformScope, TenantRole, TenantScope,
        UserStatus,
    };
    #[test]
    fn legacy_and_unknown_roles_fail_closed() {
        for value in ["system", "user", "admin", "ROOT", " root", ""] {
            assert!(value.parse::<PlatformRole>().is_err());
            assert!(serde_json::from_value::<PlatformRole>(serde_json::json!(value)).is_err());
        }
        assert!("owner".parse::<TenantRole>().is_err());
        assert!("unknown".parse::<CredentialKind>().is_err());
        assert!("inactive".parse::<UserStatus>().is_err());
        assert!("deleted".parse::<MembershipStatus>().is_err());
    }
    #[test]
    fn scope_constructors_reject_missing_identity() {
        let user = uuid::Uuid::new_v4();
        assert!(PlatformScope::checked(user, PlatformRole::None).is_err());
        assert!(PlatformScope::checked(uuid::Uuid::nil(), PlatformRole::Root).is_err());
        assert!(TenantScope::checked(uuid::Uuid::nil(), user, TenantRole::Admin).is_err());
        assert!(TenantScope::checked(user, uuid::Uuid::nil(), TenantRole::Member).is_err());
    }
}
