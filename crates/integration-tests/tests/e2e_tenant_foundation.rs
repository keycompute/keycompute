//! Database regressions for the global identity and explicit membership model.

use chrono::{Duration, Utc};
use integration_tests::{
    common::generate_test_id,
    db::{cleanup_test_data, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{
    AuditContext, CreateTenantInvitationRequest, CreateUserRequest, TenantInvitation,
    TenantMembership, User,
};
use keycompute_types::{CredentialKind, PlatformRole, TenantRole};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};

fn tenant_audit(actor: uuid::Uuid, role: Option<TenantRole>) -> AuditContext {
    AuditContext {
        actor_user_id: actor,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::None,
        actor_tenant_role: role,
        request_id: Some(uuid::Uuid::new_v4()),
    }
}

#[tokio::test]
async fn global_identity_has_explicit_memberships_and_single_use_invitations() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let tenant = create_test_tenant(&db, "foundation", &run).await;
    let member = create_test_user(&db, tenant.id, "foundation-member", &run).await;
    let second_tenant = create_test_tenant(&db, "foundation-second", &run).await;

    let member_audit = tenant_audit(second_tenant.owner_user_id, Some(TenantRole::Admin));
    let tx = db.begin().await.unwrap();
    let second_membership = TenantMembership::create(
        &tx,
        &keycompute_db::CreateTenantMembershipRequest {
            tenant_id: second_tenant.id,
            user_id: member.id,
            tenant_role: TenantRole::Admin,
        },
        &member_audit,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let memberships = TenantMembership::list_active_for_user(&db, member.id)
        .await
        .unwrap();
    assert_eq!(memberships.len(), 2);
    assert!(
        memberships
            .iter()
            .any(|m| m.tenant_id == tenant.id && m.tenant_role().unwrap() == TenantRole::Member)
    );
    assert!(
        memberships
            .iter()
            .any(|m| m.tenant_id == second_tenant.id
                && m.tenant_role().unwrap() == TenantRole::Admin)
    );
    assert_eq!(
        User::find_by_id(&db, member.id)
            .await
            .unwrap()
            .unwrap()
            .platform_role()
            .unwrap(),
        PlatformRole::None
    );
    assert_eq!(second_membership.user_id, member.id);

    let tx = db.begin().await.unwrap();
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET status='removed' WHERE tenant_id=$1 AND user_id=$2",
        [tenant.id.into(), member.id.into()],
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let removed = TenantMembership::find_any(&db, tenant.id, member.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        removed.membership_status().unwrap(),
        keycompute_types::MembershipStatus::Removed
    );

    let invitee = User::create(
        &db,
        &CreateUserRequest {
            email: format!("invitee-{run}@example.com"),
            name: Some("Invitation User".into()),
        },
    )
    .await
    .unwrap();
    let audit = tenant_audit(second_tenant.owner_user_id, Some(TenantRole::Admin));
    let tx = db.begin().await.unwrap();
    let created = TenantInvitation::create(
        &tx,
        &CreateTenantInvitationRequest {
            tenant_id: second_tenant.id,
            invited_by: second_tenant.owner_user_id,
            email: invitee.email.clone(),
            tenant_role: TenantRole::Member,
            expires_at: Utc::now() + Duration::hours(1),
        },
        &audit,
    )
    .await
    .unwrap();
    let token = created.token.clone().unwrap();
    let invitation_id = created.invitation.id;
    tx.commit().await.unwrap();

    let accept_audit = tenant_audit(invitee.id, None);
    // The invitation token alone is insufficient. A current verified email
    // and the accepting user's own audit identity are also required.
    let tx = db.begin().await.unwrap();
    assert!(
        TenantInvitation::accept(
            &tx,
            second_tenant.id,
            &token,
            invitee.id,
            &invitee.email,
            &accept_audit,
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO user_credentials(user_id,password_hash,email_verified) VALUES($1,'unusable-invitation-test-hash',TRUE)",
        [invitee.id.into()],
    )).await.unwrap();
    let tx = db.begin().await.unwrap();
    let accepted = TenantInvitation::accept(
        &tx,
        second_tenant.id,
        &token,
        invitee.id,
        &invitee.email,
        &accept_audit,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(accepted.id, invitation_id);
    let tx = db.begin().await.unwrap();
    let replay = TenantInvitation::accept(
        &tx,
        second_tenant.id,
        &token,
        invitee.id,
        &invitee.email,
        &accept_audit,
    )
    .await;
    assert!(replay.is_err(), "an invitation token must be single use");
    tx.rollback().await.unwrap();

    let by_email = User::find_by_email(&db, &invitee.email)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_email.id, invitee.id);
    cleanup_test_data(&db, &run).await.unwrap();
}

#[tokio::test]
async fn tenant_owner_remains_an_active_admin_under_last_admin_changes() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let tenant = create_test_tenant(&db, "last-admin", &run).await;
    let other = create_test_user(&db, tenant.id, "last-admin-other", &run).await;
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
        [tenant.id.into(), other.id.into()],
    ))
    .await
    .unwrap();

    let tx = db.begin().await.unwrap();
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET status='suspended' WHERE tenant_id=$1 AND user_id=$2",
        [tenant.id.into(), tenant.owner_user_id.into()],
    ))
    .await
    .unwrap();
    assert!(
        tx.commit().await.is_err(),
        "the deferred tenant invariant must reject suspending the owner"
    );
    let active_admins = TenantMembership::list_active_for_tenant(&db, tenant.id)
        .await
        .unwrap();
    assert!(
        active_admins
            .iter()
            .any(|m| m.user_id == tenant.owner_user_id
                && m.tenant_role == TenantRole::Admin.as_str())
    );
    cleanup_test_data(&db, &run).await.unwrap();
}
