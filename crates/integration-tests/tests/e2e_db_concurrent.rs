//! 并发操作测试

use integration_tests::common::VerificationChain;
use integration_tests::common::generate_test_id;
use integration_tests::db::{cleanup_test_data, create_test_pool, create_test_tenant};
use keycompute_db::{CreateUserRequest, DbRouter, User};
use keycompute_types::UserRole;
use std::sync::Arc;
use tokio::sync::Barrier;
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_concurrent_operations() {
        let mut chain = VerificationChain::new();

        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("test_tenant_isolation cleanup should succeed");

        // 1. 创建租户
        let tenant = create_test_tenant(&pool, "concurrent", &test_id).await;
        let pool = DbRouter::single(pool);
        let tenant_id = tenant.id;

        // 2. 并发创建用户
        let barrier = Arc::new(Barrier::new(10));
        let mut handles = Vec::new();

        for i in 0..10 {
            let pool_clone = Arc::clone(&pool);
            let barrier_clone = Arc::clone(&barrier);

            handles.push(tokio::spawn(async move {
                barrier_clone.wait().await;

                let email = format!("concurrent-{}-{}@example.com", i, Uuid::new_v4().simple());
                User::create(
                    pool_clone.as_ref(),
                    &CreateUserRequest {
                        tenant_id,
                        email,
                        name: Some(format!("Concurrent User {}", i)),
                        role: Some(UserRole::User),
                    },
                )
                .await
            }));
        }

        // 3. 等待所有操作完成
        let results: Vec<_> = futures::future::join_all(handles).await;

        // 收集错误信息用于调试
        let mut errors = Vec::new();
        for (i, r) in results.iter().enumerate() {
            match r {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => errors.push(format!("Task {} error: {}", i, e)),
                Err(e) => errors.push(format!("Task {} panicked: {}", i, e)),
            }
        }
        if !errors.is_empty() {
            eprintln!("Concurrent user creation errors:\n{}", errors.join("\n"));
        }

        let success_count = results
            .iter()
            .filter(|r| r.is_ok() && r.as_ref().unwrap().is_ok())
            .count();
        chain.add_step(
            "keycompute-db",
            "concurrent_user_creation",
            format!(
                "Created {} users concurrently (10 attempted)",
                success_count
            ),
            success_count == 10,
        );

        // 4. 验证所有用户存在
        let all_users = User::find_by_tenant(pool.as_ref(), tenant_id).await;
        chain.add_step(
            "keycompute-db",
            "verify_concurrent_users",
            format!(
                "Found {} users in tenant",
                all_users.as_ref().map(|v| v.len()).unwrap_or(0)
            ),
            all_users.map(|v| v.len() == 10).unwrap_or(false),
        );

        chain.print_report();
        assert!(chain.all_passed(), "Concurrent operations tests failed");
    }
}
