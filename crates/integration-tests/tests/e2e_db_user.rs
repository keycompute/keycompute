//! Global identity and tenant membership database tests.

use integration_tests::{
    common::{VerificationChain, generate_test_id},
    db::{cleanup_test_data, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{CreateUserRequest, TenantMembership, UpdateUserRequest, User};
use keycompute_types::{MembershipStatus, PlatformRole, TenantRole};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};

#[tokio::test]
async fn test_user_crud() {
    let pool = create_test_pool().await;
    let run = generate_test_id();
    let tenant = create_test_tenant(&pool, "user-crud", &run).await;
    let actor = create_test_user(&pool, tenant.id, "user-crud", &run).await;
    let mut chain = VerificationChain::new();

    let found = User::find_by_id(&pool, actor.id).await.unwrap().unwrap();
    chain.add_step(
        "keycompute-db",
        "User::find_by_id",
        "global identity found",
        found.id == actor.id,
    );
    chain.add_step(
        "keycompute-db",
        "User::platform_role",
        "membership does not become platform authority",
        found.platform_role().unwrap() == PlatformRole::None,
    );
    let members = User::find_by_tenant(&pool, tenant.id).await.unwrap();
    chain.add_step(
        "keycompute-db",
        "User::find_by_tenant",
        "explicit tenant membership filter",
        members.iter().any(|u| u.id == actor.id),
    );
    let updated = found
        .update(
            &pool,
            &UpdateUserRequest {
                name: Some("Updated User".into()),
            },
        )
        .await
        .unwrap();
    chain.add_step(
        "keycompute-db",
        "User::update",
        "profile update",
        updated.name.as_deref() == Some("Updated User"),
    );
    chain.print_report();
    assert!(chain.all_passed());
    assert!(
        actor.delete(&pool).await.is_err(),
        "membership history protects global identity"
    );
    cleanup_test_data(&pool, &run).await.unwrap();
}

#[tokio::test]
async fn test_two_memberships_keep_roles_and_global_identity_independent() {
    let pool = create_test_pool().await;
    let run = generate_test_id();
    let first = create_test_tenant(&pool, "membership-one", &run).await;
    let second = create_test_tenant(&pool, "membership-two", &run).await;
    let actor = create_test_user(&pool, first.id, "multi-membership", &run).await;
    let tx = pool.begin().await.unwrap();
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO tenant_memberships(tenant_id,user_id,role,status) VALUES($1,$2,'admin','active')",
        [second.id.into(), actor.id.into()],
    )).await.unwrap();
    tx.commit().await.unwrap();
    let rows = TenantMembership::list_active_for_user(&pool, actor.id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .find(|m| m.tenant_id == first.id)
            .unwrap()
            .tenant_role()
            .unwrap(),
        TenantRole::Member
    );
    assert_eq!(
        rows.iter()
            .find(|m| m.tenant_id == second.id)
            .unwrap()
            .tenant_role()
            .unwrap(),
        TenantRole::Admin
    );
    assert_eq!(
        User::find_by_id(&pool, actor.id)
            .await
            .unwrap()
            .unwrap()
            .platform_role()
            .unwrap(),
        PlatformRole::None
    );
    cleanup_test_data(&pool, &run).await.unwrap();
}

#[tokio::test]
async fn test_membership_revoke_retains_history_and_blocks_session() {
    let pool = create_test_pool().await;
    let run = generate_test_id();
    let tenant = create_test_tenant(&pool, "membership-revoke", &run).await;
    let actor = create_test_user(&pool, tenant.id, "membership-revoke", &run).await;
    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET status='revoked' WHERE tenant_id=$1 AND user_id=$2",
        [tenant.id.into(), actor.id.into()],
    ))
    .await
    .unwrap();
    let historical = TenantMembership::find_any(&pool, tenant.id, actor.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        historical.membership_status().unwrap(),
        MembershipStatus::Revoked
    );
    assert!(
        TenantMembership::find(&pool, tenant.id, actor.id)
            .await
            .unwrap()
            .is_none()
    );
    cleanup_test_data(&pool, &run).await.unwrap();
}

#[tokio::test]
async fn test_platform_role_values_are_global_and_typed() {
    let pool = create_test_pool().await;
    let run = generate_test_id();
    let root = User::create(
        &pool,
        &CreateUserRequest {
            email: format!("platform-root-{run}@example.com"),
            name: Some("Root".into()),
        },
    )
    .await
    .unwrap();
    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET platform_role='root' WHERE id=$1",
        [root.id.into()],
    ))
    .await
    .unwrap();
    let loaded = User::find_by_id(&pool, root.id).await.unwrap().unwrap();
    assert_eq!(loaded.platform_role().unwrap(), PlatformRole::Root);
    assert_eq!(loaded.token_version, root.token_version + 1);
    let bad = pool
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='tenant_admin' WHERE id=$1",
            [root.id.into()],
        ))
        .await;
    assert!(bad.is_err());
    cleanup_test_data(&pool, &run).await.unwrap();
}

#[tokio::test]
async fn test_invalid_membership_role_is_rejected() {
    let pool = create_test_pool().await;
    let run = generate_test_id();
    let tenant = create_test_tenant(&pool, "membership-role", &run).await;
    let actor = create_test_user(&pool, tenant.id, "membership-role", &run).await;
    let bad = pool
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET role='tenant_admin' WHERE tenant_id=$1 AND user_id=$2",
            [tenant.id.into(), actor.id.into()],
        ))
        .await;
    assert!(bad.is_err());
    cleanup_test_data(&pool, &run).await.unwrap();
}
