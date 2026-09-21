//! Wallet projections enforce current authorization without performing financial work.
use integration_tests::{
    common::generate_test_id,
    db::{TenantActor, TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_billing::balance::BalanceService;
use keycompute_db::{
    AuditContext, CreateTenantMembershipRequest, DbRouter, Tenant, TenantMembership, User,
    UserBalance,
};
use keycompute_types::{
    CredentialKind, MembershipStatus, PlatformRole, PlatformScope, TenantRole, TenantScope,
    UserStatus,
};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use uuid::Uuid;

fn admin(t: &Tenant) -> TenantScope {
    TenantScope::checked(t.id, t.owner_user_id, TenantRole::Admin).unwrap()
}
fn actor(user: Uuid) -> AuditContext {
    AuditContext {
        actor_user_id: user,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::None,
        actor_tenant_role: Some(TenantRole::Admin),
        request_id: Some(Uuid::new_v4()),
    }
}
struct Fixture {
    db: DatabaseConnection,
    a: Tenant,
    b: Tenant,
    user: TenantActor,
    other: TenantActor,
    guard: TestDataGuard,
}
impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run.clone());
        let a = create_test_tenant(&db, "wallet-scope-a", &run).await;
        let b = create_test_tenant(&db, "wallet-scope-b", &run).await;
        let user = create_test_user(&db, a.id, "wallet-multi", &run).await;
        let other = create_test_user(&db, a.id, "wallet-other", &run).await;
        let tx = db.begin().await.unwrap();
        TenantMembership::create(
            &tx,
            &CreateTenantMembershipRequest {
                tenant_id: b.id,
                user_id: user.id,
                role: TenantRole::Member,
            },
            &actor(b.owner_user_id),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let service = BalanceService::new(DbRouter::single(db.clone()));
        for (tenant, owner, amount) in [
            (a.id, user.id, 12),
            (b.id, user.id, 34),
            (a.id, other.id, 55),
        ] {
            service
                .recharge(tenant, owner, Decimal::from(amount), None, None)
                .await
                .unwrap();
        }
        Self {
            db,
            a,
            b,
            user,
            other,
            guard,
        }
    }
}
#[tokio::test]
async fn wallet_snapshots_keep_personal_tenant_and_platform_targets_separate() {
    let mut f = Fixture::new().await;
    let a = UserBalance::find_owned_display_snapshot(&f.db, f.user.scope())
        .await
        .unwrap();
    let bscope = TenantScope::checked(f.b.id, f.user.id, TenantRole::Member).unwrap();
    let b = UserBalance::find_owned_display_snapshot(&f.db, bscope)
        .await
        .unwrap();
    assert_eq!(a.available_balance, Decimal::from(12));
    assert_eq!(b.available_balance, Decimal::from(34));
    assert_ne!(a.tenant_id, b.tenant_id);
    let rows =
        UserBalance::find_display_snapshots_in_tenant(&f.db, admin(&f.a), &[f.user.id, f.other.id])
            .await
            .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[&f.other.id].available_balance, Decimal::from(55));
    assert!(
        UserBalance::find_display_snapshots_in_tenant(&f.db, f.user.scope(), &[f.user.id])
            .await
            .is_err()
    );
    let forged = TenantScope::checked(f.a.id, f.user.id, TenantRole::Admin).unwrap();
    assert!(
        UserBalance::find_display_snapshots_in_tenant(&f.db, forged, &[f.other.id])
            .await
            .is_err()
    );
    assert!(
        UserBalance::find_display_snapshots_in_tenant(
            &f.db,
            admin(&f.a),
            &[f.user.id, f.b.owner_user_id]
        )
        .await
        .is_err(),
        "foreign target must reject the entire batch, not become a zero snapshot"
    );
    let owner = UserBalance::find_owned_display_snapshot(&f.db, admin(&f.a))
        .await
        .unwrap();
    assert!(!owner.initialized);
    assert_eq!(owner.available_balance, Decimal::ZERO);
    assert!(
        UserBalance::find_by_user(&f.db, f.a.id, f.a.owner_user_id)
            .await
            .unwrap()
            .is_none()
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn revoked_member_wallet_is_private_but_remains_inspectable_by_its_tenant_admin() {
    let mut f = Fixture::new().await;
    let membership = TenantMembership::find_any(&f.db, f.a.id, f.user.id)
        .await
        .unwrap()
        .unwrap();
    let tx = f.db.begin().await.unwrap();
    TenantMembership::set_status(
        &tx,
        f.a.id,
        f.user.id,
        MembershipStatus::Revoked,
        membership.version,
        &actor(f.a.owner_user_id),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        UserBalance::find_owned_display_snapshot(&f.db, f.user.scope())
            .await
            .is_err()
    );
    let inspected = UserBalance::find_display_snapshots_in_tenant(&f.db, admin(&f.a), &[f.user.id])
        .await
        .unwrap();
    assert_eq!(inspected[&f.user.id].available_balance, Decimal::from(12));
    let bscope = TenantScope::checked(f.b.id, f.user.id, TenantRole::Member).unwrap();
    assert_eq!(
        UserBalance::find_owned_display_snapshot(&f.db, bscope)
            .await
            .unwrap()
            .available_balance,
        Decimal::from(34)
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn wallet_admin_and_root_projections_recheck_current_role_and_lifecycle() {
    let mut f = Fixture::new().await;
    let tx = f.db.begin().await.unwrap();
    let elevated = TenantMembership::set_role(
        &tx,
        f.a.id,
        f.other.id,
        TenantRole::Admin,
        1,
        &actor(f.a.owner_user_id),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let stale = TenantScope::checked(f.a.id, f.other.id, TenantRole::Admin).unwrap();
    assert!(
        UserBalance::find_display_snapshots_in_tenant(&f.db, stale, &[f.user.id])
            .await
            .is_ok()
    );
    let tx = f.db.begin().await.unwrap();
    TenantMembership::set_role(
        &tx,
        f.a.id,
        f.other.id,
        TenantRole::Member,
        elevated.version,
        &actor(f.a.owner_user_id),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        UserBalance::find_display_snapshots_in_tenant(&f.db, stale, &[f.user.id])
            .await
            .is_err()
    );
    let root = User::find_by_email(&f.db, "tenant-test-root@fixture.invalid")
        .await
        .unwrap()
        .unwrap();
    let fake = PlatformScope::checked(f.other.id, PlatformRole::Root).unwrap();
    assert!(
        UserBalance::find_display_snapshots_platform(&f.db, fake, f.a.id, &[f.user.id])
            .await
            .is_err()
    );
    let actual = PlatformScope::checked(root.id, PlatformRole::Root).unwrap();
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenants SET status='inactive' WHERE id=$1",
        [f.a.id.into()],
    ))
    .await
    .unwrap();
    assert!(
        UserBalance::find_owned_display_snapshot(&f.db, f.user.scope())
            .await
            .is_err()
    );
    assert!(
        UserBalance::find_display_snapshots_in_tenant(&f.db, admin(&f.a), &[f.user.id])
            .await
            .is_err()
    );
    let rows = UserBalance::find_display_snapshots_platform(&f.db, actual, f.a.id, &[f.user.id])
        .await
        .unwrap();
    assert_eq!(rows[&f.user.id].available_balance, Decimal::from(12));
    let tx = f.db.begin().await.unwrap();
    User::set_security(
        &tx,
        f.other.id,
        PlatformRole::Operator,
        UserStatus::Active,
        &actor(root.id),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let op = PlatformScope::checked(f.other.id, PlatformRole::Operator).unwrap();
    assert!(
        UserBalance::find_display_snapshots_platform(&f.db, op, f.a.id, &[f.user.id])
            .await
            .is_err()
    );
    f.guard.cleanup().await.unwrap();
}
