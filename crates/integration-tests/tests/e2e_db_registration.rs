//! 注册流程测试

use chrono::{Duration as ChronoDuration, Utc};
use integration_tests::common::generate_test_id;
use integration_tests::db::{
    cleanup_test_data, create_test_pending_registration, create_test_pool, create_test_tenant,
    create_test_user, delete_user_by_email,
};
use keycompute_auth::password::{
    PasswordHasher, RegistrationService, RequestRegistrationCodeRequest,
};
use keycompute_db::{DbRouter, PendingRegistration, UpsertPendingRegistrationRequest};
use keycompute_types::KeyComputeError;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Barrier;

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试首次请求验证码失败时，pending 记录仍会保留并进入冷却
    #[tokio::test]
    async fn test_registration_request_failure_keeps_pending_and_consumes_cooldown() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("registration failure cleanup should succeed");

        let email = format!("registration-placeholder-{}@example.com", test_id);
        let service = RegistrationService::new(DbRouter::single(pool.clone()));

        let err = service
            .request_registration_code(
                &RequestRegistrationCodeRequest {
                    email: email.clone(),
                    referral_code: None,
                },
                Some("127.0.0.1".to_string()),
            )
            .await
            .expect_err("missing email service should fail");

        assert!(matches!(err, KeyComputeError::ServiceUnavailable(_)));

        let pending = PendingRegistration::find_by_email(&pool, &email)
            .await
            .expect("pending query should succeed")
            .expect("pending should be kept after failed send attempt");

        assert!(
            !pending.is_expired(),
            "failed send attempt should still record a fresh verification code window"
        );
        assert_eq!(pending.resend_count, 1);
        assert_eq!(pending.verify_attempts, 0);
        assert_eq!(pending.requested_from_ip.as_deref(), Some("127.0.0.1"));

        let retry_err = service
            .request_registration_code(
                &RequestRegistrationCodeRequest {
                    email: email.clone(),
                    referral_code: None,
                },
                Some("127.0.0.1".to_string()),
            )
            .await
            .expect_err("second request during cooldown should be rejected");

        assert!(matches!(retry_err, KeyComputeError::RateLimitExceeded(_)));
    }

    /// 测试已有 pending 时，请求验证码失败会刷新字段并重新进入冷却
    #[tokio::test]
    async fn test_registration_request_failure_refreshes_existing_pending_fields() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("existing pending cleanup should succeed");

        let email = format!("registration-existing-pending-{}@example.com", test_id);
        let initial_expires_at = Utc::now() + ChronoDuration::minutes(5);
        let initial_last_sent_at = Utc::now() - ChronoDuration::seconds(300);

        let initial_pending = create_test_pending_registration(
            &pool,
            UpsertPendingRegistrationRequest {
                email: email.clone(),
                referral_code: None,
                verification_code_hash: "existing-code-hash".to_string(),
                expires_at: initial_expires_at,
                requested_from_ip: Some("10.0.0.1".to_string()),
                resend_count: 3,
                last_sent_at: initial_last_sent_at,
            },
        )
        .await;

        let service = RegistrationService::new(DbRouter::single(pool.clone()));
        let err = service
            .request_registration_code(
                &RequestRegistrationCodeRequest {
                    email: email.clone(),
                    referral_code: None,
                },
                Some("127.0.0.1".to_string()),
            )
            .await
            .expect_err("missing email service should fail");

        assert!(matches!(err, KeyComputeError::ServiceUnavailable(_)));

        let pending = PendingRegistration::find_by_email(&pool, &email)
            .await
            .expect("pending query should succeed")
            .expect("existing pending should remain");

        assert_eq!(pending.id, initial_pending.id);
        assert_ne!(
            pending.verification_code_hash,
            initial_pending.verification_code_hash
        );
        assert!(pending.expires_at > initial_pending.expires_at);
        assert_eq!(pending.verify_attempts, 0);
        assert_eq!(pending.resend_count, initial_pending.resend_count + 1);
        assert!(pending.last_sent_at > initial_pending.last_sent_at);
        assert_eq!(pending.requested_from_ip.as_deref(), Some("127.0.0.1"));
    }

    /// 测试 default_user_quota 小于等于 0 时，不会赠送初始额度
    #[tokio::test]
    async fn test_complete_registration_skips_initial_balance_when_default_quota_not_positive() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("registration quota cleanup should succeed");

        let email = format!("registration-no-quota-{}@example.com", test_id);
        delete_user_by_email(&pool, &email)
            .await
            .expect("existing test user should be removed");

        let hasher = PasswordHasher::new();
        let code = "123456";
        let code_hash = hasher.hash(code).expect("code hash should succeed");

        create_test_pending_registration(
            &pool,
            UpsertPendingRegistrationRequest {
                email: email.clone(),
                referral_code: None,
                verification_code_hash: code_hash.clone(),
                expires_at: Utc::now() + ChronoDuration::minutes(10),
                requested_from_ip: Some("127.0.0.1".to_string()),
                resend_count: 1,
                last_sent_at: Utc::now() - ChronoDuration::seconds(300),
            },
        )
        .await;

        let service = RegistrationService::new(DbRouter::single(pool.clone()));
        let response = service
            .complete_registration(
                &keycompute_auth::CompleteRegistrationRequest {
                    email: email.clone(),
                    code: code.to_string(),
                    password: "StrongPassword123!".to_string(),
                    name: Some("No Quota User".to_string()),
                },
                0.0,
            )
            .await
            .expect("registration should succeed without initial quota");

        let balance_count: i64 = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) FROM user_balances WHERE user_id = $1",
                [response.user_id.into()],
            ))
            .await
            .expect("balance count query should succeed")
            .and_then(|r| r.try_get_by_index::<i64>(0).ok())
            .unwrap_or(0);
        assert_eq!(balance_count, 0);

        let transaction_count: i64 = pool.query_one(Statement::from_sql_and_values(DbBackend::Postgres, "SELECT COUNT(*) FROM balance_transactions WHERE user_id = $1 AND description = 'Initial quota from system'", [response.user_id.into()]))
            .await
            .expect("transaction count query should succeed")
            .and_then(|r| r.try_get_by_index::<i64>(0).ok())
            .unwrap_or(0);
        assert_eq!(transaction_count, 0);

        delete_user_by_email(&pool, &email)
            .await
            .expect("created test user should be removed");
    }

    /// 测试同邮箱 pending 记录创建时会拒绝重复邮箱
    #[tokio::test]
    async fn test_pending_registration_create_rejects_duplicate_email() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("duplicate pending cleanup should succeed");

        let email = format!("registration-duplicate-pending-{}@example.com", test_id);

        let _first_pending = create_test_pending_registration(
            &pool,
            UpsertPendingRegistrationRequest {
                email: email.clone(),
                referral_code: None,
                verification_code_hash: "first-code-hash".to_string(),
                expires_at: Utc::now() + ChronoDuration::minutes(10),
                requested_from_ip: Some("10.0.0.1".to_string()),
                resend_count: 1,
                last_sent_at: Utc::now() - ChronoDuration::seconds(120),
            },
        )
        .await;

        let tx = pool.begin().await.expect("transaction should start");
        let err = PendingRegistration::create_in_tx(
            &tx,
            &UpsertPendingRegistrationRequest {
                email: email.clone(),
                referral_code: None,
                verification_code_hash: "second-code-hash".to_string(),
                expires_at: Utc::now() + ChronoDuration::minutes(10),
                requested_from_ip: Some("10.0.0.2".to_string()),
                resend_count: 1,
                last_sent_at: Utc::now(),
            },
        )
        .await
        .expect_err("duplicate email should be rejected");

        assert!(matches!(err, keycompute_db::DbError::DuplicateKey { .. }));

        tx.rollback()
            .await
            .expect("duplicate pending rollback should succeed");
    }

    /// 测试首码锁定后，后续无效推荐码会被直接忽略
    #[tokio::test]
    async fn test_registration_request_ignores_invalid_referral_after_first_touch_locked() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("locked referral cleanup should succeed");

        let tenant = create_test_tenant(&pool, "locked-referral", &test_id).await;
        let referrer = create_test_user(&pool, tenant.id, "locked-referrer", &test_id).await;
        let email = format!("registration-locked-referral-{}@example.com", test_id);

        let initial_pending = create_test_pending_registration(
            &pool,
            UpsertPendingRegistrationRequest {
                email: email.clone(),
                referral_code: Some(referrer.id),
                verification_code_hash: "existing-code-hash".to_string(),
                expires_at: Utc::now() + ChronoDuration::minutes(5),
                requested_from_ip: Some("10.0.0.1".to_string()),
                resend_count: 3,
                last_sent_at: Utc::now() - ChronoDuration::seconds(300),
            },
        )
        .await;

        let service = RegistrationService::new(DbRouter::single(pool.clone()));
        let err = service
            .request_registration_code(
                &RequestRegistrationCodeRequest {
                    email: email.clone(),
                    referral_code: Some("not-a-valid-referral".to_string()),
                },
                Some("127.0.0.1".to_string()),
            )
            .await
            .expect_err("missing email service should still be the only failure");

        assert!(matches!(err, KeyComputeError::ServiceUnavailable(_)));

        let pending = PendingRegistration::find_by_email(&pool, &email)
            .await
            .expect("pending query should succeed")
            .expect("locked referral pending should remain");

        assert_eq!(pending.id, initial_pending.id);
        assert_eq!(pending.referral_code, Some(referrer.id));
    }

    /// 测试同邮箱并发刷新验证码时，只允许一个事务成功刷新 pending
    #[tokio::test]
    async fn test_pending_registration_refresh_is_serialized_per_email() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("serialized refresh cleanup should succeed");

        let email = format!("registration-concurrent-{}@example.com", test_id);
        let initial_last_sent_at = Utc::now() - ChronoDuration::seconds(300);

        let initial_pending = create_test_pending_registration(
            &pool,
            UpsertPendingRegistrationRequest {
                email: email.clone(),
                referral_code: None,
                verification_code_hash: "initial-code-hash".to_string(),
                expires_at: Utc::now() + ChronoDuration::minutes(10),
                requested_from_ip: Some("10.0.0.1".to_string()),
                resend_count: 1,
                last_sent_at: initial_last_sent_at,
            },
        )
        .await;

        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();

        for worker in 0..2 {
            let pool = pool.clone();
            let barrier = Arc::clone(&barrier);
            let email = email.clone();

            handles.push(tokio::spawn(async move {
                let tx = pool.begin().await.expect("transaction should start");
                barrier.wait().await;

                PendingRegistration::lock_email_slot(&tx, &email)
                    .await
                    .expect("email slot lock should succeed");

                let pending = PendingRegistration::find_by_email_for_update(&tx, &email)
                    .await
                    .expect("pending lookup should succeed")
                    .expect("pending should exist");

                let elapsed = (Utc::now() - pending.last_sent_at).num_seconds();
                if !pending.is_expired() && elapsed < 60 {
                    tx.rollback()
                        .await
                        .expect("cooldown transaction rollback should succeed");
                    return false;
                }

                tokio::time::sleep(Duration::from_millis(200)).await;

                pending
                    .refresh_code_in_tx(
                        &tx,
                        &UpsertPendingRegistrationRequest {
                            email,
                            referral_code: None,
                            verification_code_hash: format!("refreshed-hash-{worker}"),
                            expires_at: Utc::now() + ChronoDuration::minutes(10),
                            requested_from_ip: Some(format!("10.0.0.{}", worker + 2)),
                            resend_count: 1,
                            last_sent_at: Utc::now(),
                        },
                    )
                    .await
                    .expect("pending refresh should succeed");

                tx.commit()
                    .await
                    .expect("refresh transaction should commit");

                true
            }));
        }

        let mut refresh_count = 0;
        let mut blocked_count = 0;
        for handle in handles {
            if handle.await.expect("task should join") {
                refresh_count += 1;
            } else {
                blocked_count += 1;
            }
        }

        let final_pending = PendingRegistration::find_by_email(&pool, &email)
            .await
            .expect("final pending lookup should succeed")
            .expect("pending should still exist");

        assert_eq!(
            refresh_count, 1,
            "exactly one concurrent request should refresh pending"
        );
        assert_eq!(
            blocked_count, 1,
            "exactly one concurrent request should hit cooldown"
        );
        assert_eq!(final_pending.id, initial_pending.id);
        assert_eq!(
            final_pending.resend_count, 2,
            "resend count should increase only once"
        );
        assert!(
            final_pending.last_sent_at > initial_pending.last_sent_at,
            "last_sent_at should be refreshed exactly once"
        );
        assert!(
            matches!(
                final_pending.verification_code_hash.as_str(),
                "refreshed-hash-0" | "refreshed-hash-1"
            ),
            "final code hash should come from the winning refresh"
        );
        assert!(
            matches!(
                final_pending.requested_from_ip.as_deref(),
                Some("10.0.0.2") | Some("10.0.0.3")
            ),
            "final IP should belong to the winning request"
        );
    }

    // ============================================================================
    // 事务测试
    // ============================================================================
}
