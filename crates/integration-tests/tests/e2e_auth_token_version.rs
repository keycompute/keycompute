//! JWT token_version 失效机制端到端测试
//!
//! 覆盖回归点：`AppState` 构造的 `AuthService` 必须注入带数据库连接的
//! `UserService`，使 `verify_token` 能对 JWT 的 token_version 做数据库比对。
//! 若未正确接线（历史缺陷 B1），密码重置/登出后签发的旧 access token 仍能通过
//! 认证，token_version 失效机制形同虚设。

use chrono::{Duration as ChronoDuration, Utc};
use integration_tests::common::generate_test_id;
use integration_tests::db::{cleanup_test_data, create_test_pool, create_test_tenant};
use keycompute_auth::{
    AuthService, JwtValidator, LoginRequest, LoginService, PasswordHasher, PasswordResetService,
    ProduceAiKeyValidator, ResetPasswordRequest, UserService,
};
use keycompute_db::{
    CreatePasswordResetRequest, CreateProduceAiKeyRequest, CreateUserCredentialRequest,
    CreateUserRequest, DbRouter, PasswordReset, ProduceAiKey, Tenant, UpdateUserCredentialRequest,
    User, UserBalance, UserCredential, models::user::UpdateUserRequest,
};
use keycompute_types::UserRole;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use std::sync::Arc;
use std::time::Duration;

/// 构造一个与生产一致、带数据库连接的 AuthService（含 UserService 用于 token_version 校验）
fn build_auth_service(router: Arc<DbRouter>) -> AuthService {
    let jwt_validator = JwtValidator::new("test-secret", "keycompute");
    AuthService::new(ProduceAiKeyValidator::with_pool(Arc::clone(&router)))
        .with_jwt(jwt_validator)
        .with_user_service(UserService::with_pool(router))
}

/// token_version 递增后，旧 token 必须在 verify_token 处被拒绝
#[tokio::test]
async fn test_verify_token_rejected_after_token_version_bump() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("cleanup should succeed");

    let tenant = create_test_tenant(&pool, "tv-verify", &test_id).await;
    let user = User::create(
        &pool,
        &CreateUserRequest {
            tenant_id: tenant.id,
            email: format!("tv-verify-{}@example.com", test_id),
            name: Some("Token Version User".to_string()),
            role: Some(UserRole::User),
        },
    )
    .await
    .expect("user should be created");

    // 新建用户 token_version 默认为 0
    assert_eq!(user.token_version, 0, "新用户 token_version 应为 0");

    let router = DbRouter::single(pool.clone());
    let auth = build_auth_service(Arc::clone(&router));
    let jwt = auth
        .get_jwt_validator()
        .expect("jwt validator configured")
        .clone();

    // 使用用户当前 token_version 签发 token
    let token = jwt
        .generate_token_with_version(user.id, user.tenant_id, &user.role, user.token_version)
        .expect("token should be generated");

    // 1. 递增前：token 有效
    let ctx = auth
        .verify_token(&token)
        .await
        .expect("token should be valid before version bump");
    assert_eq!(ctx.user_id, user.id);
    assert_eq!(ctx.token_version, 0);

    // 2. 递增 token_version（模拟密码重置/登出）
    let new_version = User::increment_token_version(&pool, user.id)
        .await
        .expect("increment should succeed");
    assert_eq!(new_version, 1, "递增后 token_version 应为 1");

    // 3. 递增后：旧 token 必须被拒绝（关键回归断言）
    let result = auth.verify_token(&token).await;
    cleanup_test_data(&pool, &test_id).await.ok();

    let err = result.expect_err("stale token must be rejected after token_version bump");
    assert!(
        err.to_string().contains("invalidated"),
        "错误信息应表明 token 已失效，实际: {err}"
    );
}

/// 无 UserService（无数据库连接）时，verify_token 退化为纯结构性校验，不做 token_version 比对
#[tokio::test]
async fn test_verify_token_without_user_service_skips_version_check() {
    let jwt_validator = JwtValidator::new("test-secret", "keycompute");
    // 注意：未调用 with_user_service，模拟无数据库连接场景
    let auth = AuthService::new(ProduceAiKeyValidator::new()).with_jwt(jwt_validator);

    let jwt = auth.get_jwt_validator().expect("jwt configured").clone();
    // 即便 token 携带非零 token_version，无 UserService 时也不会比对数据库
    let token = jwt
        .generate_token_with_version(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "user", 42)
        .expect("token generated");

    let ctx = auth
        .verify_token(&token)
        .await
        .expect("without user_service, verification is structural only");
    assert_eq!(ctx.token_version, 42);
}

/// 覆盖模型层新增方法：increment_token_version / find_token_version
#[tokio::test]
async fn test_user_token_version_model_methods() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("cleanup should succeed");

    let tenant = create_test_tenant(&pool, "tv-model", &test_id).await;
    let user = User::create(
        &pool,
        &CreateUserRequest {
            tenant_id: tenant.id,
            email: format!("tv-model-{}@example.com", test_id),
            name: Some("TV Model User".to_string()),
            role: Some(UserRole::User),
        },
    )
    .await
    .expect("user should be created");

    // find_token_version 初始为 0
    let v0 = User::find_token_version(&pool, user.id)
        .await
        .expect("query should succeed");
    assert_eq!(v0, Some(0));

    // 连续两次递增，返回值单调递增
    let v1 = User::increment_token_version(&pool, user.id)
        .await
        .expect("first increment");
    let v2 = User::increment_token_version(&pool, user.id)
        .await
        .expect("second increment");
    assert_eq!(v1, 1);
    assert_eq!(v2, 2);

    // find_token_version 反映最新值
    let latest = User::find_token_version(&pool, user.id)
        .await
        .expect("query should succeed");
    assert_eq!(latest, Some(2));

    // 不存在的用户返回 None
    let missing = User::find_token_version(&pool, uuid::Uuid::new_v4())
        .await
        .expect("query should succeed");
    assert_eq!(missing, None);

    cleanup_test_data(&pool, &test_id).await.ok();
}

/// 角色承载授权边界；发生实际角色变化时必须原子递增 token_version，
/// 仅修改名称或重复写入相同角色则不能无故注销用户。
#[tokio::test]
async fn test_role_change_invalidates_existing_jwt_only_when_role_changes() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("cleanup should succeed");

    let tenant = create_test_tenant(&pool, "tv-role", &test_id).await;
    let user = User::create(
        &pool,
        &CreateUserRequest {
            tenant_id: tenant.id,
            email: format!("tv-role-{}@example.com", test_id),
            name: Some("Role Token User".to_string()),
            role: Some(UserRole::Admin),
        },
    )
    .await
    .expect("user should be created");
    let router = DbRouter::single(pool.clone());
    let auth = build_auth_service(Arc::clone(&router));
    let jwt = auth
        .get_jwt_validator()
        .expect("jwt validator configured")
        .clone();
    let admin_token = jwt
        .generate_token_with_version(user.id, user.tenant_id, &user.role, user.token_version)
        .expect("admin token should be generated");

    let renamed = user
        .update(
            &pool,
            &UpdateUserRequest {
                name: Some("Renamed Admin".to_string()),
                role: None,
                tenant_id: None,
            },
        )
        .await
        .expect("name update should succeed");
    assert_eq!(renamed.token_version, 0);
    auth.verify_token(&admin_token)
        .await
        .expect("name-only update must not invalidate the token");

    let unchanged_role = renamed
        .update(
            &pool,
            &UpdateUserRequest {
                name: None,
                role: Some(keycompute_types::AssignableUserRole::Admin),
                tenant_id: None,
            },
        )
        .await
        .expect("same-role update should succeed");
    assert_eq!(unchanged_role.token_version, 0);

    let demoted = unchanged_role
        .update(
            &pool,
            &UpdateUserRequest {
                name: None,
                role: Some(keycompute_types::AssignableUserRole::User),
                tenant_id: None,
            },
        )
        .await
        .expect("role update should succeed");
    assert_eq!(demoted.role, UserRole::User.as_str());
    assert_eq!(demoted.token_version, 1);

    let error = auth
        .verify_token(&admin_token)
        .await
        .expect_err("pre-demotion admin token must be invalidated");
    assert!(error.to_string().contains("invalidated"));

    cleanup_test_data(&pool, &test_id).await.ok();
}

/// 租户归属变化与角色变化一样必须立即使旧 JWT 失效。
#[tokio::test]
async fn test_tenant_change_invalidates_existing_jwt() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("cleanup should succeed");

    let source_tenant = create_test_tenant(&pool, "tv-tenant-source", &test_id).await;
    let target_tenant = create_test_tenant(&pool, "tv-tenant-target", &test_id).await;
    let user = User::create(
        &pool,
        &CreateUserRequest {
            tenant_id: source_tenant.id,
            email: format!("tv-tenant-{}@example.com", test_id),
            name: Some("Tenant Move User".to_string()),
            role: Some(UserRole::User),
        },
    )
    .await
    .expect("user should be created");
    let router = DbRouter::single(pool.clone());
    let auth = build_auth_service(Arc::clone(&router));
    let jwt = auth
        .get_jwt_validator()
        .expect("jwt validator configured")
        .clone();
    let token = jwt
        .generate_token_with_version(user.id, user.tenant_id, &user.role, user.token_version)
        .expect("token should be generated");

    let api_key = ProduceAiKeyValidator::generate_key();
    ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: source_tenant.id,
            user_id: user.id,
            name: "tenant-move-key".to_string(),
            produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&api_key),
            produce_ai_key_preview: "sk-test-****".to_string(),
            expires_at: None,
        },
    )
    .await
    .expect("API key should be created");

    let tx = pool.begin().await.expect("transaction should begin");
    let locked = User::find_by_id_for_no_key_update(&tx, user.id)
        .await
        .expect("user lookup should succeed")
        .expect("user should exist");
    UserBalance::reassign_tenant(&tx, user.id, target_tenant.id)
        .await
        .expect("balance should move");
    ProduceAiKey::revoke_all_for_user(&tx, user.id)
        .await
        .expect("API keys should be revoked");
    let moved = locked
        .update_in_tx(
            &tx,
            &UpdateUserRequest {
                name: None,
                role: None,
                tenant_id: Some(target_tenant.id),
            },
        )
        .await
        .expect("tenant update should succeed");
    tx.commit().await.expect("transaction should commit");
    assert_eq!(moved.tenant_id, target_tenant.id);
    assert_eq!(moved.token_version, user.token_version + 1);

    let error = auth
        .verify_token(&token)
        .await
        .expect_err("tenant move must invalidate the old token");
    assert!(error.to_string().contains("invalidated"));

    let key_error = auth
        .verify_token(&api_key)
        .await
        .expect_err("tenant move must invalidate the old API key");
    assert!(key_error.to_string().contains("revoked"));

    // A request that was authenticated before the move must not create a new
    // key under the former tenant after the move has committed.
    let stale_key_error = ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: source_tenant.id,
            user_id: user.id,
            name: "stale-tenant-key".to_string(),
            produce_ai_key_hash: format!("stale-key-{test_id}"),
            produce_ai_key_preview: "stale-key".to_string(),
            expires_at: None,
        },
    )
    .await
    .expect_err("a stale tenant API-key create must be rejected");
    assert!(matches!(
        stale_key_error,
        keycompute_db::DbError::UserTenantMismatch {
            requested_tenant_id,
            actual_tenant_id,
            ..
        } if requested_tenant_id == source_tenant.id && actual_tenant_id == target_tenant.id
    ));
    cleanup_test_data(&pool, &test_id).await.ok();
}

/// API-key validation must linearize with revocation.  In particular, a
/// validator that has already read the key hash must not be able to wait on a
/// user lock, let the move/revocation commit, and then authenticate from the
/// stale pre-revocation snapshot.
#[tokio::test]
async fn test_api_key_validation_rejects_revocation_while_waiting_for_user_lock() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("cleanup should succeed");

    let tenant = create_test_tenant(&pool, "tv-api-key-race", &test_id).await;
    let user = User::create(
        &pool,
        &CreateUserRequest {
            tenant_id: tenant.id,
            email: format!("tv-api-key-race-{}@example.com", test_id),
            name: Some("API Key Race User".to_string()),
            role: Some(UserRole::User),
        },
    )
    .await
    .expect("user should be created");
    let api_key = ProduceAiKeyValidator::generate_key();
    let key_row = ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: "api-key-race".to_string(),
            produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&api_key),
            produce_ai_key_preview: "sk-race-****".to_string(),
            expires_at: None,
        },
    )
    .await
    .expect("API key should be created");

    // Hold the same user lock that tenant reassignment acquires.  The
    // validator will have to wait after its initial hash lookup, giving this
    // transaction a deterministic opportunity to revoke the key.
    let gate = pool.begin().await.expect("gate transaction should begin");
    User::find_by_id_for_no_key_update(&gate, user.id)
        .await
        .expect("user lock should succeed")
        .expect("user should exist");

    let router = DbRouter::single(pool.clone());
    let validator = ProduceAiKeyValidator::with_pool(Arc::clone(&router));
    let key_for_task = api_key.clone();
    let validation_task = tokio::spawn(async move { validator.validate(&key_for_task).await });

    #[derive(Debug, FromQueryResult)]
    struct Waiting {
        waiting: bool,
    }
    let mut validator_is_waiting = false;
    for _ in 0..300 {
        let row = pool
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND query LIKE '%FROM users WHERE id = $1 FOR NO KEY UPDATE%') AS waiting".to_string(),
            ))
            .await
            .expect("lock wait probe should succeed")
            .expect("lock wait probe should return a row");
        if Waiting::from_query_result(&row, "")
            .expect("waiting flag should decode")
            .waiting
        {
            validator_is_waiting = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        validator_is_waiting,
        "API-key validation should wait on the user lock after its candidate lookup"
    );

    ProduceAiKey::revoke_all_for_user(&gate, user.id)
        .await
        .expect("revocation should succeed while the gate owns the user lock");
    gate.commit()
        .await
        .expect("revocation transaction should commit");

    let validation_result = tokio::time::timeout(Duration::from_secs(10), validation_task)
        .await
        .expect("validation should finish after the user lock is released")
        .expect("validation task should not panic");
    let error = validation_result.expect_err("revoked API key must be rejected");
    assert!(
        error.to_string().contains("revoked"),
        "error should identify revocation, got: {error}"
    );

    // Keep the row used by the assertion explicit so a future refactor does
    // not accidentally remove the key before the validator observes it.
    assert_eq!(key_row.user_id, user.id);
    cleanup_test_data(&pool, &test_id).await.ok();
}

/// Authentication and tenant deletion must acquire locks in parent-first
/// order.  This exercises the model-level delete path directly (the HTTP
/// handler normally rejects tenants that still have users): a child-first
/// validator would hold the user row while waiting for the tenant and deadlock
/// when the delete cascades back to that user.
#[tokio::test]
async fn test_api_key_validation_does_not_deadlock_with_tenant_delete() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("cleanup should succeed");

    let tenant = create_test_tenant(&pool, "tv-api-key-delete-race", &test_id).await;
    let user = User::create(
        &pool,
        &CreateUserRequest {
            tenant_id: tenant.id,
            email: format!("tv-api-key-delete-race-{}@example.com", test_id),
            name: Some("API Key Delete Race User".to_string()),
            role: Some(UserRole::User),
        },
    )
    .await
    .expect("user should be created");
    let api_key = ProduceAiKeyValidator::generate_key();
    ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: "api-key-delete-race".to_string(),
            produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&api_key),
            produce_ai_key_preview: "sk-delete-****".to_string(),
            expires_at: None,
        },
    )
    .await
    .expect("API key should be created");

    // Hold the tenant parent lock exactly as Tenant::delete_in_tx does before
    // starting its cascade.
    let delete_tx = pool.begin().await.expect("delete transaction should begin");
    Tenant::find_by_id_for_update(&delete_tx, tenant.id)
        .await
        .expect("tenant lock should succeed")
        .expect("tenant should exist");

    let router = DbRouter::single(pool.clone());
    let validator = ProduceAiKeyValidator::with_pool(Arc::clone(&router));
    let key_for_task = api_key.clone();
    let validation_task = tokio::spawn(async move { validator.validate(&key_for_task).await });

    #[derive(Debug, FromQueryResult)]
    struct Waiting {
        waiting: bool,
    }
    let mut validator_is_waiting = false;
    for _ in 0..300 {
        let row = pool
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND query LIKE '%FROM tenants WHERE id = $1 FOR UPDATE%') AS waiting".to_string(),
            ))
            .await
            .expect("lock wait probe should succeed")
            .expect("lock wait probe should return a row");
        if Waiting::from_query_result(&row, "")
            .expect("waiting flag should decode")
            .waiting
        {
            validator_is_waiting = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        validator_is_waiting,
        "API-key validation should wait on the tenant parent lock"
    );

    // With parent-first validation, the delete can cascade users/keys without
    // waiting on a lock held by the validator.
    tokio::time::timeout(Duration::from_secs(3), tenant.delete_in_tx(&delete_tx))
        .await
        .expect("tenant delete should not deadlock")
        .expect("tenant delete should succeed");
    delete_tx
        .commit()
        .await
        .expect("tenant delete transaction should commit");

    let validation_result = tokio::time::timeout(Duration::from_secs(10), validation_task)
        .await
        .expect("validation should finish after tenant deletion")
        .expect("validation task should not panic");
    assert!(
        validation_result.is_err(),
        "an API key whose tenant was deleted must not authenticate"
    );

    cleanup_test_data(&pool, &test_id).await.ok();
}

/// Tenant status updates use PostgreSQL's NO KEY UPDATE row lock, which is
/// compatible with a KEY SHARE lock.  API-key validation therefore needs a
/// stronger parent lock: otherwise it can read an active tenant while an
/// uncommitted deactivation is in flight, commit authentication, and only then
/// observe the deactivation.  The validator must wait for the status update
/// and reject the key once the inactive state commits.
#[tokio::test]
async fn test_api_key_validation_serializes_with_tenant_deactivation() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("cleanup should succeed");

    let tenant = create_test_tenant(&pool, "tv-api-key-deactivation-race", &test_id).await;
    let user = User::create(
        &pool,
        &CreateUserRequest {
            tenant_id: tenant.id,
            email: format!("tv-api-key-deactivation-{}@example.com", test_id),
            name: Some("API Key Deactivation Race User".to_string()),
            role: Some(UserRole::User),
        },
    )
    .await
    .expect("user should be created");
    let api_key = ProduceAiKeyValidator::generate_key();
    ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: "api-key-deactivation-race".to_string(),
            produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&api_key),
            produce_ai_key_preview: "sk-deactivation-****".to_string(),
            expires_at: None,
        },
    )
    .await
    .expect("API key should be created");

    // Leave a regular UPDATE uncommitted.  It holds NO KEY UPDATE on the
    // tenant, which is intentionally compatible with FOR KEY SHARE (the
    // pre-fix validator lock) but conflicts with FOR UPDATE (the required
    // authorization lock).
    let deactivate_tx = pool
        .begin()
        .await
        .expect("deactivation transaction should begin");
    deactivate_tx
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenants SET status = 'inactive', updated_at = NOW() WHERE id = $1",
            [tenant.id.into()],
        ))
        .await
        .expect("tenant deactivation should execute");

    let router = DbRouter::single(pool.clone());
    let validator = ProduceAiKeyValidator::with_pool(Arc::clone(&router));
    let key_for_task = api_key.clone();
    let validation_task = tokio::spawn(async move { validator.validate(&key_for_task).await });

    #[derive(Debug, FromQueryResult)]
    struct Waiting {
        waiting: bool,
    }
    let mut validator_is_waiting = false;
    for _ in 0..300 {
        let row = pool
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND query LIKE '%FROM tenants WHERE id = $1 FOR UPDATE%') AS waiting".to_string(),
            ))
            .await
            .expect("tenant lock wait probe should succeed")
            .expect("tenant lock wait probe should return a row");
        if Waiting::from_query_result(&row, "")
            .expect("waiting flag should decode")
            .waiting
        {
            validator_is_waiting = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        validator_is_waiting,
        "API-key validation should wait for the in-flight tenant deactivation"
    );

    deactivate_tx
        .commit()
        .await
        .expect("tenant deactivation should commit");

    let validation_result = tokio::time::timeout(Duration::from_secs(10), validation_task)
        .await
        .expect("validation should finish after deactivation commits")
        .expect("validation task should not panic");
    let error = validation_result.expect_err("a key in an inactive tenant must be rejected");
    assert!(
        error.to_string().contains("Tenant is not active"),
        "error should identify inactive tenant, got: {error}"
    );

    cleanup_test_data(&pool, &test_id).await.ok();
}

/// If a password reset commits while Argon2 verifies the old hash, login must
/// not re-read the new token version and turn that stale password check into a
/// valid JWT. Holding the user row lock makes the final login snapshot wait,
/// so the credential replacement can be committed deterministically.
#[tokio::test]
async fn test_login_rejects_password_hash_changed_during_verification() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("cleanup should succeed");

    let tenant = create_test_tenant(&pool, "tv-login-race", &test_id).await;
    let user = User::create(
        &pool,
        &CreateUserRequest {
            tenant_id: tenant.id,
            email: format!("tv-login-race-{}@example.com", test_id),
            name: Some("Login Race User".to_string()),
            role: Some(UserRole::User),
        },
    )
    .await
    .expect("user should be created");
    let hasher = PasswordHasher::new();
    let old_password = "OldPassword123!";
    let old_hash = hasher.hash(old_password).expect("old password should hash");
    let new_hash = hasher
        .hash("NewPassword123!")
        .expect("new password should hash");
    let credential = UserCredential::create(
        &pool,
        &CreateUserCredentialRequest {
            user_id: user.id,
            password_hash: old_hash.clone(),
        },
    )
    .await
    .expect("credential should be created");
    credential
        .update(
            &pool,
            &UpdateUserCredentialRequest {
                email_verified: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("credential should be verified");

    // Block only the final user snapshot. The initial user/credential reads
    // and Argon2 verification can complete while this transaction is held.
    let gate = pool.begin().await.expect("gate transaction should begin");
    User::find_by_id_for_update(&gate, user.id)
        .await
        .expect("user lock should succeed")
        .expect("user should exist");

    let router = DbRouter::single(pool.clone());
    let login = LoginService::new(
        Arc::clone(&router),
        JwtValidator::new("test-secret", "keycompute"),
    );
    let email = user.email.clone();
    let login_task = tokio::spawn(async move {
        login
            .login(&LoginRequest {
                email,
                password: old_password.to_string(),
                client_ip: None,
            })
            .await
    });

    #[derive(Debug, FromQueryResult)]
    struct Waiting {
        waiting: bool,
    }
    let mut login_is_waiting = false;
    for _ in 0..300 {
        let row = pool
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND query LIKE '%FROM users WHERE id = $1 FOR UPDATE%') AS waiting".to_string(),
            ))
            .await
            .expect("lock wait probe should succeed")
            .expect("lock wait probe should return a row");
        if Waiting::from_query_result(&row, "")
            .expect("waiting flag should decode")
            .waiting
        {
            login_is_waiting = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        login_is_waiting,
        "login should reach the final user lock before the credential changes"
    );

    credential
        .update(
            &pool,
            &UpdateUserCredentialRequest {
                password_hash: Some(new_hash),
                ..Default::default()
            },
        )
        .await
        .expect("password replacement should commit while final user lock is held");
    gate.commit().await.expect("gate transaction should commit");

    let login_result = tokio::time::timeout(Duration::from_secs(10), login_task)
        .await
        .expect("login should finish after the user lock is released")
        .expect("login task should not panic");
    assert!(
        login_result.is_err(),
        "login must reject a password hash replaced during verification"
    );
    cleanup_test_data(&pool, &test_id).await.ok();
}

/// Password reset and successful login must acquire the user row before the
/// credential row.  The old reset path updated the credential first and then
/// bumped the user token version, which could deadlock with login's
/// user→credential transaction.  Holding the user lock lets this test observe
/// the reset's lock order without relying on scheduler timing.
#[tokio::test]
async fn test_password_reset_locks_user_before_credential() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("cleanup should succeed");

    let tenant = create_test_tenant(&pool, "tv-reset-lock-order", &test_id).await;
    let user = User::create(
        &pool,
        &CreateUserRequest {
            tenant_id: tenant.id,
            email: format!("tv-reset-lock-order-{}@example.com", test_id),
            name: Some("Reset Lock Order User".to_string()),
            role: Some(UserRole::User),
        },
    )
    .await
    .expect("user should be created");

    let hasher = PasswordHasher::new();
    let old_password = "OldPassword123!";
    let old_hash = hasher.hash(old_password).expect("old password should hash");
    let credential = UserCredential::create(
        &pool,
        &CreateUserCredentialRequest {
            user_id: user.id,
            password_hash: old_hash,
        },
    )
    .await
    .expect("credential should be created");
    credential
        .update(
            &pool,
            &UpdateUserCredentialRequest {
                email_verified: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("credential should be verified");

    let token = format!("reset-lock-order-{test_id}");
    PasswordReset::create(
        &pool,
        &CreatePasswordResetRequest {
            user_id: user.id,
            token: token.clone(),
            expires_at: Utc::now() + ChronoDuration::hours(1),
            requested_from_ip: None,
        },
    )
    .await
    .expect("reset token should be created");

    // Keep the user row locked while login reaches its final snapshot.  Login
    // is started first so, with the historical child-first reset path, it is
    // queued ahead of reset on the user row and exposes the reverse order.
    let gate = pool.begin().await.expect("gate transaction should begin");
    User::find_by_id_for_update(&gate, user.id)
        .await
        .expect("user lock should succeed")
        .expect("user should exist");

    let router = DbRouter::single(pool.clone());
    let login = LoginService::new(
        Arc::clone(&router),
        JwtValidator::new("test-secret", "keycompute"),
    );
    let login_email = user.email.clone();
    let login_task = tokio::spawn(async move {
        login
            .login(&LoginRequest {
                email: login_email,
                password: old_password.to_string(),
                client_ip: None,
            })
            .await
    });

    #[derive(Debug, FromQueryResult)]
    struct Waiting {
        waiting: bool,
    }
    let mut login_is_waiting = false;
    for _ in 0..300 {
        let row = pool
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND query LIKE '%FROM users WHERE id = $1 FOR UPDATE%') AS waiting".to_string(),
            ))
            .await
            .expect("login lock wait probe should succeed")
            .expect("login lock wait probe should return a row");
        if Waiting::from_query_result(&row, "")
            .expect("waiting flag should decode")
            .waiting
        {
            login_is_waiting = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let reset_service = PasswordResetService::new(Arc::clone(&router));
    let reset_token = token.clone();
    let reset_task = tokio::spawn(async move {
        reset_service
            .reset_password(&ResetPasswordRequest {
                token: reset_token,
                new_password: "NewPassword123!".to_string(),
            })
            .await
    });

    // Wait until reset has either queued its user lock (the fixed path) or
    // already locked the credential (the historical path).  NOWAIT makes the
    // credential probe deterministic and bounded.
    let mut reset_user_waiter_seen = false;
    let mut credential_locked_before_user = false;
    for _ in 0..300 {
        let row = pool
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT COUNT(*) AS waiting FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND query LIKE '%FROM users WHERE id = $1 FOR UPDATE%'".to_string(),
            ))
            .await
            .expect("reset lock wait probe should succeed")
            .expect("reset lock wait probe should return a row");
        let waiting = row
            .try_get_by_index::<i64>(0)
            .expect("waiting count should decode");

        let probe = pool.begin().await.expect("credential probe should begin");
        let credential_probe = probe
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM user_credentials WHERE user_id = $1 FOR UPDATE NOWAIT",
                [user.id.into()],
            ))
            .await;
        let _ = probe.rollback().await;

        if credential_probe.is_err() {
            credential_locked_before_user = true;
            break;
        }
        if waiting >= 2 {
            reset_user_waiter_seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    gate.commit().await.expect("gate transaction should commit");

    let login_result = tokio::time::timeout(Duration::from_secs(10), login_task)
        .await
        .expect("login should finish after the user lock is released")
        .expect("login task should not panic");
    let reset_result = tokio::time::timeout(Duration::from_secs(10), reset_task)
        .await
        .expect("password reset should finish after the user lock is released")
        .expect("password reset task should not panic");

    cleanup_test_data(&pool, &test_id).await.ok();

    assert!(login_is_waiting, "login should queue on the held user lock");
    assert!(
        reset_user_waiter_seen || credential_locked_before_user,
        "reset should reach its user/credential lock sequence while the gate is held"
    );
    assert!(
        !credential_locked_before_user,
        "password reset must not lock the credential before acquiring the user lock"
    );
    assert!(
        reset_result.is_ok(),
        "password reset should commit without a lock-order/deadlock error: {reset_result:?}"
    );
    if let Err(error) = &login_result {
        assert!(
            !matches!(error, keycompute_types::KeyComputeError::DatabaseError(message) if message.to_ascii_lowercase().contains("deadlock")),
            "login must not fail with a database deadlock: {error}"
        );
    }
}

/// refresh_token 路径回归：token_version 递增后，旧 token 必须被拒绝刷新。
///
/// refresh-token 为公开路由、不经过 `AuthService::verify_token`，其 token_version
/// 比对读取必须走主库（`write_conn`），否则读副本复制延迟会形成绕过窗口——
/// 攻击者可用密码重置/登出后本应失效的旧 token 刷新出新的有效 token。
#[tokio::test]
async fn test_refresh_token_rejected_after_token_version_bump() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("cleanup should succeed");

    let tenant = create_test_tenant(&pool, "tv-refresh", &test_id).await;
    let user = User::create(
        &pool,
        &CreateUserRequest {
            tenant_id: tenant.id,
            email: format!("tv-refresh-{}@example.com", test_id),
            name: Some("TV Refresh User".to_string()),
            role: Some(UserRole::User),
        },
    )
    .await
    .expect("user should be created");

    // 创建凭证并标记邮箱已验证——refresh 流程要求凭证存在、未锁定、邮箱已验证。
    let credential = UserCredential::create(
        &pool,
        &CreateUserCredentialRequest {
            user_id: user.id,
            password_hash: "dummy-argon2-hash".to_string(),
        },
    )
    .await
    .expect("credential should be created");
    credential
        .update(
            &pool,
            &UpdateUserCredentialRequest {
                email_verified: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("credential email_verified should be set");

    let router = DbRouter::single(pool.clone());
    let jwt = JwtValidator::new("test-secret", "keycompute");
    let login = LoginService::new(Arc::clone(&router), jwt.clone());

    // 用用户当前 token_version(=0) 签发 token
    let token = jwt
        .generate_token_with_version(user.id, user.tenant_id, &user.role, user.token_version)
        .expect("token should be generated");

    // 1. 递增前：refresh 成功，返回新 token
    let refreshed = login
        .refresh_token(&token)
        .await
        .expect("refresh should succeed before version bump");
    assert_eq!(refreshed.user_id, user.id);

    // 2. 递增 token_version（模拟密码重置/登出）
    let new_version = User::increment_token_version(&pool, user.id)
        .await
        .expect("increment should succeed");
    assert_eq!(new_version, 1);

    // 3. 递增后：旧 token 必须被拒绝刷新（关键回归断言）
    let result = login.refresh_token(&token).await;
    cleanup_test_data(&pool, &test_id).await.ok();

    let err = result.expect_err("stale token must be rejected on refresh after version bump");
    assert!(
        err.to_string().contains("invalidated"),
        "错误信息应表明 token 已失效，实际: {err}"
    );
}
