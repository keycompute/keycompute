//! Real PostgreSQL coverage for the phase-3 pricing authority boundary.
//!
//! These tests intentionally exercise the DAO instead of manufacturing HTTP
//! authorization contexts. The HTTP handler tests cover the legacy response
//! envelope; this file protects scope, lock, audit and transaction behavior.

use integration_tests::{
    common::generate_test_id,
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::models::pricing_model::{
    BillingDimension, PlatformPricingScope, PricingScopeType, PricingTarget, TenantPricingScope,
};
use keycompute_db::{AuditContext, CreatePricingRequest, PricingModel, UpdatePricingRequest, User};
use keycompute_types::{CredentialKind, PlatformRole, TenantRole};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use std::time::Duration;
use uuid::Uuid;

struct Fixture {
    db: DatabaseConnection,
    run_id: String,
    a: keycompute_db::Tenant,
    b: keycompute_db::Tenant,
    member_a: integration_tests::db::TenantActor,
    root: Uuid,
    second_root: Uuid,
    guard: TestDataGuard,
}

impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run_id = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run_id.clone());
        let a = create_test_tenant(&db, "pricing-a", &run_id).await;
        let b = create_test_tenant(&db, "pricing-b", &run_id).await;
        let member_a = create_test_user(&db, a.id, "pricing-member", &run_id).await;
        let root = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM users WHERE email='tenant-test-root@fixture.invalid' AND platform_role='root' AND status='active'",
                [],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap();
        let second_root = User::create(
            &db,
            &keycompute_db::CreateUserRequest {
                email: format!("pricing-root-{}@example.com", run_id),
                name: Some("Pricing second root".into()),
            },
        )
        .await
        .unwrap();
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='root' WHERE id=$1",
            [second_root.id.into()],
        ))
        .await
        .unwrap();
        Self {
            db,
            run_id,
            a,
            b,
            member_a,
            root,
            second_root: second_root.id,
            guard,
        }
    }

    fn tenant_scope(&self, tenant_id: Uuid, actor: Uuid, _role: TenantRole) -> TenantPricingScope {
        TenantPricingScope::checked(tenant_id, actor, CredentialKind::Jwt, 0, 1, 1).unwrap()
    }

    fn platform_scope(&self, actor: Uuid) -> PlatformPricingScope {
        PlatformPricingScope::checked(
            actor,
            CredentialKind::Jwt,
            if actor == self.second_root { 1 } else { 0 },
        )
        .unwrap()
    }

    fn audit(
        &self,
        actor: Uuid,
        platform_role: PlatformRole,
        tenant_role: Option<TenantRole>,
    ) -> AuditContext {
        AuditContext {
            actor_user_id: actor,
            credential_kind: CredentialKind::Jwt,
            actor_platform_role: platform_role,
            actor_tenant_role: tenant_role,
            request_id: Some(Uuid::new_v4()),
        }
    }

    fn audit_for(actor: Uuid) -> AuditContext {
        AuditContext {
            actor_user_id: actor,
            credential_kind: CredentialKind::Jwt,
            actor_platform_role: PlatformRole::Root,
            actor_tenant_role: None,
            request_id: Some(Uuid::new_v4()),
        }
    }

    fn tenant_request(
        &self,
        tenant_id: Uuid,
        model_name: &str,
        is_default: bool,
    ) -> CreatePricingRequest {
        CreatePricingRequest {
            scope_type: PricingScopeType::Tenant,
            tenant_id: Some(tenant_id),
            model_name: model_name.into(),
            billing_dimension: BillingDimension::ProviderAccount,
            currency: Some("USD".into()),
            input_price_per_1k: "0.01".parse().unwrap(),
            output_price_per_1k: "0.02".parse().unwrap(),
            is_default: Some(is_default),
            effective_from: None,
            effective_until: None,
        }
    }

    fn platform_request(&self, model_name: &str, is_default: bool) -> CreatePricingRequest {
        CreatePricingRequest {
            scope_type: PricingScopeType::Platform,
            tenant_id: None,
            model_name: model_name.into(),
            billing_dimension: BillingDimension::ProviderAccount,
            currency: Some("USD".into()),
            input_price_per_1k: "0.01".parse().unwrap(),
            output_price_per_1k: "0.02".parse().unwrap(),
            is_default: Some(is_default),
            effective_from: None,
            effective_until: None,
        }
    }

    async fn pricing_audit_count(&self, pricing_id: Uuid, action: Option<&str>) -> i64 {
        self.db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) FROM pricing_audit_events WHERE pricing_id=$1 AND ($2::TEXT IS NULL OR action=$2)",
                [pricing_id.into(), action.map(str::to_owned).into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap()
    }

    async fn tenant_audit_count(&self, pricing_id: Uuid, action: Option<&str>) -> i64 {
        self.db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) FROM tenant_audit_events WHERE resource_type='pricing_model' AND resource_id=$1 AND ($2::TEXT IS NULL OR action=$2)",
                [pricing_id.to_string().into(), action.map(str::to_owned).into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap()
    }
}

async fn create_tenant_price(
    fixture: &Fixture,
    tenant_id: Uuid,
    actor: Uuid,
    request: &CreatePricingRequest,
) -> PricingModel {
    let tx = fixture.db.begin().await.unwrap();
    let row = PricingModel::create_in_tenant(
        &tx,
        fixture.tenant_scope(tenant_id, actor, TenantRole::Admin),
        request,
        &fixture.audit(actor, PlatformRole::None, Some(TenantRole::Admin)),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    row
}

#[tokio::test]
async fn tenant_pricing_reads_bind_current_admin_and_tenant() {
    let mut fixture = Fixture::new().await;
    let a_one = create_tenant_price(
        &fixture,
        fixture.a.id,
        fixture.a.owner_user_id,
        &fixture.tenant_request(
            fixture.a.id,
            &format!("pricing-test-{}-alpha", fixture.run_id),
            true,
        ),
    )
    .await;
    let a_two = create_tenant_price(
        &fixture,
        fixture.a.id,
        fixture.a.owner_user_id,
        &fixture.tenant_request(
            fixture.a.id,
            &format!("pricing-test-{}-beta", fixture.run_id),
            false,
        ),
    )
    .await;
    let b_one = create_tenant_price(
        &fixture,
        fixture.b.id,
        fixture.b.owner_user_id,
        &fixture.tenant_request(
            fixture.b.id,
            &format!("pricing-test-{}-foreign", fixture.run_id),
            true,
        ),
    )
    .await;
    let admin = fixture.tenant_scope(fixture.a.id, fixture.a.owner_user_id, TenantRole::Admin);
    let rows = PricingModel::find_in_tenant(&fixture.db, admin, Some("alpha"), 1, 0)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, a_one.id);
    assert_eq!(
        PricingModel::count_in_tenant(&fixture.db, admin, Some("alpha"))
            .await
            .unwrap(),
        1
    );
    assert!(
        PricingModel::find_in_tenant_by_id(&fixture.db, admin, b_one.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        PricingModel::find_in_tenant(&fixture.db, admin, None, 1, 0)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        PricingModel::count_in_tenant(&fixture.db, admin, None)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        PricingModel::find_in_tenant_by_id(&fixture.db, admin, a_two.id)
            .await
            .unwrap()
            .unwrap()
            .tenant_id,
        Some(fixture.a.id)
    );
    let member_scope = fixture.tenant_scope(fixture.a.id, fixture.member_a.id, TenantRole::Member);
    assert!(
        PricingModel::find_in_tenant(&fixture.db, member_scope, None, 100, 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        PricingModel::count_in_tenant(&fixture.db, member_scope, None)
            .await
            .unwrap(),
        0
    );
    assert!(
        PricingModel::find_in_tenant_by_id(&fixture.db, member_scope, a_one.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        PricingModel::find_in_tenant_by_id(&fixture.db, member_scope, b_one.id)
            .await
            .unwrap()
            .is_none()
    );
    fixture.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn tenant_delete_requires_an_active_tenant() {
    let mut fixture = Fixture::new().await;
    let row = create_tenant_price(
        &fixture,
        fixture.a.id,
        fixture.a.owner_user_id,
        &fixture.tenant_request(
            fixture.a.id,
            &format!("pricing-test-{}-inactive-delete", fixture.run_id),
            false,
        ),
    )
    .await;
    fixture
        .db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenants SET status='inactive' WHERE id=$1",
            [fixture.a.id.into()],
        ))
        .await
        .unwrap();
    let tx = fixture.db.begin().await.unwrap();
    let scope = TenantPricingScope::checked(
        fixture.a.id,
        fixture.a.owner_user_id,
        CredentialKind::Jwt,
        0,
        2,
        1,
    )
    .unwrap();
    assert!(
        PricingModel::delete_in_tenant(
            &tx,
            scope,
            row.id,
            &fixture.audit(
                fixture.a.owner_user_id,
                PlatformRole::None,
                Some(TenantRole::Admin)
            ),
        )
        .await
        .is_err()
    );
    tx.commit().await.unwrap();
    assert!(
        PricingModel::find_platform_by_id(
            &fixture.db,
            fixture.platform_scope(fixture.root),
            PricingTarget::Tenant(fixture.a.id),
            row.id,
        )
        .await
        .unwrap()
        .is_some()
    );
    fixture.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn forged_scope_actor_and_non_jwt_credentials_fail_before_mutation() {
    let mut fixture = Fixture::new().await;
    let row = create_tenant_price(
        &fixture,
        fixture.a.id,
        fixture.a.owner_user_id,
        &fixture.tenant_request(
            fixture.a.id,
            &format!("pricing-test-{}-forged", fixture.run_id),
            false,
        ),
    )
    .await;
    let forged_actor = fixture.audit(
        fixture.member_a.id,
        PlatformRole::Root,
        Some(TenantRole::Admin),
    );
    let tx = fixture.db.begin().await.unwrap();
    let result = PricingModel::update_in_tenant(
        &tx,
        fixture.tenant_scope(fixture.a.id, fixture.member_a.id, TenantRole::Admin),
        row.id,
        &UpdatePricingRequest {
            input_price_per_1k: Some("9".parse().unwrap()),
            output_price_per_1k: None,
            effective_until: None,
            expected_version: row.version,
        },
        &forged_actor,
    )
    .await;
    assert!(result.is_err());
    tx.commit().await.unwrap();
    let unchanged = PricingModel::find_in_tenant_by_id(
        &fixture.db,
        fixture.tenant_scope(fixture.a.id, fixture.a.owner_user_id, TenantRole::Admin),
        row.id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(unchanged.input_price_per_1k, row.input_price_per_1k);

    let stale_scope = TenantPricingScope::checked(
        fixture.a.id,
        fixture.a.owner_user_id,
        CredentialKind::Jwt,
        1,
        1,
        1,
    )
    .unwrap();
    let tx = fixture.db.begin().await.unwrap();
    assert!(
        PricingModel::update_in_tenant(
            &tx,
            stale_scope,
            row.id,
            &UpdatePricingRequest {
                input_price_per_1k: Some("8".parse().unwrap()),
                output_price_per_1k: None,
                effective_until: None,
                expected_version: row.version,
            },
            &fixture.audit(
                fixture.a.owner_user_id,
                PlatformRole::None,
                Some(TenantRole::Admin)
            ),
        )
        .await
        .is_err()
    );
    tx.commit().await.unwrap();

    for credential in [
        CredentialKind::ApiKey,
        CredentialKind::Node,
        CredentialKind::System,
    ] {
        let scope =
            TenantPricingScope::checked(fixture.a.id, fixture.a.owner_user_id, credential, 0, 1, 1)
                .unwrap();
        assert!(
            PricingModel::find_in_tenant(&fixture.db, scope, None, 10, 0)
                .await
                .is_err()
        );
    }
    fixture.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn platform_root_works_without_tenant_and_operator_does_not_manage() {
    let mut fixture = Fixture::new().await;
    let platform = fixture.platform_scope(fixture.root);
    let row = {
        let tx = fixture.db.begin().await.unwrap();
        let row = PricingModel::create_platform(
            &tx,
            platform,
            PricingTarget::Platform,
            &fixture.platform_request(&format!("pricing-test-{}-platform", fixture.run_id), true),
            &fixture.audit(fixture.root, PlatformRole::Root, None),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        row
    };
    assert!(
        PricingModel::find_platform_by_id(&fixture.db, platform, PricingTarget::Platform, row.id,)
            .await
            .unwrap()
            .is_some()
    );

    let operator = User::create(
        &fixture.db,
        &keycompute_db::CreateUserRequest {
            email: format!("pricing-operator-{}@example.com", fixture.run_id),
            name: Some("Pricing operator".into()),
        },
    )
    .await
    .unwrap();
    fixture
        .db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='operator' WHERE id=$1",
            [operator.id.into()],
        ))
        .await
        .unwrap();
    let operator_scope =
        PlatformPricingScope::checked(operator.id, CredentialKind::Jwt, 0).unwrap();
    assert!(
        PricingModel::find_platform_filtered(
            &fixture.db,
            operator_scope,
            PricingTarget::Platform,
            None,
            10,
            0,
        )
        .await
        .unwrap()
        .is_empty()
    );
    let tx = fixture.db.begin().await.unwrap();
    assert!(
        PricingModel::update_platform(
            &tx,
            operator_scope,
            PricingTarget::Platform,
            row.id,
            &UpdatePricingRequest {
                input_price_per_1k: Some("3".parse().unwrap()),
                output_price_per_1k: None,
                effective_until: None,
                expected_version: row.version,
            },
            &fixture.audit(operator.id, PlatformRole::Operator, None),
        )
        .await
        .is_err()
    );
    tx.commit().await.unwrap();
    fixture.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn duplicate_insert_is_rejected_without_losing_default_or_audits() {
    let mut fixture = Fixture::new().await;
    let first = {
        let tx = fixture.db.begin().await.unwrap();
        let row = PricingModel::create_platform(
            &tx,
            fixture.platform_scope(fixture.root),
            PricingTarget::Platform,
            &fixture.platform_request(&format!("pricing-test-{}-duplicate", fixture.run_id), true),
            &fixture.audit(fixture.root, PlatformRole::Root, None),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        row
    };

    let pricing_audits_before = fixture.pricing_audit_count(first.id, None).await;
    let tenant_audits_before = fixture.tenant_audit_count(first.id, None).await;
    let tx = fixture.db.begin().await.unwrap();
    let duplicate = PricingModel::create_platform(
        &tx,
        fixture.platform_scope(fixture.root),
        PricingTarget::Platform,
        &fixture.platform_request(&format!("pricing-test-{}-duplicate", fixture.run_id), true),
        &fixture.audit(fixture.root, PlatformRole::Root, None),
    )
    .await;
    assert!(duplicate.is_err());
    tx.commit().await.unwrap();

    let after = PricingModel::find_platform_by_id(
        &fixture.db,
        fixture.platform_scope(fixture.root),
        PricingTarget::Platform,
        first.id,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(after.is_default);
    assert_eq!(after.version, first.version);
    assert_eq!(
        fixture.pricing_audit_count(first.id, None).await,
        pricing_audits_before
    );
    assert_eq!(
        fixture.tenant_audit_count(first.id, None).await,
        tenant_audits_before
    );

    let row_count: i64 = fixture
        .db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*) FROM pricing_models WHERE scope_type='platform' AND tenant_id IS NULL AND model_name=$1 AND billing_dimension='provideraccount'",
            [format!("pricing-test-{}-duplicate", fixture.run_id).into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    assert_eq!(row_count, 1);
    fixture.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn batch_defaults_reject_missing_or_foreign_ids_atomically() {
    let mut fixture = Fixture::new().await;
    let first = {
        let tx = fixture.db.begin().await.unwrap();
        let row = PricingModel::create_platform(
            &tx,
            fixture.platform_scope(fixture.root),
            PricingTarget::Platform,
            &fixture.platform_request(&format!("pricing-test-{}-batch-a", fixture.run_id), false),
            &fixture.audit(fixture.root, PlatformRole::Root, None),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        row
    };
    let second = {
        let tx = fixture.db.begin().await.unwrap();
        let row = PricingModel::create_platform(
            &tx,
            fixture.platform_scope(fixture.root),
            PricingTarget::Platform,
            &fixture.platform_request(&format!("pricing-test-{}-batch-b", fixture.run_id), false),
            &fixture.audit(fixture.root, PlatformRole::Root, None),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        row
    };
    let foreign = create_tenant_price(
        &fixture,
        fixture.b.id,
        fixture.b.owner_user_id,
        &fixture.tenant_request(
            fixture.b.id,
            &format!("pricing-test-{}-foreign-batch", fixture.run_id),
            false,
        ),
    )
    .await;
    let pricing_audits_before = fixture.pricing_audit_count(first.id, None).await
        + fixture.pricing_audit_count(second.id, None).await;
    let tenant_audits_before = fixture.tenant_audit_count(first.id, None).await
        + fixture.tenant_audit_count(second.id, None).await;

    for ids in [[first.id, Uuid::new_v4()], [first.id, foreign.id]] {
        let tx = fixture.db.begin().await.unwrap();
        let result = PricingModel::batch_make_defaults_platform(
            &tx,
            fixture.platform_scope(fixture.root),
            PricingTarget::Platform,
            &ids,
            &fixture.audit(fixture.root, PlatformRole::Root, None),
        )
        .await;
        assert!(result.is_err());
        tx.commit().await.unwrap();
    }

    for id in [first.id, second.id] {
        let row = PricingModel::find_platform_by_id(
            &fixture.db,
            fixture.platform_scope(fixture.root),
            PricingTarget::Platform,
            id,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!row.is_default);
        assert_eq!(row.version, 1);
    }
    assert_eq!(
        fixture.pricing_audit_count(first.id, None).await
            + fixture.pricing_audit_count(second.id, None).await,
        pricing_audits_before
    );
    assert_eq!(
        fixture.tenant_audit_count(first.id, None).await
            + fixture.tenant_audit_count(second.id, None).await,
        tenant_audits_before
    );
    fixture.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn reversed_order_concurrent_batches_are_deadlock_free_and_audited_once() {
    let mut fixture = Fixture::new().await;
    let first = {
        let tx = fixture.db.begin().await.unwrap();
        let row = PricingModel::create_platform(
            &tx,
            fixture.platform_scope(fixture.root),
            PricingTarget::Platform,
            &fixture.platform_request(
                &format!("pricing-test-{}-concurrent-a", fixture.run_id),
                false,
            ),
            &fixture.audit(fixture.root, PlatformRole::Root, None),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        row
    };
    let second = {
        let tx = fixture.db.begin().await.unwrap();
        let row = PricingModel::create_platform(
            &tx,
            fixture.platform_scope(fixture.root),
            PricingTarget::Platform,
            &fixture.platform_request(
                &format!("pricing-test-{}-concurrent-b", fixture.run_id),
                false,
            ),
            &fixture.audit(fixture.root, PlatformRole::Root, None),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        row
    };
    let first_id = first.id;
    let second_id = second.id;
    let db_a = fixture.db.clone();
    let db_b = fixture.db.clone();
    let root_a = fixture.root;
    let root_b = fixture.second_root;
    let (left, right) = tokio::time::timeout(Duration::from_secs(5), async move {
        tokio::join!(
            async move {
                let tx = db_a.begin().await.unwrap();
                let audit = Fixture::audit_for(root_a);
                let result = PricingModel::batch_make_defaults_platform(
                    &tx,
                    PlatformPricingScope::checked(root_a, CredentialKind::Jwt, 0).unwrap(),
                    PricingTarget::Platform,
                    &[first_id, second_id],
                    &audit,
                )
                .await;
                if result.is_ok() {
                    tx.commit().await.unwrap();
                } else {
                    tx.rollback().await.unwrap();
                }
                result
            },
            async move {
                let tx = db_b.begin().await.unwrap();
                let audit = Fixture::audit_for(root_b);
                let result = PricingModel::batch_make_defaults_platform(
                    &tx,
                    PlatformPricingScope::checked(root_b, CredentialKind::Jwt, 1).unwrap(),
                    PricingTarget::Platform,
                    &[second_id, first_id],
                    &audit,
                )
                .await;
                if result.is_ok() {
                    tx.commit().await.unwrap();
                } else {
                    tx.rollback().await.unwrap();
                }
                result
            }
        )
    })
    .await
    .expect("reversed pricing batches must not deadlock");
    assert!(left.is_ok());
    assert!(right.is_ok());
    for id in [first_id, second_id] {
        let row = PricingModel::find_platform_by_id(
            &fixture.db,
            fixture.platform_scope(fixture.root),
            PricingTarget::Platform,
            id,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(row.is_default);
        assert_eq!(row.version, 2);
        assert_eq!(
            fixture.pricing_audit_count(id, Some("make_default")).await,
            1
        );
        assert_eq!(
            fixture
                .tenant_audit_count(id, Some("pricing.make_default"))
                .await,
            1
        );
    }
    fixture.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn concurrent_same_row_default_is_idempotent() {
    let mut fixture = Fixture::new().await;
    let row = {
        let tx = fixture.db.begin().await.unwrap();
        let row = PricingModel::create_platform(
            &tx,
            fixture.platform_scope(fixture.root),
            PricingTarget::Platform,
            &fixture.platform_request(&format!("pricing-test-{}-same-row", fixture.run_id), false),
            &fixture.audit(fixture.root, PlatformRole::Root, None),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        row
    };
    let row_id = row.id;
    let db_a = fixture.db.clone();
    let db_b = fixture.db.clone();
    let root_a = fixture.root;
    let root_b = fixture.second_root;
    let (left, right) = tokio::time::timeout(Duration::from_secs(5), async move {
        tokio::join!(
            async move {
                let tx = db_a.begin().await.unwrap();
                let audit = Fixture::audit_for(root_a);
                let result = PricingModel::make_default_platform(
                    &tx,
                    PlatformPricingScope::checked(root_a, CredentialKind::Jwt, 0).unwrap(),
                    PricingTarget::Platform,
                    row_id,
                    &audit,
                )
                .await;
                if result.is_ok() {
                    tx.commit().await.unwrap();
                } else {
                    tx.rollback().await.unwrap();
                }
                result
            },
            async move {
                let tx = db_b.begin().await.unwrap();
                let audit = Fixture::audit_for(root_b);
                let result = PricingModel::make_default_platform(
                    &tx,
                    PlatformPricingScope::checked(root_b, CredentialKind::Jwt, 1).unwrap(),
                    PricingTarget::Platform,
                    row_id,
                    &audit,
                )
                .await;
                if result.is_ok() {
                    tx.commit().await.unwrap();
                } else {
                    tx.rollback().await.unwrap();
                }
                result
            }
        )
    })
    .await
    .expect("same-row pricing defaults must not deadlock");
    assert!(left.is_ok());
    assert!(right.is_ok());
    let current = PricingModel::find_platform_by_id(
        &fixture.db,
        fixture.platform_scope(fixture.root),
        PricingTarget::Platform,
        row_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(current.is_default);
    assert_eq!(current.version, 2);
    assert_eq!(
        fixture
            .pricing_audit_count(row_id, Some("make_default"))
            .await,
        1
    );
    fixture.guard.cleanup().await.unwrap();
}

// Fault-injection DDL takes a ShareRowExclusive table lock. Acquire the
// production administration fence first, just as a pricing mutation does.
// Otherwise a parallel writer can hold the fence and wait to insert its audit,
// while this test holds the audit table and waits for that same fence (40P01).
// This orders only the fault transaction; it does not serialize the test suite.
async fn install_pricing_audit_fault(tx: &sea_orm::DatabaseTransaction) {
    let fence = tx
        .execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    assert_eq!(fence.rows_affected(), 1);
    tx.execute_unprepared(
        "CREATE FUNCTION pg_temp.reject_pricing_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'pricing audit rejected'; END $$;
         CREATE TRIGGER reject_pricing_audit BEFORE INSERT ON tenant_audit_events
         FOR EACH ROW WHEN (NEW.action='pricing.update')
         EXECUTE FUNCTION pg_temp.reject_pricing_audit()",
    ).await.unwrap();
}

#[tokio::test]
async fn failed_audit_rolls_back_pricing_mutation_inside_savepoint() {
    let mut fixture = Fixture::new().await;
    let row = {
        let tx = fixture.db.begin().await.unwrap();
        let row = PricingModel::create_platform(
            &tx,
            fixture.platform_scope(fixture.root),
            PricingTarget::Platform,
            &fixture.platform_request(&format!("pricing-test-{}-audit", fixture.run_id), false),
            &fixture.audit(fixture.root, PlatformRole::Root, None),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        row
    };
    let pricing_audits_before = fixture.pricing_audit_count(row.id, None).await;
    let tenant_audits_before = fixture.tenant_audit_count(row.id, None).await;
    let tx = fixture.db.begin().await.unwrap();
    install_pricing_audit_fault(&tx).await;
    let result = PricingModel::update_platform(
        &tx,
        fixture.platform_scope(fixture.root),
        PricingTarget::Platform,
        row.id,
        &UpdatePricingRequest {
            input_price_per_1k: Some("0.07".parse().unwrap()),
            output_price_per_1k: None,
            effective_until: None,
            expected_version: row.version,
        },
        &fixture.audit(fixture.root, PlatformRole::Root, None),
    )
    .await;
    assert!(result.is_err());
    tx.execute_unprepared("DROP TRIGGER reject_pricing_audit ON tenant_audit_events")
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let after = PricingModel::find_platform_by_id(
        &fixture.db,
        fixture.platform_scope(fixture.root),
        PricingTarget::Platform,
        row.id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(after.version, row.version);
    assert_eq!(after.input_price_per_1k, row.input_price_per_1k);
    assert_eq!(
        fixture.pricing_audit_count(row.id, None).await,
        pricing_audits_before
    );
    assert_eq!(
        fixture.tenant_audit_count(row.id, None).await,
        tenant_audits_before
    );
    fixture.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn optimistic_conflict_and_outer_rollback_leave_pricing_unchanged() {
    let mut fixture = Fixture::new().await;
    let row = {
        let tx = fixture.db.begin().await.unwrap();
        let row = PricingModel::create_platform(
            &tx,
            fixture.platform_scope(fixture.root),
            PricingTarget::Platform,
            &fixture.platform_request(&format!("pricing-test-{}-rollback", fixture.run_id), false),
            &fixture.audit(fixture.root, PlatformRole::Root, None),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        row
    };
    let tx = fixture.db.begin().await.unwrap();
    let updated = PricingModel::update_platform(
        &tx,
        fixture.platform_scope(fixture.root),
        PricingTarget::Platform,
        row.id,
        &UpdatePricingRequest {
            input_price_per_1k: Some("0.03".parse().unwrap()),
            output_price_per_1k: None,
            effective_until: None,
            expected_version: row.version,
        },
        &fixture.audit(fixture.root, PlatformRole::Root, None),
    )
    .await
    .unwrap();
    assert!(
        PricingModel::update_platform(
            &tx,
            fixture.platform_scope(fixture.root),
            PricingTarget::Platform,
            row.id,
            &UpdatePricingRequest {
                input_price_per_1k: Some("0.04".parse().unwrap()),
                output_price_per_1k: None,
                effective_until: None,
                expected_version: row.version,
            },
            &fixture.audit(fixture.root, PlatformRole::Root, None),
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    let after = PricingModel::find_platform_by_id(
        &fixture.db,
        fixture.platform_scope(fixture.root),
        PricingTarget::Platform,
        row.id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(after.version, row.version);
    assert_eq!(after.input_price_per_1k, row.input_price_per_1k);
    assert_ne!(updated.version, after.version);
    fixture.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn platform_price_reads_use_real_global_authority_without_tenant_role_inheritance() {
    use axum::{
        Extension, Router,
        body::Body,
        http::{Request, StatusCode},
        routing::get,
    };
    use keycompute_server::{
        AppState, extractors::RequestId, handlers::admin_pricing::list_pricing,
    };
    use tower::ServiceExt;
    let mut fixture = Fixture::new().await;
    let state = AppState::with_pool(keycompute_db::DbRouter::single(fixture.db.clone()));
    let bare = Router::new()
        .route("/pricing", get(list_pricing))
        .layer(Extension(RequestId::new()))
        .with_state(state.clone());
    for (user_id, selected, expected) in [
        (fixture.root, None, StatusCode::OK),
        (
            fixture.a.owner_user_id,
            Some(fixture.a.id),
            StatusCode::FORBIDDEN,
        ),
        (
            fixture.member_a.id,
            Some(fixture.a.id),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let user = User::find_by_id(&fixture.db, user_id)
            .await
            .unwrap()
            .unwrap();
        let global = state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(user.id, None, user.token_version, None, None, 3600)
            .unwrap();
        let token = if let Some(tenant) = selected {
            let auth = state.auth.verify_token(&global).await.unwrap();
            state
                .auth
                .select_tenant(&auth, Some(tenant))
                .await
                .unwrap()
                .access_token
        } else {
            global
        };
        let response = bare
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/pricing?scope_type=platform")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    fixture.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn audit_fault_waits_for_admin_fence_without_locking_the_audit_table() {
    let mut fixture = Fixture::new().await;
    let blocker = fixture.db.begin().await.unwrap();
    blocker
        .execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let blocker_pid: i32 = blocker
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT pg_backend_pid()",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    let (send_pid, receive_pid) = tokio::sync::oneshot::channel();
    let db = fixture.db.clone();
    let mut fault = tokio::spawn(async move {
        let tx = db.begin().await.unwrap();
        let pid: i32 = tx
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT pg_backend_pid()",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap();
        send_pid.send(pid).unwrap();
        install_pricing_audit_fault(&tx).await;
        // Rollback removes both the temporary trigger and its table locks.
        tx.rollback().await.unwrap();
    });
    let waiter = receive_pid.await.unwrap();
    // Other parallel cases may be ahead in the tuple-lock queue. Follow the
    // complete wait chain, not just its immediate predecessor.
    let observed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row = fixture.db.query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "WITH RECURSIVE blockers(pid,visited) AS (SELECT p,ARRAY[$1,p] FROM unnest(pg_blocking_pids($1)) AS p UNION ALL SELECT p,b.visited||p FROM blockers b CROSS JOIN LATERAL unnest(pg_blocking_pids(b.pid)) AS p WHERE p<>ALL(b.visited) AND cardinality(b.visited)<64) SELECT EXISTS(SELECT 1 FROM blockers WHERE pid=$2) AS waiting, EXISTS(SELECT 1 FROM pg_locks WHERE pid=$1 AND relation='tenant_audit_events'::regclass AND granted) AS audit_locked",
                [waiter.into(), blocker_pid.into()],
            )).await.unwrap().unwrap();
            if row.try_get::<bool>("", "waiting").unwrap() {
                break row.try_get::<bool>("", "audit_locked").unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await;
    // Always release the blocker before asserting, including on regressions.
    // No background waiter should outlive a failed lock-order assertion.
    let audit_locked = observed.as_ref().copied().unwrap_or(true);
    let writer_can_lock =
        if !audit_locked {
            blocker.execute_unprepared(
            "SET LOCAL lock_timeout='2s'; LOCK TABLE tenant_audit_events IN ROW EXCLUSIVE MODE",
        ).await.is_ok()
        } else {
            false
        };
    blocker.rollback().await.unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(5), &mut fault).await;
    if completed.is_err() {
        fault.abort();
        let _ = fault.await;
    }
    assert!(
        observed.is_ok(),
        "fault installer did not wait on the expected fence"
    );
    assert!(
        !audit_locked,
        "fault installer acquired the audit table before its admin fence"
    );
    assert!(
        writer_can_lock,
        "the fenced writer must still be able to acquire its audit lock"
    );
    completed
        .expect("fault installer did not finish after releasing the fence")
        .unwrap();
    fixture.guard.cleanup().await.unwrap();
}
