//! Credential races retained across the global-identity tenancy cutover.
use chrono::{Duration as ChronoDuration, Utc};
use integration_tests::{
    common::generate_test_id,
    db::{cleanup_test_data, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_auth::{
    JwtValidator, LoginRequest, LoginService, PasswordHasher, PasswordResetService,
    ResetPasswordRequest,
};
use keycompute_db::{
    CreatePasswordResetRequest, CreateUserCredentialRequest, DbRouter, PasswordReset,
    UpdateUserCredentialRequest, User, UserCredential,
};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn test_login_rejects_password_hash_changed_during_verification() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("cleanup should succeed");

    let tenant = create_test_tenant(&pool, "tv-login-race", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "credential-regression", &test_id).await;
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
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND datname=current_database() AND replace(query,' ','') LIKE '%FROMusersWHEREid=$1FORUPDATE%') AS waiting".to_string(),
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
    cleanup_test_data(&pool, &test_id).await.unwrap();
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
    let user = create_test_user(&pool, tenant.id, "credential-regression", &test_id).await;

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
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND datname=current_database() AND replace(query,' ','') LIKE '%FROMusersWHEREid=$1FORUPDATE%') AS waiting".to_string(),
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
                "SELECT COUNT(*) AS waiting FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND datname=current_database() AND replace(query,' ','') LIKE '%FROMusersWHEREid=$1FORUPDATE%'".to_string(),
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

    cleanup_test_data(&pool, &test_id).await.unwrap();

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

#[tokio::test]
async fn test_refresh_token_rejected_after_token_version_bump() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("cleanup should succeed");

    let tenant = create_test_tenant(&pool, "tv-refresh", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "credential-regression", &test_id).await;

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
        .generate_identity_token(user.id, None, user.token_version, None, None, 3600)
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
    cleanup_test_data(&pool, &test_id).await.unwrap();

    let err = result.expect_err("stale token must be rejected on refresh after version bump");
    assert!(
        matches!(err, keycompute_types::KeyComputeError::AuthError(_)),
        "错误信息应表明 token 已失效，实际: {err}"
    );
}
