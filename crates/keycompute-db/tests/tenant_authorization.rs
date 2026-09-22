//! Real PostgreSQL DAO regression. Each run owns a freshly created database.
use chrono::{Duration, Utc};
use keycompute_db::models::tenant_invitation::CreatedTenantInvitation;
use keycompute_db::{
    AuditContext, CreateTenantInvitationRequest, CreateTenantRequest, CreateUserRequest, Tenant,
    TenantAuditEvent, TenantInvitation, TenantMembership, User, UserBalance, initialize_schema,
};
use keycompute_types::{CredentialKind, PlatformRole, TenantRole, UserStatus};
use rust_decimal::Decimal;
use sea_orm::{
    ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement, TransactionTrait,
};
use uuid::Uuid;

fn actor(user: &User, role: Option<TenantRole>) -> AuditContext {
    AuditContext {
        actor_user_id: user.id,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: user.platform_role().unwrap(),
        actor_tenant_role: role,
        request_id: Some(Uuid::new_v4()),
    }
}
async fn user(db: &DatabaseConnection, name: &str, verified: bool) -> User {
    let user = User::create(
        db,
        &CreateUserRequest {
            email: format!("{name}@fixture.invalid"),
            name: None,
        },
    )
    .await
    .unwrap();
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO user_credentials(user_id,password_hash,email_verified) VALUES($1,'unusable-test-hash',$2)",
        [user.id.into(),verified.into()],
    )).await.unwrap();
    user
}
async fn tenant(db: &DatabaseConnection, owner: &User, slug: &str) -> Tenant {
    let tx = db.begin().await.unwrap();
    let tenant = Tenant::create_owned(
        &tx,
        &CreateTenantRequest {
            name: slug.into(),
            slug: slug.into(),
            description: None,
            default_rpm_limit: None,
            default_tpm_limit: None,
        },
        owner.id,
        &actor(owner, None),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    tenant
}
async fn issue(
    db: &DatabaseConnection,
    req: &CreateTenantInvitationRequest,
    actor: &AuditContext,
) -> Result<CreatedTenantInvitation, keycompute_db::DbError> {
    let tx = db.begin().await?;
    match TenantInvitation::create(&tx, req, actor).await {
        Ok(invitation) => {
            tx.commit().await?;
            Ok(invitation)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
async fn accept(
    db: &DatabaseConnection,
    tenant: Uuid,
    token: &str,
    user: &User,
) -> Result<TenantInvitation, keycompute_db::DbError> {
    let tx = db.begin().await?;
    match TenantInvitation::accept(&tx, tenant, token, user.id, &user.email, &actor(user, None))
        .await
    {
        Ok(invitation) => {
            tx.commit().await?;
            Ok(invitation)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
fn request(
    tenant: &Tenant,
    owner: &User,
    target: &User,
    role: TenantRole,
) -> CreateTenantInvitationRequest {
    CreateTenantInvitationRequest {
        tenant_id: tenant.id,
        invited_by: owner.id,
        email: target.email.to_ascii_uppercase(),
        tenant_role: role,
        expires_at: Utc::now() + Duration::hours(2),
    }
}
async fn run(db: DatabaseConnection) {
    initialize_schema(&db).await.unwrap();
    let tx = db.begin().await.unwrap();
    let root = User::bootstrap_root(&tx, "root@fixture.invalid", None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        TenantMembership::list_active_for_user(&db, root.id)
            .await
            .unwrap()
            .is_empty()
    );
    let tx = db.begin().await.unwrap();
    assert!(
        User::bootstrap_root(&tx, "another-root@fixture.invalid", None)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let owner = user(&db, "owner", true).await;
    let organization = tenant(&db, &owner, "tenant-a").await;
    let admin = actor(&owner, Some(TenantRole::Admin));
    // A globally suspended identity cannot remain the tenant's usable owner,
    // even when its retained membership row still says active/admin.
    let suspended_owner = db.begin().await.unwrap();
    suspended_owner
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET status='suspended' WHERE id=$1",
            [owner.id.into()],
        ))
        .await
        .unwrap();
    let rejected = suspended_owner
        .commit()
        .await
        .expect_err("owner suspension must fail without ownership transfer");
    assert!(
        rejected
            .to_string()
            .contains("tenant owner must be an active admin"),
        "{rejected}"
    );
    assert_eq!(
        User::find_by_id(&db, owner.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "active"
    );
    let invitee = user(&db, "invitee", false).await;
    let req = request(&organization, &owner, &invitee, TenantRole::Admin);

    let (first, second) = tokio::join!(issue(&db, &req, &admin), issue(&db, &req, &admin));
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.invitation.id, second.invitation.id);
    assert_ne!(first.token.is_some(), second.token.is_some());
    let token = first.token.or(second.token).unwrap();
    let mut changed = req.clone();
    changed.tenant_role = TenantRole::Member;
    assert!(issue(&db, &changed, &admin).await.is_err());
    assert!(
        accept(&db, organization.id, &token, &invitee)
            .await
            .is_err(),
        "unverified identity cannot accept"
    );
    assert!(
        TenantMembership::find_any(&db, organization.id, invitee.id)
            .await
            .unwrap()
            .is_none()
    );
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE user_credentials SET email_verified=TRUE WHERE user_id=$1",
        [invitee.id.into()],
    ))
    .await
    .unwrap();
    assert!(
        accept(&db, Uuid::new_v4(), &token, &invitee).await.is_err(),
        "token is bound to tenant"
    );
    assert!(
        accept(&db, organization.id, &token, &owner).await.is_err(),
        "token is bound to verified email"
    );
    let (a, b) = tokio::join!(
        accept(&db, organization.id, &token, &invitee),
        accept(&db, organization.id, &token, &invitee)
    );
    assert_ne!(a.is_ok(), b.is_ok(), "one concurrent accept must succeed");
    assert_eq!(
        TenantMembership::find(&db, organization.id, invitee.id)
            .await
            .unwrap()
            .unwrap()
            .tenant_role,
        "admin"
    );
    assert_eq!(
        User::find_by_id(&db, invitee.id)
            .await
            .unwrap()
            .unwrap()
            .platform_role,
        "none"
    );

    // A pending invitation becomes unusable when its inviting administrator
    // is suspended before acceptance; authority is checked at acceptance time.
    let second_admin = user(&db, "second-admin", true).await;
    let second_admin_invite = issue(
        &db,
        &request(&organization, &owner, &second_admin, TenantRole::Admin),
        &admin,
    )
    .await
    .unwrap();
    accept(
        &db,
        organization.id,
        second_admin_invite.token.as_deref().unwrap(),
        &second_admin,
    )
    .await
    .unwrap();
    let pending_target = user(&db, "pending-target", true).await;
    let pending = issue(
        &db,
        &request(
            &organization,
            &second_admin,
            &pending_target,
            TenantRole::Member,
        ),
        &actor(&second_admin, Some(TenantRole::Admin)),
    )
    .await
    .unwrap();
    let tx = db.begin().await.unwrap();
    TenantMembership::set_status(
        &tx,
        organization.id,
        second_admin.id,
        keycompute_types::MembershipStatus::Suspended,
        1,
        &admin,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        accept(
            &db,
            organization.id,
            pending.token.as_deref().unwrap(),
            &pending_target,
        )
        .await
        .is_err(),
        "suspended inviter cannot authorize a pending invitation"
    );

    let tx = db.begin().await.unwrap();
    TenantMembership::set_role(
        &tx,
        organization.id,
        owner.id,
        TenantRole::Member,
        1,
        &admin,
    )
    .await
    .unwrap();
    assert!(
        tx.commit().await.is_err(),
        "owner cannot be demoted even when another admin exists"
    );
    assert_eq!(
        TenantMembership::find(&db, organization.id, owner.id)
            .await
            .unwrap()
            .unwrap()
            .tenant_role,
        "admin"
    );
    let tx = db.begin().await.unwrap();
    let fake_root = AuditContext {
        actor_platform_role: PlatformRole::Root,
        ..admin
    };
    assert!(
        User::set_security(
            &tx,
            invitee.id,
            PlatformRole::Root,
            UserStatus::Active,
            &fake_root
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();

    let key = Uuid::new_v4();
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO produce_ai_keys(id,tenant_id,user_id,name,produce_ai_key_hash,produce_ai_key_preview) VALUES($1,$2,$3,'fixture',$4,'fixture-preview')",
        [key.into(),organization.id.into(),invitee.id.into(),Uuid::new_v4().to_string().into()],
    )).await.unwrap();
    let tx = db.begin().await.unwrap();
    TenantMembership::set_status(
        &tx,
        organization.id,
        invitee.id,
        keycompute_types::MembershipStatus::Removed,
        1,
        &admin,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        TenantMembership::find(&db, organization.id, invitee.id)
            .await
            .unwrap()
            .is_none()
    );
    let rejoin = issue(
        &db,
        &request(&organization, &owner, &invitee, TenantRole::Member),
        &admin,
    )
    .await
    .unwrap();
    accept(
        &db,
        organization.id,
        rejoin.token.as_deref().unwrap(),
        &invitee,
    )
    .await
    .unwrap();
    let rejoined = TenantMembership::find(&db, organization.id, invitee.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        rejoined.tenant_role, "member",
        "rejoin must not resurrect historical admin authority"
    );
    assert_eq!(rejoined.authz_version, 3);
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT revoked FROM produce_ai_keys WHERE id=$1",
            [key.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert!(
        row.try_get::<bool>("", "revoked").unwrap(),
        "rejoin must not resurrect old credentials"
    );

    let personal = tenant(&db, &invitee, "tenant-b").await;
    assert_eq!(
        TenantMembership::list_active_for_user(&db, invitee.id)
            .await
            .unwrap()
            .len(),
        2
    );
    UserBalance::recharge(
        &db,
        organization.id,
        invitee.id,
        Decimal::from(12),
        None,
        None,
    )
    .await
    .unwrap();
    UserBalance::recharge(&db, personal.id, invitee.id, Decimal::from(34), None, None)
        .await
        .unwrap();
    assert_eq!(
        UserBalance::find_by_user(&db, organization.id, invitee.id)
            .await
            .unwrap()
            .unwrap()
            .available_balance,
        Decimal::from(12)
    );
    assert_eq!(
        UserBalance::find_by_user(&db, personal.id, invitee.id)
            .await
            .unwrap()
            .unwrap()
            .available_balance,
        Decimal::from(34)
    );
    let outsider = user(&db, "outsider", true).await;
    let forged_tenant =
        keycompute_types::TenantScope::checked(organization.id, outsider.id, TenantRole::Admin)
            .unwrap();
    assert!(
        TenantMembership::list_in_tenant(&db, forged_tenant, 100, 0)
            .await
            .unwrap()
            .is_empty(),
        "a constructed role is not current membership authority"
    );
    assert!(
        TenantInvitation::list_in_tenant(&db, forged_tenant, 100, 0)
            .await
            .unwrap()
            .is_empty(),
        "invitations must not leak to a forged scope"
    );
    assert!(
        TenantAuditEvent::list_in_tenant(&db, forged_tenant, 100, 0)
            .await
            .unwrap()
            .is_empty(),
        "tenant audit must recheck its reader"
    );
    let forged_platform =
        keycompute_types::PlatformScope::checked(outsider.id, PlatformRole::Root).unwrap();
    assert!(
        TenantAuditEvent::list_platform(&db, forged_platform, 100, 0)
            .await
            .unwrap()
            .is_empty(),
        "a constructed root scope is not database authority"
    );
    let unauthorized = request(&organization, &outsider, &root, TenantRole::Admin);
    assert!(
        issue(
            &db,
            &unauthorized,
            &actor(&outsider, Some(TenantRole::Admin))
        )
        .await
        .is_err()
    );
    assert!(
        issue(
            &db,
            &request(&organization, &root, &outsider, TenantRole::Admin),
            &actor(&root, Some(TenantRole::Admin))
        )
        .await
        .is_err(),
        "platform root does not supply tenant membership"
    );

    let expires = request(&organization, &owner, &outsider, TenantRole::Member);
    let old = issue(&db, &expires, &admin).await.unwrap();
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE tenant_invitations SET expires_at=NOW()-INTERVAL '1 second' WHERE tenant_id=$1 AND id=$2",
        [organization.id.into(),old.invitation.id.into()],
    )).await.unwrap();
    let new = issue(&db, &expires, &admin).await.unwrap();
    assert_ne!(
        old.invitation.id, new.invitation.id,
        "expired pending rows cannot block future invitations"
    );
    assert!(
        accept(
            &db,
            organization.id,
            old.token.as_deref().unwrap(),
            &outsider
        )
        .await
        .is_err()
    );
    let tx = db.begin().await.unwrap();
    TenantInvitation::revoke(&tx, organization.id, new.invitation.id, &admin)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        accept(
            &db,
            organization.id,
            new.token.as_deref().unwrap(),
            &outsider
        )
        .await
        .is_err()
    );

    // An invitation issued before deactivation is not a new grant to rejoin.
    for status in [
        keycompute_types::MembershipStatus::Suspended,
        keycompute_types::MembershipStatus::Removed,
    ] {
        let outstanding = issue(
            &db,
            &request(&organization, &owner, &invitee, TenantRole::Member),
            &admin,
        )
        .await
        .unwrap();
        let before = TenantMembership::find(&db, organization.id, invitee.id)
            .await
            .unwrap()
            .unwrap();
        let tx = db.begin().await.unwrap();
        TenantMembership::set_status(
            &tx,
            organization.id,
            invitee.id,
            status,
            before.authz_version,
            &admin,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert!(
            accept(
                &db,
                organization.id,
                outstanding.token.as_deref().unwrap(),
                &invitee
            )
            .await
            .is_err(),
            "an invitation predating membership deactivation must not restore access"
        );
        assert!(
            TenantMembership::find(&db, organization.id, invitee.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            TenantMembership::find(&db, personal.id, invitee.id)
                .await
                .unwrap()
                .is_some()
        );
        if status == keycompute_types::MembershipStatus::Removed {
            // An inviter field and a previous acceptance timestamp are not a
            // new grant. Reject both metadata forgery and a suspended detour.
            for sql in [
                "UPDATE tenant_memberships SET status='suspended',removed_at=NULL WHERE tenant_id=$1 AND user_id=$2",
                "UPDATE tenant_memberships SET status='active',removed_at=NULL,invited_by=$2,joined_at=clock_timestamp() WHERE tenant_id=$1 AND user_id=$2",
                "UPDATE tenant_memberships SET status='active',removed_at=NULL WHERE tenant_id=$1 AND user_id=$2",
            ] {
                let rejected = db.begin().await.unwrap();
                let error = rejected
                    .execute(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        sql,
                        [organization.id.into(), invitee.id.into()],
                    ))
                    .await
                    .expect_err("removed membership must require a fresh invitation");
                assert!(
                    error
                        .to_string()
                        .contains("removed memberships require a new invitation"),
                    "unexpected database error: {error}"
                );
                rejected.rollback().await.unwrap();
            }
            let retained = TenantMembership::find_any(&db, organization.id, invitee.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retained.status, "removed");
            assert!(retained.removed_at.is_some());
            assert_eq!(retained.joined_at, before.joined_at);
            assert_eq!(retained.invited_by, before.invited_by);
        }
        let fresh = issue(
            &db,
            &request(&organization, &owner, &invitee, TenantRole::Member),
            &admin,
        )
        .await
        .unwrap();
        assert_ne!(outstanding.invitation.id, fresh.invitation.id);
        accept(
            &db,
            organization.id,
            fresh.token.as_deref().unwrap(),
            &invitee,
        )
        .await
        .unwrap();
    }

    let scope =
        keycompute_types::TenantScope::checked(organization.id, owner.id, TenantRole::Admin)
            .unwrap();
    let audit = TenantAuditEvent::list_in_tenant(&db, scope, 100, 0)
        .await
        .unwrap();
    assert!(
        audit
            .iter()
            .any(|event| event.action == "invitation.accept")
    );
    assert!(!serde_json::to_string(&audit).unwrap().contains(&token));
    let tx = db.begin().await.unwrap();
    assert!(
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM tenant_audit_events WHERE id=$1",
            [audit[0].id.into()],
        ))
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    println!(
        "Global identity, orthogonal membership, owner protection, invitations, permanent key revocation, independent wallets and immutable audit: passed"
    );
}

#[tokio::test]
async fn tenant_foundation_real_database() {
    assert!(
        std::env::var("KC_TENANT_TEST_ACK_ISOLATED").as_deref() == Ok("1")
            || std::env::var_os("CI").is_some(),
        "requires explicitly acknowledged isolated PostgreSQL or CI services"
    );
    let url = std::env::var("DATABASE_URL").expect("isolated DATABASE_URL is required");
    assert!(
        url.contains("@127.0.0.1:") || url.contains("@localhost:"),
        "only local test database endpoints are accepted"
    );
    let (prefix, _) = url
        .rsplit_once('/')
        .expect("database URL must contain a database name");
    let name = format!("kc_tenant_dao_{}", Uuid::new_v4().simple());
    let admin = Database::connect(&url).await.unwrap();
    admin
        .execute_unprepared(&format!("CREATE DATABASE {name}"))
        .await
        .unwrap();
    let db = Database::connect(format!("{prefix}/{name}")).await.unwrap();
    let worker = tokio::spawn(run(db.clone()));
    let result = worker.await;
    db.close().await.unwrap();
    admin
        .execute_unprepared(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .await
        .unwrap();
    admin.close().await.unwrap();
    result.unwrap();
}
