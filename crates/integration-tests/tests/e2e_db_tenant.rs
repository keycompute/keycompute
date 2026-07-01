//! 租户 CRUD 测试

use integration_tests::common::VerificationChain;
use integration_tests::common::generate_test_id;
use integration_tests::db::{cleanup_test_data, create_test_pool, create_test_tenant};
use keycompute_db::Tenant;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_tenant_crud() {
        let mut chain = VerificationChain::new();

        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("test_tenant_crud cleanup should succeed");

        // 1. 创建租户
        let tenant = create_test_tenant(&pool, "crud", &test_id).await;
        chain.add_step(
            "keycompute-db",
            "Tenant::create",
            format!("Tenant created: {} ({})", tenant.name, tenant.id),
            !tenant.id.is_nil() && tenant.status == "active",
        );

        // 2. 查找租户 (by ID)
        let found = Tenant::find_by_id(&pool, tenant.id).await;
        chain.add_step(
            "keycompute-db",
            "Tenant::find_by_id",
            "Tenant found by ID",
            found.is_ok() && found.as_ref().unwrap().is_some(),
        );

        // 3. 查找租户 (by slug)
        let found_by_slug = Tenant::find_by_slug(&pool, &tenant.slug).await;
        chain.add_step(
            "keycompute-db",
            "Tenant::find_by_slug",
            format!(
                "Tenant found by slug: {:?}",
                found_by_slug
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .map(|t| t.name.clone())
            ),
            found_by_slug.is_ok() && found_by_slug.as_ref().unwrap().is_some(),
        );

        // 4. 更新租户
        let update_req = keycompute_db::UpdateTenantRequest {
            name: Some("Updated Test Tenant".to_string()),
            description: Some("Updated description".to_string()),
            status: None,
            default_rpm_limit: Some(200),
            default_tpm_limit: Some(100000),
        };
        let updated = tenant.update(&pool, &update_req).await;
        chain.add_step(
            "keycompute-db",
            "Tenant::update",
            format!(
                "Tenant updated: {:?}",
                updated.as_ref().map(|t| t.name.clone())
            ),
            updated.is_ok() && updated.as_ref().unwrap().name == "Updated Test Tenant",
        );

        // 5. 验证更新
        if let Ok(Some(t)) = Tenant::find_by_id(&pool, tenant.id).await {
            chain.add_step(
                "keycompute-db",
                "verify_update",
                format!("RPM: {}, TPM: {}", t.default_rpm_limit, t.default_tpm_limit),
                t.default_rpm_limit == 200 && t.default_tpm_limit == 100000,
            );
        }

        // 6. 查找所有租户
        let all = Tenant::find_all(&pool).await;
        chain.add_step(
            "keycompute-db",
            "Tenant::find_all",
            format!(
                "Found {} tenants",
                all.as_ref().map(|v| v.len()).unwrap_or(0)
            ),
            all.is_ok(),
        );

        // 7. 删除租户
        let delete_result = tenant.delete(&pool).await;
        chain.add_step(
            "keycompute-db",
            "Tenant::delete",
            "Tenant deleted",
            delete_result.is_ok(),
        );

        // 8. 验证删除
        let after_delete = Tenant::find_by_id(&pool, tenant.id).await;
        chain.add_step(
            "keycompute-db",
            "verify_delete",
            "Tenant no longer exists",
            after_delete.map(|t| t.is_none()).unwrap_or(false),
        );

        chain.print_report();
        assert!(chain.all_passed(), "Tenant CRUD tests failed");
    }

    // ============================================================================
    // 用户 CRUD 测试
    // ============================================================================
}
