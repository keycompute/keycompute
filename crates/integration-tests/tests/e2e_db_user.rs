//! 用户 CRUD 及角色约束测试

use integration_tests::common::{VerificationChain, generate_test_id};
use integration_tests::db::{
    cleanup_test_data, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_db::{
    CreatePaymentOrderRequest, CreateProduceAiKeyRequest, PaymentMethod, PaymentOrder,
    ProduceAiKey, Tenant, UpdateUserRequest, User, UserBalance,
};
use keycompute_types::AssignableUserRole;
use once_cell::sync::Lazy;
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use std::time::Duration;
use tokio::sync::Mutex;
use uuid::Uuid;

/// 全局互斥锁，序列化 `role = 'system'` 用户的相关测试。
///
/// `uq_users_single_system_role` 部分唯一索引确保整个数据库至多存在
/// 一个 system 用户。多个测试并发插入 role='system' 的记录时会发生冲突
///（PostgreSQL 唯一索引锁竞争导致 `duplicate key` 错误）。
///
/// 持有此锁的测试独占 system 用户创建/修改权限，完成后释放。
static SYSTEM_USER_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json,
        extract::{Path, State},
    };
    use keycompute_auth::Permission;
    use keycompute_server::{AppState, AuthExtractor};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_user_crud() {
        let mut chain = VerificationChain::new();

        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("test_api_key_crud cleanup should succeed");

        // 1. 创建租户和用户
        let tenant = create_test_tenant(&pool, "user-crud", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "user-crud", &test_id).await;

        chain.add_step(
            "keycompute-db",
            "User::create",
            format!("User created: {} ({})", user.email, user.id),
            !user.id.is_nil() && user.tenant_id == tenant.id,
        );

        // 2. 查找用户 (by ID)
        let found = User::find_by_id(&pool, user.id).await;
        chain.add_step(
            "keycompute-db",
            "User::find_by_id",
            "User found by ID",
            found.is_ok() && found.as_ref().unwrap().is_some(),
        );

        // 3. 查找用户 (by email)
        let found_by_email = User::find_by_email(&pool, &user.email).await;
        chain.add_step(
            "keycompute-db",
            "User::find_by_email",
            format!(
                "User found by email: {:?}",
                found_by_email
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .map(|u| u.email.clone())
            ),
            found_by_email.is_ok() && found_by_email.as_ref().unwrap().is_some(),
        );

        // 4. 查找租户下的用户
        let tenant_users = User::find_by_tenant(&pool, tenant.id).await;
        chain.add_step(
            "keycompute-db",
            "User::find_by_tenant",
            format!(
                "Found {} users in tenant",
                tenant_users.as_ref().map(|v| v.len()).unwrap_or(0)
            ),
            tenant_users.is_ok() && tenant_users.as_ref().unwrap().len() == 1,
        );

        // 5. 更新用户
        let update_req = keycompute_db::UpdateUserRequest {
            name: Some("Updated User Name".to_string()),
            role: Some(AssignableUserRole::Admin),
            tenant_id: None,
        };
        let updated = user.update(&pool, &update_req).await;
        chain.add_step(
            "keycompute-db",
            "User::update",
            format!(
                "User updated: {:?}",
                updated.as_ref().map(|u| u.name.clone())
            ),
            updated.is_ok()
                && updated.as_ref().unwrap().name == Some("Updated User Name".to_string()),
        );

        // 6. 删除用户
        let delete_result = user.delete(&pool).await;
        chain.add_step(
            "keycompute-db",
            "User::delete",
            "User deleted",
            delete_result.is_ok(),
        );

        chain.print_report();
        assert!(chain.all_passed(), "User CRUD tests failed");
    }

    #[tokio::test]
    async fn test_user_tenant_reassignment_moves_balance_and_revokes_keys() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cleanup should succeed");
        let source = create_test_tenant(&pool, "user-move-source", &test_id).await;
        let target = create_test_tenant(&pool, "user-move-target", &test_id).await;
        let user = create_test_user(&pool, source.id, "user-move", &test_id).await;

        let balance = UserBalance::get_or_create(&pool, source.id, user.id)
            .await
            .expect("balance should be created");
        assert_eq!(balance.tenant_id, source.id);
        let key = ProduceAiKey::create(
            &pool,
            &CreateProduceAiKeyRequest {
                tenant_id: source.id,
                user_id: user.id,
                name: "move-test".to_string(),
                produce_ai_key_hash: format!("move-key-{test_id}"),
                produce_ai_key_preview: "move-key".to_string(),
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
        UserBalance::reassign_tenant(&tx, user.id, target.id)
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
                    tenant_id: Some(target.id),
                },
            )
            .await
            .expect("user should move");
        tx.commit().await.expect("transaction should commit");

        assert_eq!(moved.tenant_id, target.id);
        assert_eq!(moved.token_version, user.token_version + 1);
        assert_eq!(
            UserBalance::find_by_user(&pool, user.id)
                .await
                .unwrap()
                .unwrap()
                .tenant_id,
            target.id
        );
        assert!(
            ProduceAiKey::find_by_id(&pool, key.id)
                .await
                .unwrap()
                .unwrap()
                .revoked
        );
        cleanup_test_data(&pool, &test_id).await.ok();
    }

    /// The reassignment transaction holds the user row while it locks pending
    /// orders. PostgreSQL foreign-key checks in payment/balance writers take
    /// a KEY SHARE lock on that user, so the reassignment lock must remain
    /// compatible with it. This reproduces the reverse order (order -> user)
    /// and fails with a deadlock when the move uses FOR UPDATE.
    #[tokio::test]
    async fn test_tenant_reassignment_does_not_deadlock_with_payment_fk_writer() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cleanup should succeed");
        let source = create_test_tenant(&pool, "user-lock-source", &test_id).await;
        let target = create_test_tenant(&pool, "user-lock-target", &test_id).await;
        let user = create_test_user(&pool, source.id, "user-lock", &test_id).await;
        UserBalance::get_or_create(&pool, source.id, user.id)
            .await
            .expect("balance should be created");
        let order = PaymentOrder::create(
            &pool,
            &CreatePaymentOrderRequest {
                tenant_id: source.id,
                user_id: user.id,
                amount: Decimal::ONE,
                subject: "lock-order test".to_string(),
                body: None,
                payment_method: PaymentMethod::WechatPay,
                payment_scene: "native".to_string(),
                expired_at: chrono::Utc::now() + chrono::Duration::minutes(30),
            },
            &format!("TESTLOCK{}", test_id.replace('-', "")),
            "",
        )
        .await
        .expect("payment order should be created");

        // Simulate a callback after it has acquired the order lock.
        let callback_tx = pool
            .begin()
            .await
            .expect("callback transaction should begin");
        callback_tx
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM payment_orders WHERE id = $1 FOR UPDATE",
                [order.id.into()],
            ))
            .await
            .expect("callback should lock the order");

        // The move owns the user lock and then waits for the callback's order
        // lock. The callback's FK insert below must still be able to finish.
        let move_tx = pool.begin().await.expect("move transaction should begin");
        User::find_by_id_for_no_key_update(&move_tx, user.id)
            .await
            .expect("move should lock the user")
            .expect("user should exist");
        let move_task = tokio::spawn(async move {
            let result =
                PaymentOrder::reassign_pending_for_user(&move_tx, user.id, source.id, target.id)
                    .await;
            let _ = move_tx.rollback().await;
            result
        });

        #[derive(Debug, FromQueryResult)]
        struct Waiting {
            waiting: bool,
        }
        let mut move_is_waiting = false;
        for _ in 0..200 {
            let row = pool
                .query_one(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND query LIKE '%FROM payment_orders WHERE user_id = $1%') AS waiting".to_string(),
                ))
                .await
                .expect("lock wait probe should succeed")
                .expect("lock wait probe should return a row");
            if Waiting::from_query_result(&row, "")
                .expect("waiting flag should decode")
                .waiting
            {
                move_is_waiting = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            move_is_waiting,
            "tenant move should be waiting on the callback's order lock"
        );

        // This INSERT performs the same user FK check as the callback's
        // balance transaction. It must not wait on the move's user lock.
        let callback_insert = tokio::time::timeout(
            Duration::from_secs(3),
            callback_tx.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO balance_transactions (tenant_id, user_id, order_id, transaction_type, amount, balance_before, balance_after, description) VALUES ($1, $2, $3, 'recharge', 0, 0, 0, 'lock-order test')",
                [source.id.into(), user.id.into(), order.id.into()],
            )),
        )
        .await
        .expect("callback FK insert should not deadlock")
        .expect("callback FK insert should succeed");
        assert_eq!(callback_insert.rows_affected(), 1);
        callback_tx
            .rollback()
            .await
            .expect("callback transaction should roll back");

        let moved = tokio::time::timeout(Duration::from_secs(3), move_task)
            .await
            .expect("move should finish after callback releases the order")
            .expect("move task should not panic")
            .expect("move should complete without a deadlock");
        assert_eq!(moved, 1);
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cleanup should succeed");
    }

    /// A user move and an API-key insert both touch the target tenant and the
    /// user row.  The move must acquire the parent tenant lock before waiting
    /// on the user; otherwise the insert can hold a tenant KEY SHARE lock and
    /// wait on the same user, forming a user -> tenant / tenant -> user cycle.
    ///
    /// The user lock is deliberately held by this test to make the ordering
    /// observable: the move should be waiting on the user while already
    /// holding the target tenant, and the key insert should consequently wait
    /// on that tenant.  Releasing the user lock must let both operations
    /// complete without PostgreSQL's deadlock detector aborting either one.
    #[tokio::test]
    async fn test_user_reassignment_does_not_deadlock_with_api_key_create() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cleanup should succeed");
        let source = create_test_tenant(&pool, "move-create-source", &test_id).await;
        let target = create_test_tenant(&pool, "move-create-target", &test_id).await;
        let user = create_test_user(&pool, source.id, "move-create", &test_id).await;

        // Hold the user row so the real admin handler reaches (and holds) its
        // tenant locks before it waits for the user lock.
        let user_gate = pool
            .begin()
            .await
            .expect("user gate transaction should begin");
        User::find_by_id_for_update(&user_gate, user.id)
            .await
            .expect("user gate lookup should succeed")
            .expect("user should exist");

        let router = keycompute_db::DbRouter::single(pool.clone());
        let state = AppState::with_pool(Arc::clone(&router));
        let auth = AuthExtractor::new(Uuid::new_v4(), source.id, Uuid::new_v4(), "system")
            .with_permissions(vec![Permission::SystemAdmin, Permission::ManageTenant]);
        let update_request = keycompute_server::handlers::admin_user::UpdateUserRequest {
            name: None,
            role: None,
            tenant_id: Some(target.id),
        };
        let update_task = tokio::spawn(async move {
            keycompute_server::handlers::admin_user::update_user(
                auth,
                Path(user.id),
                State(state),
                Json(update_request),
            )
            .await
        });

        #[derive(Debug, FromQueryResult)]
        struct Waiting {
            waiting: bool,
        }

        // Wait until the handler has reached its user lock.  A fixed sleep is
        // insufficient here because the test shares a database with other
        // integration cases.
        let mut move_waiting_on_user = false;
        for _ in 0..300 {
            let row = pool
                .query_one(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND query LIKE '%FROM users WHERE id = $1 FOR NO KEY UPDATE%') AS waiting".to_string(),
                ))
                .await
                .expect("user lock wait probe should succeed")
                .expect("user lock wait probe should return a row");
            if Waiting::from_query_result(&row, "")
                .expect("waiting flag should decode")
                .waiting
            {
                move_waiting_on_user = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            move_waiting_on_user,
            "user move should wait on the gate's user lock"
        );

        // The create path uses the same target tenant as the move.  With the
        // canonical tenant-first order it must now wait on the move's
        // FOR UPDATE tenant lock rather than acquire the tenant and wait on
        // the gated user (the old inverse order).
        let create_pool = pool.clone();
        let create_request = CreateProduceAiKeyRequest {
            tenant_id: target.id,
            user_id: user.id,
            name: format!("move-create-key-{test_id}"),
            produce_ai_key_hash: format!("move-create-hash-{test_id}"),
            produce_ai_key_preview: "move-create".to_string(),
            expires_at: None,
        };
        let create_task =
            tokio::spawn(async move { ProduceAiKey::create(&create_pool, &create_request).await });

        let mut create_waiting_on_target = false;
        for _ in 0..300 {
            let row = pool
                .query_one(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND query LIKE '%FROM tenants WHERE id = $1 FOR KEY SHARE%') AS waiting".to_string(),
                ))
                .await
                .expect("tenant lock wait probe should succeed")
                .expect("tenant lock wait probe should return a row");
            if Waiting::from_query_result(&row, "")
                .expect("waiting flag should decode")
                .waiting
            {
                create_waiting_on_target = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            create_waiting_on_target,
            "API-key creation should wait on the move's target tenant lock"
        );

        user_gate
            .rollback()
            .await
            .expect("user gate transaction should roll back");

        let update_result = tokio::time::timeout(Duration::from_secs(5), update_task)
            .await
            .expect("user move should finish after releasing the gate")
            .expect("user move task should not panic")
            .expect("user move should succeed");
        assert_eq!(update_result.0["tenant_id"], serde_json::json!(target.id));

        let created_key = tokio::time::timeout(Duration::from_secs(5), create_task)
            .await
            .expect("API-key creation should finish after the move")
            .expect("API-key creation task should not panic")
            .expect("API-key creation should succeed after the move");
        assert_eq!(created_key.tenant_id, target.id);
        assert_eq!(created_key.user_id, user.id);

        let final_user = User::find_by_id(&pool, user.id)
            .await
            .expect("final user lookup should succeed")
            .expect("final user should exist");
        assert_eq!(final_user.tenant_id, target.id);

        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cleanup should succeed");
    }

    /// 测试 users.role 数据库约束
    #[tokio::test]
    async fn test_user_role_constraint_rejects_invalid_role() {
        let pool = create_test_pool().await;
        let run_id = generate_test_id();
        let tenant = create_test_tenant(&pool, "role-constraint", &run_id).await;

        let result = pool
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"
        INSERT INTO users (tenant_id, email, name, role)
        VALUES ($1, $2, $3, $4)
        "#,
                [
                    tenant.id.into(),
                    format!("invalid-role-{}@example.com", run_id).into(),
                    "Invalid Role User".into(),
                    "tenant_admin".into(),
                ],
            ))
            .await;

        cleanup_test_data(&pool, &run_id)
            .await
            .expect("test_user_role_constraint_rejects_invalid_role cleanup should succeed");

        let err = result.expect_err("invalid role insert should be rejected");
        assert!(err.to_string().contains("chk_users_role_allowed"));
    }

    /// 测试 default_user_role 数据库约束
    ///
    /// 在事务内执行并回滚，避免影响其他并行测试。
    #[tokio::test]
    async fn test_default_user_role_setting_constraint_rejects_invalid_role() {
        let pool = create_test_pool().await;
        let tx = pool.begin().await.expect("transaction should start");

        let result = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"
        UPDATE system_settings
        SET value = $1
        WHERE key = 'default_user_role'
        "#,
                ["tenant_admin".into()],
            ))
            .await;

        let err = result.expect_err("invalid default_user_role should be rejected");
        assert!(
            err.to_string()
                .contains("chk_system_settings_default_user_role")
        );

        tx.rollback().await.expect("rollback should succeed");
    }

    /// 测试 system 角色全局唯一约束
    ///
    /// 通过 `SYSTEM_USER_MUTEX` 序列化，避免并发测试同时插入 role='system'。
    #[tokio::test]
    async fn test_system_role_unique_index_rejects_duplicate_system_user() {
        let _guard = SYSTEM_USER_MUTEX.lock().await;
        let pool = create_test_pool().await;
        let run_id = generate_test_id();
        let tx = pool.begin().await.expect("transaction should start");
        let tenant_a_id = Uuid::new_v4();
        let tenant_b_id = Uuid::new_v4();

        let _ = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO tenants (id, name, slug, description) VALUES ($1, $2, $3, $4)",
                [
                    tenant_a_id.into(),
                    "System Unique A".into(),
                    format!("test-tenant-system-unique-a-{}", run_id).into(),
                    "System unique A".into(),
                ],
            ))
            .await
            .expect("tenant A should be inserted");

        let _ = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO tenants (id, name, slug, description) VALUES ($1, $2, $3, $4)",
                [
                    tenant_b_id.into(),
                    "System Unique B".into(),
                    format!("test-tenant-system-unique-b-{}", run_id).into(),
                    "System unique B".into(),
                ],
            ))
            .await
            .expect("tenant B should be inserted");

        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO users (tenant_id, email, name, role) VALUES ($1, $2, $3, $4)",
            [
                tenant_a_id.into(),
                format!("system-a-{}@example.com", run_id).into(),
                "System A".into(),
                "system".into(),
            ],
        ))
        .await
        .expect("first system user should be created");

        let result = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO users (tenant_id, email, name, role) VALUES ($1, $2, $3, $4)",
                [
                    tenant_b_id.into(),
                    format!("system-b-{}@example.com", run_id).into(),
                    "System B".into(),
                    "system".into(),
                ],
            ))
            .await;

        let err = result.expect_err("duplicate system user should be rejected");
        assert!(err.to_string().contains("uq_users_single_system_role"));
        tx.rollback()
            .await
            .expect("transaction rollback should succeed");
    }

    /// 测试禁止将 system 用户降级
    ///
    /// 通过 `SYSTEM_USER_MUTEX` 序列化，避免并发测试同时插入 role='system'。
    #[tokio::test]
    async fn test_system_role_change_trigger_rejects_downgrade() {
        let _guard = SYSTEM_USER_MUTEX.lock().await;
        let pool = create_test_pool().await;
        let run_id = generate_test_id();
        let tx = pool.begin().await.expect("transaction should start");
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();

        let _ = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO tenants (id, name, slug, description) VALUES ($1, $2, $3, $4)",
                [
                    tenant_id.into(),
                    "System Downgrade".into(),
                    format!("test-tenant-system-role-downgrade-{}", run_id).into(),
                    "System role downgrade".into(),
                ],
            ))
            .await
            .expect("tenant should be inserted");

        let _ = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO users (id, tenant_id, email, name, role) VALUES ($1, $2, $3, $4, $5)",
                [
                    user_id.into(),
                    tenant_id.into(),
                    format!("system-downgrade-{}@example.com", run_id).into(),
                    "System Downgrade".into(),
                    "system".into(),
                ],
            ))
            .await
            .expect("system user should be created for downgrade trigger test");

        let result = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE users SET role = 'user' WHERE id = $1",
                [user_id.into()],
            ))
            .await;

        let err = result.expect_err("system role downgrade should be rejected");
        assert!(
            err.to_string()
                .contains("system user role cannot be changed")
        );
        tx.rollback()
            .await
            .expect("transaction rollback should succeed");
    }

    /// 测试禁止通过更新将普通用户提升为 system
    #[tokio::test]
    async fn test_system_role_change_trigger_rejects_promotion() {
        let pool = create_test_pool().await;
        let run_id = generate_test_id();
        let tx = pool.begin().await.expect("transaction should start");
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();

        let _ = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO tenants (id, name, slug, description) VALUES ($1, $2, $3, $4)",
                [
                    tenant_id.into(),
                    "System Promotion".into(),
                    format!("test-tenant-system-role-promotion-{}", run_id).into(),
                    "System role promotion".into(),
                ],
            ))
            .await
            .expect("tenant should be inserted");

        let _ = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO users (id, tenant_id, email, name, role) VALUES ($1, $2, $3, $4, $5)",
                [
                    user_id.into(),
                    tenant_id.into(),
                    format!("user-promotion-{}@example.com", run_id).into(),
                    "Promotion User".into(),
                    "user".into(),
                ],
            ))
            .await
            .expect("user should be created for promotion trigger test");

        let result = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE users SET role = 'system' WHERE id = $1",
                [user_id.into()],
            ))
            .await;

        let err = result.expect_err("promotion to system should be rejected");
        assert!(
            err.to_string()
                .contains("system role cannot be assigned by update")
        );
        tx.rollback()
            .await
            .expect("transaction rollback should succeed");
    }

    /// 测试 system 用户删除触发器
    ///
    /// 通过 `SYSTEM_USER_MUTEX` 序列化，避免并发测试同时插入 role='system'。
    #[tokio::test]
    async fn test_system_user_delete_trigger_rejects_direct_delete() {
        let _guard = SYSTEM_USER_MUTEX.lock().await;
        let pool = create_test_pool().await;
        let run_id = generate_test_id();
        let tx = pool.begin().await.expect("transaction should start");
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();

        let _ = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO tenants (id, name, slug, description) VALUES ($1, $2, $3, $4)",
                [
                    tenant_id.into(),
                    "System Delete Guard".into(),
                    format!("test-tenant-system-delete-guard-{}", run_id).into(),
                    "System delete guard".into(),
                ],
            ))
            .await
            .expect("tenant should be inserted");

        let _ = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO users (id, tenant_id, email, name, role) VALUES ($1, $2, $3, $4, $5)",
                [
                    user_id.into(),
                    tenant_id.into(),
                    format!("system-delete-guard-{}@example.com", run_id).into(),
                    "System Guard".into(),
                    "system".into(),
                ],
            ))
            .await
            .expect("system user should be created for trigger test");

        let result = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM users WHERE id = $1",
                [user_id.into()],
            ))
            .await;

        let err = result.expect_err("system user delete should be rejected");
        assert!(err.to_string().contains("system user cannot be deleted"));
        tx.rollback()
            .await
            .expect("transaction rollback should succeed");
    }

    /// A stale tenant from an old JWT must not materialize a new balance after
    /// the user has been moved. The tip transaction intentionally snapshots
    /// the missing balance first, then waits on the user lock held by the move;
    /// once the move commits, the guarded write must fail with a tenant
    /// mismatch instead of creating a source-tenant balance row.
    #[tokio::test]
    async fn test_tip_credit_rejects_stale_tenant_after_user_move_without_balance() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cleanup should succeed");
        let source = create_test_tenant(&pool, "tip-stale-source", &test_id).await;
        let target = create_test_tenant(&pool, "tip-stale-target", &test_id).await;
        let user = create_test_user(&pool, source.id, "tip-stale", &test_id).await;

        // Hold the move's canonical parent/user locks while the stale tip
        // transaction snapshots the missing balance and reaches its user lock.
        let move_tx = pool.begin().await.expect("move transaction should begin");
        Tenant::find_by_id_for_key_share(&move_tx, source.id)
            .await
            .expect("source tenant lock should succeed")
            .expect("source tenant should exist");
        Tenant::find_by_id_for_update(&move_tx, target.id)
            .await
            .expect("target tenant lock should succeed")
            .expect("target tenant should exist");
        User::find_by_id_for_no_key_update(&move_tx, user.id)
            .await
            .expect("move should lock user")
            .expect("user should exist");

        let tip_tx = pool.begin().await.expect("tip transaction should begin");
        let tip_task = tokio::spawn(async move {
            UserBalance::credit_tips(
                &tip_tx,
                user.id,
                source.id,
                Decimal::ONE,
                Some("stale tip test"),
            )
            .await
        });

        // Ensure the tip transaction has reached the user lock before the
        // move creates its target balance and commits.
        #[derive(Debug, FromQueryResult)]
        struct Waiting {
            waiting: bool,
        }
        let mut tip_is_waiting = false;
        for _ in 0..300 {
            let row = pool
                .query_one(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND query LIKE '%FROM users WHERE id = $1 FOR NO KEY UPDATE%') AS waiting".to_string(),
                ))
                .await
                .expect("tip lock wait probe should succeed")
                .expect("tip lock wait probe should return a row");
            if Waiting::from_query_result(&row, "")
                .expect("waiting flag should decode")
                .waiting
            {
                tip_is_waiting = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            tip_is_waiting,
            "tip credit should wait on the move's user lock"
        );

        UserBalance::reassign_tenant(&move_tx, user.id, target.id)
            .await
            .expect("move should create the target balance");
        let locked_user = User::find_by_id_for_no_key_update(&move_tx, user.id)
            .await
            .expect("moved user lookup should succeed")
            .expect("moved user should exist");
        locked_user
            .update_in_tx(
                &move_tx,
                &UpdateUserRequest {
                    name: None,
                    role: None,
                    tenant_id: Some(target.id),
                },
            )
            .await
            .expect("user tenant should update");
        move_tx
            .commit()
            .await
            .expect("move transaction should commit");

        let tip_result = tokio::time::timeout(Duration::from_secs(3), tip_task)
            .await
            .expect("tip credit should finish after the move commits")
            .expect("tip task should not panic");
        assert!(matches!(
            tip_result,
            Err(keycompute_db::DbError::UserTenantMismatch {
                requested_tenant_id,
                actual_tenant_id,
                ..
            }) if requested_tenant_id == source.id && actual_tenant_id == target.id
        ));
        assert_eq!(
            UserBalance::find_by_user(&pool, user.id)
                .await
                .expect("balance lookup should succeed")
                .expect("move should have materialized a balance")
                .tenant_id,
            target.id
        );
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cleanup should succeed");
    }
}
