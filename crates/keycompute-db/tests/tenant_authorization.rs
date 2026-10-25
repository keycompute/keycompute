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
use sha2::{Digest, Sha256};
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
async fn tenant(db: &DatabaseConnection, admin: &User, slug: &str) -> Tenant {
    let tx = db.begin().await.unwrap();
    let tenant = Tenant::create_with_admin(
        &tx,
        &CreateTenantRequest {
            name: slug.into(),
            slug: slug.into(),
            description: None,
            default_rpm_limit: None,
            default_tpm_limit: None,
        },
        admin.id,
        &actor(admin, None),
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
    admin: &User,
    target: &User,
    role: TenantRole,
) -> CreateTenantInvitationRequest {
    CreateTenantInvitationRequest {
        tenant_id: tenant.id,
        invited_by: admin.id,
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
    let primary_admin = user(&db, "primary-admin", true).await;
    let organization = tenant(&db, &primary_admin, "tenant-a").await;
    let admin = actor(&primary_admin, Some(TenantRole::Admin));
    // Tenant administration is membership-based. Suspending the global user
    // invalidates access without relying on a distinguished administrator.
    let suspended_admin = db.begin().await.unwrap();
    suspended_admin
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET status='suspended' WHERE id=$1",
            [primary_admin.id.into()],
        ))
        .await
        .unwrap();
    suspended_admin.commit().await.unwrap();
    assert_eq!(
        User::find_by_id(&db, primary_admin.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "suspended"
    );
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET status='active' WHERE id=$1",
        [primary_admin.id.into()],
    ))
    .await
    .unwrap();
    let invitee = user(&db, "invitee", false).await;
    let req = request(&organization, &primary_admin, &invitee, TenantRole::Member);

    let (first, second) = tokio::join!(issue(&db, &req, &admin), issue(&db, &req, &admin));
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.invitation.id, second.invitation.id);
    assert_ne!(first.token.is_some(), second.token.is_some());
    let token = first.token.or(second.token).unwrap();
    let mut changed = req.clone();
    changed.tenant_role = TenantRole::Admin;
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
        accept(&db, organization.id, &token, &primary_admin)
            .await
            .is_err(),
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
        "member"
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
        &request(
            &organization,
            &primary_admin,
            &second_admin,
            TenantRole::Member,
        ),
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
    let second_membership = TenantMembership::find(&db, organization.id, second_admin.id)
        .await
        .unwrap()
        .unwrap();
    let platform_admin = actor(&root, None);
    let tx = db.begin().await.unwrap();
    let promoted = TenantMembership::set_role(
        &tx,
        organization.id,
        second_admin.id,
        TenantRole::Admin,
        second_membership.authz_version,
        &platform_admin,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let invitation_race_admin = user(&db, "invitation-race-admin", true).await;
    let invitation_race = issue(
        &db,
        &request(
            &organization,
            &primary_admin,
            &invitation_race_admin,
            TenantRole::Member,
        ),
        &admin,
    )
    .await
    .unwrap();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO tenant_memberships(tenant_id,user_id,tenant_role,status,invited_by) VALUES($1,$2,'admin','active',$3)",
        [
            organization.id.into(),
            invitation_race_admin.id.into(),
            root.id.into(),
        ],
    ))
    .await
    .unwrap();
    let protected = accept(
        &db,
        organization.id,
        invitation_race.token.as_deref().unwrap(),
        &invitation_race_admin,
    )
    .await
    .unwrap_err();
    assert_eq!(protected.to_string(), "peer_tenant_administrator_protected");
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
    let protected = TenantMembership::set_status(
        &tx,
        organization.id,
        second_admin.id,
        keycompute_types::MembershipStatus::Suspended,
        1,
        &admin,
    )
    .await
    .unwrap_err();
    assert_eq!(protected.to_string(), "peer_tenant_administrator_protected");
    let protected = TenantMembership::set_role(
        &tx,
        organization.id,
        second_admin.id,
        TenantRole::Member,
        1,
        &admin,
    )
    .await
    .unwrap_err();
    assert_eq!(protected.to_string(), "peer_tenant_administrator_protected");
    tx.rollback().await.unwrap();

    let tx = db.begin().await.unwrap();
    TenantMembership::set_status(
        &tx,
        organization.id,
        second_admin.id,
        keycompute_types::MembershipStatus::Suspended,
        promoted.authz_version,
        &platform_admin,
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
    let protected = issue(
        &db,
        &request(
            &organization,
            &primary_admin,
            &second_admin,
            TenantRole::Member,
        ),
        &admin,
    )
    .await
    .unwrap_err();
    assert_eq!(protected.to_string(), "peer_tenant_administrator_protected");

    let suspended_peer = TenantMembership::find_any(&db, organization.id, second_admin.id)
        .await
        .unwrap()
        .unwrap();
    let tx = db.begin().await.unwrap();
    let restored = TenantMembership::set_status(
        &tx,
        organization.id,
        second_admin.id,
        keycompute_types::MembershipStatus::Active,
        suspended_peer.authz_version,
        &platform_admin,
    )
    .await
    .unwrap();
    TenantMembership::set_role(
        &tx,
        organization.id,
        second_admin.id,
        TenantRole::Member,
        restored.authz_version,
        &platform_admin,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        TenantMembership::find(&db, organization.id, second_admin.id)
            .await
            .unwrap()
            .unwrap()
            .tenant_role,
        "member"
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
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO produce_ai_keys(id,tenant_id,user_id,name,produce_ai_key_hash,
         produce_ai_key_ciphertext,produce_ai_key_nonce,produce_ai_key_wrapped_dek,
         produce_ai_key_wrap_nonce,produce_ai_key_kek_id,produce_ai_key_encryption_version,
         produce_ai_key_preview)
         VALUES($1,$2,$3,'fixture',$4,'fixture-unrecoverable','AAAAAAAAAAAAAAAA',
         'fixture-unrecoverable','AAAAAAAAAAAAAAAA','test-fixture',1,'fixture-preview')",
        [
            key.into(),
            organization.id.into(),
            invitee.id.into(),
            hex::encode(Sha256::digest(format!("sk-{key}").as_bytes())).into(),
        ],
    ))
    .await
    .unwrap();
    let tx = db.begin().await.unwrap();
    TenantMembership::set_status(
        &tx,
        organization.id,
        invitee.id,
        keycompute_types::MembershipStatus::Removed,
        1,
        &platform_admin,
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
        &request(&organization, &primary_admin, &invitee, TenantRole::Member),
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

    let duplicate_tenant = db.begin().await.unwrap();
    assert!(
        Tenant::create_with_admin(
            &duplicate_tenant,
            &CreateTenantRequest {
                name: "duplicate".into(),
                slug: "tenant-duplicate".into(),
                description: None,
                default_rpm_limit: None,
                default_tpm_limit: None,
            },
            invitee.id,
            &actor(&invitee, None),
        )
        .await
        .is_err(),
        "a current tenant member cannot become administrator of another tenant"
    );
    duplicate_tenant.rollback().await.unwrap();
    let personal_admin = user(&db, "personal-admin", true).await;
    let personal = tenant(&db, &personal_admin, "tenant-b").await;
    assert_eq!(
        TenantMembership::list_active_for_user(&db, invitee.id)
            .await
            .unwrap()
            .len(),
        1
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
    UserBalance::recharge(
        &db,
        personal.id,
        personal_admin.id,
        Decimal::from(34),
        None,
        None,
    )
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
        UserBalance::find_by_user(&db, personal.id, personal_admin.id)
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
    let platform_invitee = user(&db, "platform-invitee", true).await;
    let platform_invitation = issue(
        &db,
        &request(&organization, &root, &platform_invitee, TenantRole::Member),
        &actor(&root, None),
    )
    .await
    .expect("platform root has implicit tenant administration");
    assert!(
        TenantMembership::list_active_for_user(&db, root.id)
            .await
            .unwrap()
            .is_empty(),
        "implicit platform administration must not create a membership"
    );
    accept(
        &db,
        organization.id,
        platform_invitation.token.as_deref().unwrap(),
        &platform_invitee,
    )
    .await
    .unwrap();

    let expires = request(&organization, &primary_admin, &outsider, TenantRole::Member);
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
            &request(&organization, &primary_admin, &invitee, TenantRole::Member),
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
            &request(&organization, &primary_admin, &invitee, TenantRole::Member),
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

    let scope = keycompute_types::TenantScope::checked(
        organization.id,
        primary_admin.id,
        TenantRole::Admin,
    )
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
        "Global identity, single-tenant membership, peer administrators, invitations, permanent key revocation, scoped wallets and immutable audit: passed"
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
