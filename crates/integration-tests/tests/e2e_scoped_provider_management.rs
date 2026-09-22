//! PostgreSQL coverage for scoped provider-account and passthrough management.
//!
//! These tests deliberately call the production scoped DAOs.  They do not
//! construct an alternate authorization object or rely on a handler-only
//! tenant filter.

use chrono::{Duration, Utc};
use integration_tests::common::generate_test_id;
use integration_tests::db::{
    TenantActor, TestDataGuard, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_db::models::account::{AccountListFilter, ProviderAuthzSnapshot};
use keycompute_db::models::passthrough_binding::PassthroughBindingListFilter;
use keycompute_db::{
    Account, CreateAccountRequest, CreatePassthroughBindingRequest, DbError, ResponseAffinity,
    Tenant, UpdateAccountRequest, User,
};
use keycompute_types::{CredentialKind, PlatformRole, PlatformScope, TenantRole};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use serde_json::json;
use std::time::Duration as StdDuration;
use uuid::Uuid;

fn tenant_admin(tenant: &Tenant, user_id: Uuid) -> keycompute_types::TenantScope {
    keycompute_types::TenantScope::checked(tenant.id, user_id, TenantRole::Admin).unwrap()
}

fn tenant_member(tenant: &Tenant, user_id: Uuid) -> keycompute_types::TenantScope {
    keycompute_types::TenantScope::checked(tenant.id, user_id, TenantRole::Member).unwrap()
}

fn root_scope(user: &User) -> PlatformScope {
    PlatformScope::checked(user.id, PlatformRole::Root).unwrap()
}

async fn tenant_snapshot(
    db: &DatabaseConnection,
    tenant: &Tenant,
    user: &User,
) -> ProviderAuthzSnapshot {
    let member = keycompute_db::TenantMembership::find(db, tenant.id, user.id)
        .await
        .unwrap()
        .expect("active fixture membership");
    ProviderAuthzSnapshot::tenant(
        user.token_version,
        tenant.authz_version,
        member.authz_version,
    )
}

fn root_snapshot(user: &User) -> ProviderAuthzSnapshot {
    ProviderAuthzSnapshot::platform(user.token_version)
}

fn audit_tenant(
    scope: keycompute_types::TenantScope,
    request_id: Option<Uuid>,
) -> keycompute_db::AuditContext {
    keycompute_db::AuditContext {
        actor_user_id: scope.user_id(),
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::None,
        actor_tenant_role: Some(scope.tenant_role()),
        request_id,
    }
}

fn audit_root(scope: PlatformScope, request_id: Option<Uuid>) -> keycompute_db::AuditContext {
    keycompute_db::AuditContext {
        actor_user_id: scope.user_id(),
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::Root,
        actor_tenant_role: None,
        request_id,
    }
}

fn account_request(tenant_id: Uuid, name: &str, visibility: &str) -> CreateAccountRequest {
    CreateAccountRequest {
        tenant_id,
        provider: "openai".into(),
        name: name.into(),
        endpoint: "https://provider.example/v1".into(),
        upstream_api_key_encrypted: "encrypted-fixture-secret".into(),
        upstream_api_key_preview: "enc***".into(),
        rpm_limit: Some(100),
        tpm_limit: Some(100_000),
        priority: Some(1),
        // Global grants share one model namespace, so each fixture needs its
        // own model even when the database is deliberately shared in CI.
        models_supported: vec![format!("gpt-scoped-{tenant_id}")],
        api_capabilities: vec!["chat_completions".into(), "responses".into()],
        visibility: Some(visibility.into()),
        pool_enabled: Some(true),
    }
}

fn update_name(name: &str) -> UpdateAccountRequest {
    UpdateAccountRequest {
        tenant_id: None,
        name: Some(name.into()),
        endpoint: None,
        upstream_api_key_encrypted: None,
        upstream_api_key_preview: None,
        rpm_limit: None,
        tpm_limit: None,
        priority: None,
        enabled: None,
        models_supported: None,
        api_capabilities: None,
        visibility: None,
        pool_enabled: None,
    }
}

struct Fixture {
    db: DatabaseConnection,
    guard: TestDataGuard,
    run_id: String,
    a: Tenant,
    b: Tenant,
    a_admin: TenantActor,
    b_admin: TenantActor,
    b_member: TenantActor,
    account_a: Account,
    account_b: Account,
    root: User,
}

impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run_id = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run_id.clone());
        let a = create_test_tenant(&db, "provider-scope-a", &run_id).await;
        let b = create_test_tenant(&db, "provider-scope-b", &run_id).await;
        let a_admin = create_test_user(&db, a.id, "provider-a-admin", &run_id).await;
        let b_admin = create_test_user(&db, b.id, "provider-b-admin", &run_id).await;
        let b_member = create_test_user(&db, b.id, "provider-b-member", &run_id).await;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET tenant_role='admin' WHERE (tenant_id=$1 AND user_id=$2) OR (tenant_id=$3 AND user_id=$4)",
            [a.id.into(), a_admin.id.into(), b.id.into(), b_admin.id.into()],
        ))
        .await
        .unwrap();

        let root_id = db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT id FROM users WHERE platform_role='root' AND status='active' ORDER BY created_at LIMIT 1",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap();
        let root = User::find_by_id(&db, root_id).await.unwrap().unwrap();

        let root_scope = root_scope(&root);
        let account_a = Account::create_platform(
            &db,
            root_scope,
            a.id,
            &account_request(a.id, &format!("literal_%_account_{run_id}"), "global"),
            &audit_root(root_scope, Some(Uuid::new_v4())),
            root_snapshot(&root),
        )
        .await
        .unwrap();
        let account_b = Account::create_platform(
            &db,
            root_scope,
            b.id,
            &account_request(b.id, &format!("tenant-b-account-{run_id}"), "tenant"),
            &audit_root(root_scope, Some(Uuid::new_v4())),
            root_snapshot(&root),
        )
        .await
        .unwrap();

        Self {
            db,
            guard,
            run_id,
            a,
            b,
            a_admin,
            b_admin,
            b_member,
            account_a,
            account_b,
            root,
        }
    }

    fn a_scope(&self) -> keycompute_types::TenantScope {
        tenant_admin(&self.a, self.a_admin.id)
    }

    fn b_scope(&self) -> keycompute_types::TenantScope {
        tenant_admin(&self.b, self.b_admin.id)
    }

    fn root_scope(&self) -> PlatformScope {
        root_scope(&self.root)
    }

    async fn create_binding(
        &self,
        account_id: Uuid,
        tenant_id: Uuid,
        is_global: bool,
    ) -> keycompute_db::models::passthrough_binding::PassthroughBinding {
        let scope = self.root_scope();
        keycompute_db::models::passthrough_binding::PassthroughBinding::create_platform(
            &self.db,
            scope,
            &CreatePassthroughBindingRequest {
                account_id,
                tenant_id,
                is_global,
                pool_enabled: true,
            },
            &audit_root(scope, Some(Uuid::new_v4())),
            root_snapshot(&self.root),
        )
        .await
        .unwrap()
    }

    async fn cleanup(&mut self) {
        self.guard.cleanup().await.unwrap();
    }
}

#[tokio::test]
async fn account_reads_are_scoped_and_list_count_filters_match() {
    let mut f = Fixture::new().await;
    let filter = AccountListFilter {
        provider: Some("openai".into()),
        enabled: Some(true),
        search: Some("literal_%".into()),
        tenant_id: Some(f.a.id),
    };
    let rows = Account::list_in_tenant(&f.db, f.a_scope(), &filter, 50, 0)
        .await
        .unwrap();
    let total = Account::count_in_tenant(&f.db, f.a_scope(), &filter)
        .await
        .unwrap();
    assert_eq!(rows.len() as i64, total);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, f.account_a.id);
    let json = serde_json::to_value(&rows[0]).unwrap();
    assert!(json.get("upstream_api_key_encrypted").is_none());
    assert!(json.get("upstream_api_key_preview").is_some());

    assert!(
        Account::list_in_tenant(
            &f.db,
            tenant_member(&f.b, f.b_member.id),
            &AccountListFilter::default(),
            50,
            0
        )
        .await
        .is_err()
    );
    assert!(
        Account::find_in_tenant(&f.db, f.b_scope(), f.account_a.id)
            .await
            .unwrap()
            .is_none()
    );

    let root_filter = AccountListFilter {
        search: Some(f.run_id.clone()),
        ..Default::default()
    };
    let root_rows = Account::list_platform(&f.db, f.root_scope(), &root_filter, 50, 0)
        .await
        .unwrap();
    let root_total = Account::count_platform(&f.db, f.root_scope(), &root_filter)
        .await
        .unwrap();
    assert_eq!(root_rows.len() as i64, root_total);
    assert!(root_rows.iter().any(|row| row.id == f.account_a.id));
    assert!(root_rows.iter().any(|row| row.id == f.account_b.id));
    f.cleanup().await;
}

#[tokio::test]
async fn tenant_cannot_transfer_account_but_root_can_transfer_atomically() {
    let mut f = Fixture::new().await;
    let before = Account::find_by_id(&f.db, f.account_a.id)
        .await
        .unwrap()
        .unwrap();
    let denied = Account::update_in_tenant(
        &f.db,
        f.a_scope(),
        before.id,
        &UpdateAccountRequest {
            tenant_id: Some(f.b.id),
            ..update_name(&before.name)
        },
        before.upstream_config_version,
        &audit_tenant(f.a_scope(), Some(Uuid::new_v4())),
        tenant_snapshot(&f.db, &f.a, &f.a_admin).await,
    )
    .await;
    assert!(denied.is_err());
    assert_eq!(
        Account::find_by_id(&f.db, before.id)
            .await
            .unwrap()
            .unwrap()
            .tenant_id,
        f.a.id
    );

    let transferred = Account::update_platform(
        &f.db,
        f.root_scope(),
        before.id,
        &UpdateAccountRequest {
            tenant_id: Some(f.b.id),
            ..update_name(&before.name)
        },
        before.upstream_config_version,
        &audit_root(f.root_scope(), Some(Uuid::new_v4())),
        root_snapshot(&f.root),
    )
    .await
    .unwrap();
    assert_eq!(transferred.tenant_id, f.b.id);
    f.cleanup().await;
}

#[tokio::test]
async fn global_consumption_does_not_grant_foreign_binding_ownership() {
    let mut f = Fixture::new().await;
    let own = f.create_binding(f.account_a.id, f.a.id, false).await;
    let foreign_global = f.create_binding(f.account_a.id, f.b.id, true).await;

    assert!(
        keycompute_db::models::passthrough_binding::PassthroughBinding::find_in_tenant(
            &f.db,
            f.a_scope(),
            own.id
        )
        .await
        .unwrap()
        .is_some()
    );
    assert!(
        keycompute_db::models::passthrough_binding::PassthroughBinding::find_in_tenant(
            &f.db,
            f.b_scope(),
            foreign_global.id
        )
        .await
        .unwrap()
        .is_none()
    );
    assert!(
        keycompute_db::models::passthrough_binding::PassthroughBinding::list_in_tenant(
            &f.db,
            f.b_scope(),
            &PassthroughBindingListFilter {
                tenant_id: Some(f.b.id),
                search: None,
            },
            50,
            0,
        )
        .await
        .unwrap()
        .is_empty()
    );
    // The schema permits one binding per account/tenant. Reanchor the one
    // global grant through root authority after removing the local grant;
    // neither duplicate account/tenant pairs nor two global grants are legal.
    keycompute_db::PassthroughBinding::delete_platform(
        &f.db,
        f.root_scope(),
        own.id,
        own.revision,
        &audit_root(f.root_scope(), Some(Uuid::new_v4())),
        root_snapshot(&f.root),
    )
    .await
    .unwrap();
    let owned_global = keycompute_db::PassthroughBinding::update_platform(
        &f.db,
        f.root_scope(),
        foreign_global.id,
        &keycompute_db::UpdatePassthroughBindingRequest {
            account_id: None,
            tenant_id: Some(f.a.id),
            is_global: None,
            pool_enabled: None,
            expected_revision: foreign_global.revision,
        },
        &audit_root(f.root_scope(), Some(Uuid::new_v4())),
        root_snapshot(&f.root),
    )
    .await
    .unwrap();
    assert!(
        keycompute_db::models::passthrough_binding::PassthroughBinding::update_in_tenant(
            &f.db,
            f.a_scope(),
            owned_global.id,
            &keycompute_db::models::passthrough_binding::UpdatePassthroughBindingRequest {
                account_id: None,
                tenant_id: None,
                is_global: None,
                pool_enabled: Some(false),
                expected_revision: owned_global.revision,
            },
            &audit_tenant(f.a_scope(), Some(Uuid::new_v4())),
            tenant_snapshot(&f.db, &f.a, &f.a_admin).await,
        )
        .await
        .is_err()
    );
    assert!(
        keycompute_db::models::passthrough_binding::PassthroughBinding::find_platform(
            &f.db,
            f.root_scope(),
            foreign_global.id
        )
        .await
        .unwrap()
        .is_some()
    );
    let binding_json = serde_json::to_value(
        keycompute_db::models::passthrough_binding::PassthroughBinding::find_platform(
            &f.db,
            f.root_scope(),
            foreign_global.id,
        )
        .await
        .unwrap()
        .unwrap(),
    )
    .unwrap();
    assert!(binding_json.get("endpoint").is_none());
    assert!(binding_json.get("upstream_api_key_encrypted").is_none());
    f.cleanup().await;
}

#[tokio::test]
async fn actor_credential_and_operator_mismatches_have_no_side_effect() {
    let mut f = Fixture::new().await;
    let before_name = f.account_a.name.clone();
    let before_audits: i64 = f
        .db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*) FROM tenant_audit_events WHERE resource_type='account' AND resource_id=$1",
            [f.account_a.id.to_string().into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();

    let mut wrong_actor = audit_tenant(f.a_scope(), Some(Uuid::new_v4()));
    wrong_actor.actor_user_id = f.b_admin.id;
    assert!(
        Account::update_in_tenant(
            &f.db,
            f.a_scope(),
            f.account_a.id,
            &update_name("must-not-apply"),
            f.account_a.upstream_config_version,
            &wrong_actor,
            tenant_snapshot(&f.db, &f.a, &f.a_admin).await,
        )
        .await
        .is_err()
    );
    let mut wrong_credential = audit_tenant(f.a_scope(), Some(Uuid::new_v4()));
    wrong_credential.credential_kind = CredentialKind::ApiKey;
    assert!(
        Account::update_in_tenant(
            &f.db,
            f.a_scope(),
            f.account_a.id,
            &update_name("must-not-apply"),
            f.account_a.upstream_config_version,
            &wrong_credential,
            tenant_snapshot(&f.db, &f.a, &f.a_admin).await,
        )
        .await
        .is_err()
    );

    let operator = User::create(
        &f.db,
        &keycompute_db::CreateUserRequest {
            email: format!("test-provider-operator-{}@example.com", f.run_id),
            name: Some("provider operator".into()),
        },
    )
    .await
    .unwrap();
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET platform_role='operator' WHERE id=$1",
        [operator.id.into()],
    ))
    .await
    .unwrap();
    let operator_scope = PlatformScope::checked(operator.id, PlatformRole::Operator).unwrap();
    assert!(
        Account::count_platform(&f.db, operator_scope, &AccountListFilter::default())
            .await
            .is_err()
    );

    let after = Account::find_by_id(&f.db, f.account_a.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.name, before_name);
    let after_audits: i64 = f
        .db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*) FROM tenant_audit_events WHERE resource_type='account' AND resource_id=$1",
            [f.account_a.id.to_string().into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    assert_eq!(after_audits, before_audits);
    f.cleanup().await;
}

#[tokio::test]
async fn foreign_writes_revision_conflicts_and_disable_then_revoke_are_guarded() {
    let mut f = Fixture::new().await;
    let binding = f.create_binding(f.account_a.id, f.a.id, false).await;
    let foreign_before = Account::find_by_id(&f.db, f.account_b.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        Account::update_in_tenant(
            &f.db,
            f.a_scope(),
            f.account_b.id,
            &update_name("foreign-write"),
            foreign_before.upstream_config_version,
            &audit_tenant(f.a_scope(), Some(Uuid::new_v4())),
            tenant_snapshot(&f.db, &f.a, &f.a_admin).await,
        )
        .await
        .is_err()
    );
    assert_eq!(
        Account::find_by_id(&f.db, f.account_b.id)
            .await
            .unwrap()
            .unwrap()
            .name,
        foreign_before.name
    );

    let changed = keycompute_db::models::passthrough_binding::PassthroughBinding::update_platform(
        &f.db,
        f.root_scope(),
        binding.id,
        &keycompute_db::models::passthrough_binding::UpdatePassthroughBindingRequest {
            account_id: None,
            tenant_id: None,
            is_global: None,
            pool_enabled: Some(false),
            expected_revision: binding.revision,
        },
        &audit_root(f.root_scope(), Some(Uuid::new_v4())),
        root_snapshot(&f.root),
    )
    .await
    .unwrap();
    assert_eq!(changed.revision, binding.revision + 1);
    assert!(
        keycompute_db::models::passthrough_binding::PassthroughBinding::update_platform(
            &f.db,
            f.root_scope(),
            binding.id,
            &keycompute_db::models::passthrough_binding::UpdatePassthroughBindingRequest {
                account_id: None,
                tenant_id: None,
                is_global: None,
                pool_enabled: Some(true),
                expected_revision: binding.revision,
            },
            &audit_root(f.root_scope(), Some(Uuid::new_v4())),
            root_snapshot(&f.root),
        )
        .await
        .is_err()
    );

    let account = Account::find_by_id(&f.db, f.account_a.id)
        .await
        .unwrap()
        .unwrap();
    let disabled = Account::update_platform(
        &f.db,
        f.root_scope(),
        account.id,
        &UpdateAccountRequest {
            enabled: Some(false),
            ..update_name(&account.name)
        },
        account.upstream_config_version,
        &audit_root(f.root_scope(), Some(Uuid::new_v4())),
        root_snapshot(&f.root),
    )
    .await
    .unwrap();
    assert!(!disabled.enabled);
    keycompute_db::models::passthrough_binding::PassthroughBinding::delete_platform(
        &f.db,
        f.root_scope(),
        changed.id,
        changed.revision,
        &audit_root(f.root_scope(), Some(Uuid::new_v4())),
        root_snapshot(&f.root),
    )
    .await
    .unwrap();
    assert!(
        keycompute_db::models::passthrough_binding::PassthroughBinding::find_platform(
            &f.db,
            f.root_scope(),
            changed.id
        )
        .await
        .unwrap()
        .is_none()
    );
    f.cleanup().await;
}

#[tokio::test]
async fn pending_settlements_and_audit_failures_roll_back_account_changes() {
    let mut f = Fixture::new().await;
    let pending = Account::create(
        &f.db,
        &account_request(f.a.id, &format!("pending-{0}", f.run_id), "tenant"),
    )
    .await
    .unwrap();
    let response_id = format!("responses-pending-{}", f.run_id);
    ResponseAffinity::upsert_hidden_settlement(
        &f.db,
        f.a.id,
        &response_id,
        "openai",
        Some(&format!("gpt-scoped-{}", f.a.id)),
        Some(pending.id),
        Utc::now() + Duration::hours(1),
        json!({"terminal_status":"success","account_id":pending.id}),
        Utc::now(),
    )
    .await
    .unwrap();
    let updated = Account::update_in_tenant(
        &f.db,
        f.a_scope(),
        pending.id,
        &update_name("pending-no-op-update"),
        pending.upstream_config_version,
        &audit_tenant(f.a_scope(), Some(Uuid::new_v4())),
        tenant_snapshot(&f.db, &f.a, &f.a_admin).await,
    )
    .await
    .unwrap();
    assert_eq!(updated.name, "pending-no-op-update");
    assert!(matches!(
        Account::delete_in_tenant(
            &f.db,
            f.a_scope(),
            pending.id,
            &audit_tenant(f.a_scope(), Some(Uuid::new_v4())),
            tenant_snapshot(&f.db, &f.a, &f.a_admin).await,
        )
        .await,
        Err(DbError::Other(message)) if message.contains("pending Responses")
    ));
    assert!(
        Account::find_by_id(&f.db, pending.id)
            .await
            .unwrap()
            .is_some()
    );

    let request_id = Uuid::new_v4();
    let tx = f.db.begin().await.unwrap();
    // Fault-injection DDL must follow the same fence-before-audit-table order
    // as production writes; do not recreate the CI #108 lock inversion.
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    tx.execute_unprepared(&format!(
        "CREATE FUNCTION pg_temp.reject_provider_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'provider audit failure'; END $$;
         CREATE TRIGGER kc_provider_audit_failure BEFORE INSERT ON tenant_audit_events
         FOR EACH ROW WHEN (NEW.request_id='{}'::uuid) EXECUTE FUNCTION pg_temp.reject_provider_audit()",
        request_id
    ))
    .await
    .unwrap();
    let audit = audit_tenant(f.a_scope(), Some(request_id));
    let audit_account = account_request(f.a.id, &format!("audit-{}", f.run_id), "tenant");
    assert!(
        Account::create_in_tenant(
            &tx,
            f.a_scope(),
            &audit_account,
            &audit,
            tenant_snapshot(&f.db, &f.a, &f.a_admin).await,
        )
        .await
        .is_err()
    );
    assert!(
        Account::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM accounts WHERE tenant_id=$1 AND name=$2",
            [f.a.id.into(), audit_account.name.clone().into()],
        ))
        .one(&tx)
        .await
        .unwrap()
        .is_none()
    );
    tx.rollback().await.unwrap();
    f.cleanup().await;
}

#[tokio::test]
async fn demotion_while_waiting_on_tenant_lock_is_rechecked_before_commit() {
    let mut f = Fixture::new().await;
    let demoted = create_test_user(&f.db, f.a.id, "provider-demoted-admin", &f.run_id).await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
        [f.a.id.into(), demoted.id.into()],
    ))
    .await
    .unwrap();
    let scope = tenant_admin(&f.a, demoted.id);
    let audit = audit_tenant(scope, Some(Uuid::new_v4()));
    let account = Account::find_by_id(&f.db, f.account_a.id)
        .await
        .unwrap()
        .unwrap();
    let snapshot = tenant_snapshot(&f.db, &f.a, &demoted).await;
    let demoted_id = demoted.id;
    let holder = f.db.begin().await.unwrap();
    // Follow production admin ordering before holding the tenant row.
    holder
        .execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    Tenant::find_by_id_for_update(&holder, f.a.id)
        .await
        .unwrap()
        .unwrap();
    let db = f.db.clone();
    let request = update_name("demotion-must-win");
    let task = tokio::spawn(async move {
        Account::update_in_tenant(
            &db,
            scope,
            account.id,
            &request,
            account.upstream_config_version,
            &audit,
            snapshot,
        )
        .await
    });
    tokio::time::sleep(StdDuration::from_millis(100)).await;
    holder
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET status='suspended',authz_version=authz_version+1 WHERE tenant_id=$1 AND user_id=$2",
            [f.a.id.into(), demoted_id.into()],
        ))
        .await
        .unwrap();
    holder.commit().await.unwrap();
    assert!(
        tokio::time::timeout(StdDuration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    f.cleanup().await;
}

#[tokio::test]
async fn probe_material_and_binding_counts_follow_the_current_authorized_owner() {
    use keycompute_db::models::account::AccountManagementScope;
    let mut f = Fixture::new().await;
    let _own = f.create_binding(f.account_a.id, f.a.id, false).await;
    let _shared = f.create_binding(f.account_a.id, f.b.id, true).await;
    let own_view = Account::find_in_tenant(&f.db, f.a_scope(), f.account_a.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(own_view.passthrough_binding_count, 2);
    let platform_view = Account::find_platform(&f.db, f.root_scope(), f.account_a.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(platform_view.passthrough_binding_count, 2);
    assert!(
        Account::find_in_tenant(&f.db, f.b_scope(), f.account_a.id)
            .await
            .unwrap()
            .is_none()
    );
    let a_scope = AccountManagementScope::Tenant(f.a_scope());
    let a_snapshot = tenant_snapshot(&f.db, &f.a, &f.a_admin).await;
    let connection = Account::load_authorized_probe(&f.db, a_scope, f.account_a.id, a_snapshot)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connection.tenant_id, f.a.id);
    assert_eq!(
        connection.upstream_config_version,
        f.account_a.upstream_config_version
    );
    assert!(
        Account::load_authorized_probe(
            &f.db,
            AccountManagementScope::Tenant(f.b_scope()),
            f.account_a.id,
            tenant_snapshot(&f.db, &f.b, &f.b_admin).await
        )
        .await
        .unwrap()
        .is_none()
    );
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET token_version=token_version+1 WHERE id=$1",
        [f.a_admin.id.into()],
    ))
    .await
    .unwrap();
    assert!(
        Account::load_authorized_probe(&f.db, a_scope, f.account_a.id, a_snapshot)
            .await
            .unwrap()
            .is_none(),
        "old console session cannot obtain probe material"
    );
    let root_connection = Account::load_authorized_probe(
        &f.db,
        AccountManagementScope::Platform(f.root_scope()),
        f.account_a.id,
        root_snapshot(&f.root),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(root_connection.tenant_id, f.a.id);
    let before_transfer_scope = AccountManagementScope::Tenant(f.b_scope());
    let before_transfer_snapshot = tenant_snapshot(&f.db, &f.b, &f.b_admin).await;
    let transferred = Account::update_platform(
        &f.db,
        f.root_scope(),
        f.account_b.id,
        &UpdateAccountRequest {
            tenant_id: Some(f.a.id),
            ..update_name("probe-owner-moved")
        },
        f.account_b.upstream_config_version,
        &audit_root(f.root_scope(), Some(Uuid::new_v4())),
        root_snapshot(&f.root),
    )
    .await
    .unwrap();
    assert_eq!(transferred.tenant_id, f.a.id);
    assert!(
        Account::load_authorized_probe(
            &f.db,
            before_transfer_scope,
            transferred.id,
            before_transfer_snapshot
        )
        .await
        .unwrap()
        .is_none(),
        "a prior owner cannot read the new owner's connection by ID"
    );
    f.cleanup().await;
}
