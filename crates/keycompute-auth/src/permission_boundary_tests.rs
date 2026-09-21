//! Regressions for credential restrictions and malformed resource scopes.
use super::*;
use crate::AuthContext;

#[test]
fn every_context_obeys_the_same_credential_ceiling() {
    let user = Uuid::new_v4();
    for credential in [
        CredentialKind::Jwt,
        CredentialKind::ApiKey,
        CredentialKind::Node,
        CredentialKind::System,
    ] {
        // Even an erroneously widened capability list cannot turn inference or
        // node credentials into console credentials.
        let context =
            AuthContext::new(user, credential).with_permissions(Permission::all().to_vec());
        for permission in Permission::all() {
            let expected = match credential {
                CredentialKind::Jwt => true,
                CredentialKind::ApiKey => *permission == Permission::UseApi,
                CredentialKind::Node | CredentialKind::System => false,
            };
            assert_eq!(
                PermissionChecker::check(credential, Permission::all(), permission),
                expected,
                "checker {credential:?}/{permission:?}"
            );
            assert_eq!(
                context.has_permission(permission),
                expected,
                "context {credential:?}/{permission:?}"
            );
        }
    }
}

#[test]
fn nil_tenant_or_owner_never_identifies_an_authorized_resource() {
    let user = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    for credential in [CredentialKind::Jwt, CredentialKind::ApiKey] {
        for platform in [
            PlatformRole::Root,
            PlatformRole::Operator,
            PlatformRole::None,
        ] {
            for role in [TenantRole::Admin, TenantRole::Member] {
                let malformed = AuthorizationSubject::tenant(user, Uuid::nil(), role, platform);
                for action in [
                    AuthorizationAction::Use,
                    AuthorizationAction::View,
                    AuthorizationAction::ManageTenantResource,
                ] {
                    assert_eq!(
                        authorize(
                            credential,
                            malformed,
                            action,
                            ResourceScope::Tenant {
                                tenant_id: Uuid::nil()
                            }
                        ),
                        AuthorizationDecision::Deny
                    );
                }
                let valid = AuthorizationSubject::tenant(user, tenant, role, platform);
                assert_eq!(
                    authorize(
                        credential,
                        valid,
                        AuthorizationAction::ManageTenantResource,
                        ResourceScope::UserOwned {
                            tenant_id: tenant,
                            owner_user_id: Uuid::nil()
                        }
                    ),
                    AuthorizationDecision::Deny
                );
            }
        }
    }
}

#[test]
fn personal_actions_never_expand_for_tenant_or_platform_administrators() {
    let user = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let other_user = Uuid::new_v4();
    let other_tenant = Uuid::new_v4();
    for platform in [
        PlatformRole::Root,
        PlatformRole::Operator,
        PlatformRole::None,
    ] {
        for role in [TenantRole::Admin, TenantRole::Member] {
            let subject = AuthorizationSubject::tenant(user, tenant, role, platform);
            for action in [
                AuthorizationAction::ReadPersonalResource,
                AuthorizationAction::ManagePersonalResource,
            ] {
                assert_eq!(
                    authorize(
                        CredentialKind::Jwt,
                        subject,
                        action,
                        ResourceScope::UserOwned {
                            tenant_id: tenant,
                            owner_user_id: user
                        }
                    ),
                    AuthorizationDecision::Allow
                );
                assert_eq!(
                    authorize(
                        CredentialKind::Jwt,
                        subject,
                        action,
                        ResourceScope::UserOwned {
                            tenant_id: tenant,
                            owner_user_id: other_user
                        }
                    ),
                    AuthorizationDecision::Deny
                );
                assert_eq!(
                    authorize(
                        CredentialKind::Jwt,
                        subject,
                        action,
                        ResourceScope::UserOwned {
                            tenant_id: other_tenant,
                            owner_user_id: user
                        }
                    ),
                    AuthorizationDecision::Deny
                );
            }
        }
    }
}

#[test]
fn no_action_can_cross_a_tenant_boundary_or_synthesize_a_membership() {
    let user = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let foreign = Uuid::new_v4();
    let actions = [
        AuthorizationAction::View,
        AuthorizationAction::Use,
        AuthorizationAction::Manage,
        AuthorizationAction::ManageMembers,
        AuthorizationAction::InviteMembers,
        AuthorizationAction::ManagePlatform,
        AuthorizationAction::ReadTenantHealth,
        AuthorizationAction::Diagnostics,
        AuthorizationAction::AggregateStats,
        AuthorizationAction::NodeOperations,
        AuthorizationAction::ManageTenantResource,
        AuthorizationAction::ReadPersonalResource,
        AuthorizationAction::ManagePersonalResource,
    ];
    for platform in [
        PlatformRole::Root,
        PlatformRole::Operator,
        PlatformRole::None,
    ] {
        for credential in [
            CredentialKind::Jwt,
            CredentialKind::ApiKey,
            CredentialKind::Node,
            CredentialKind::System,
        ] {
            for role in [None, Some(TenantRole::Admin), Some(TenantRole::Member)] {
                let subject = AuthorizationSubject {
                    user_id: user,
                    platform_role: platform,
                    tenant_id: Some(tenant),
                    tenant_role: role,
                };
                for action in actions {
                    assert_eq!(
                        authorize(
                            credential,
                            subject,
                            action,
                            ResourceScope::Tenant { tenant_id: foreign }
                        ),
                        AuthorizationDecision::Deny
                    );
                    assert_eq!(
                        authorize(
                            credential,
                            subject,
                            action,
                            ResourceScope::UserOwned {
                                tenant_id: foreign,
                                owner_user_id: user
                            }
                        ),
                        AuthorizationDecision::Deny
                    );
                    if role.is_none() {
                        assert_eq!(
                            authorize(
                                credential,
                                subject,
                                action,
                                ResourceScope::Tenant { tenant_id: tenant }
                            ),
                            AuthorizationDecision::Deny
                        );
                    }
                }
            }
        }
    }
}
