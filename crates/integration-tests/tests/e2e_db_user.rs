//! 用户 CRUD 及角色约束测试

use integration_tests::common::{VerificationChain, generate_test_id};
use integration_tests::db::{
    cleanup_test_data, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_db::User;
use keycompute_types::AssignableUserRole;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;

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
    #[tokio::test]
    async fn test_system_role_unique_index_rejects_duplicate_system_user() {
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
    #[tokio::test]
    async fn test_system_role_change_trigger_rejects_downgrade() {
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
    #[tokio::test]
    async fn test_system_user_delete_trigger_rejects_direct_delete() {
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
}
