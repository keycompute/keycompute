//! Actual platform-pricing SDK -> Axum -> isolated PostgreSQL contracts.
use client_api::{
    AdminApi, ApiClient, ClientConfig, ClientError,
    api::admin::{
        CreatePricingRequest, PricingQueryParams, PricingTarget, SetDefaultPricingRequest,
        UpdatePricingRequest,
    },
};
use integration_tests::{
    common::generate_test_id,
    db::{
        TestDataGuard, create_test_api_key, create_test_pool, create_test_tenant, create_test_user,
    },
};
use keycompute_auth::ProduceAiKeyValidator;
use keycompute_db::{CreateProduceAiKeyRequest, DbRouter, TenantMembership, User};
use keycompute_server::{AppState, create_router};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::json;
use uuid::Uuid;
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn token(
    state: &AppState,
    db: &DatabaseConnection,
    user: Uuid,
    tenant: Option<Uuid>,
) -> String {
    let u = User::find_by_id(db, user).await.unwrap().unwrap();
    let raw = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(user, None, u.token_version, None, None, 3600)
        .unwrap();
    if let Some(t) = tenant {
        let auth = state.auth.verify_token(&raw).await.unwrap();
        state
            .auth
            .select_tenant(&auth, Some(t))
            .await
            .unwrap()
            .access_token
    } else {
        raw
    }
}
#[tokio::test]
async fn platform_pricing_client_obeys_explicit_targets_and_the_actual_server_contract() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let mut cleanup = TestDataGuard::new(db.clone(), run.clone());
    let a = create_test_tenant(&db, "platform-pricing-client-a", &run).await;
    let b = create_test_tenant(&db, "platform-pricing-client-b", &run).await;
    let operator = create_test_user(&db, a.id, "platform-pricing-operator", &run).await;
    let state = AppState::with_pool(DbRouter::single(db.clone()));
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET platform_role='root' WHERE id=$1",
        [a.owner_user_id.into()],
    ))
    .await
    .unwrap();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET platform_role='operator' WHERE id=$1",
        [operator.id.into()],
    ))
    .await
    .unwrap();
    let root_global = token(&state, &db, a.owner_user_id, None).await;
    let root_in_a = token(&state, &db, a.owner_user_id, Some(a.id)).await;
    let operator = token(&state, &db, operator.id, None).await;
    let admin_b = token(&state, &db, b.owner_user_id, Some(b.id)).await;
    assert!(
        TenantMembership::find_any(&db, b.id, a.owner_user_id)
            .await
            .unwrap()
            .is_none()
    );
    let raw_key = ProduceAiKeyValidator::generate_key();
    create_test_api_key(
        &db,
        &CreateProduceAiKeyRequest {
            tenant_id: a.id,
            user_id: a.owner_user_id,
            name: "pricing-only-fixture".into(),
            produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&raw_key),
            produce_ai_key_preview: "test-only".into(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, create_router(state)).await.unwrap();
    }));
    let client =
        ApiClient::new(ClientConfig::new(format!("http://{addr}")).with_no_proxy(true)).unwrap();
    let api = AdminApi::new(&client);
    let model = format!("platform-pricing-contract-{run}");
    let target = PricingTarget::Tenant { tenant_id: b.id };
    let query = |t| PricingQueryParams::new(t).with_search(model.clone());
    for denied in [&operator, &admin_b, &raw_key] {
        assert!(matches!(
            api.list_pricing_page(&query(PricingTarget::Platform), denied)
                .await,
            Err(ClientError::Unauthorized(_) | ClientError::Forbidden(_))
        ));
    }
    // Reproduce the old SDK contract: tenant_id alone is not a create scope.
    let rejected=reqwest::Client::builder().no_proxy().build().unwrap().post(format!("http://{addr}/api/v1/platform/pricing")).bearer_auth(&root_global)
        .json(&json!({"model_name":model,"tenant_id":b.id,"billing_dimension":"provideraccount","input_price_per_1k":"0.0000000001","output_price_per_1k":"1","currency":"CNY"})).send().await.unwrap();
    assert_eq!(rejected.status().as_u16(), 422);
    assert_eq!(
        api.list_pricing_page(&query(target), &root_global)
            .await
            .unwrap()
            .total,
        0
    );
    // Same model has separate global and tenant rows. Selected A never supplies target B implicitly.
    let global = api
        .create_pricing(
            &CreatePricingRequest::new(
                PricingTarget::Platform,
                &model,
                "provideraccount",
                "0.0000000001",
                "1",
                "CNY",
            ),
            &root_in_a,
        )
        .await
        .unwrap();
    let owned = api
        .create_pricing(
            &CreatePricingRequest::new(
                target,
                &model,
                "provideraccount",
                "0.0000000002",
                "2",
                "CNY",
            ),
            &root_global,
        )
        .await
        .unwrap();
    for auth in [&root_global, &root_in_a] {
        let g = api
            .list_pricing_page(&query(PricingTarget::Platform), auth)
            .await
            .unwrap();
        assert_eq!(g.total, 1);
        assert_eq!(g.pricing[0].id, global.pricing_id);
        assert_eq!(g.pricing[0].tenant_id, None);
        let t = api.list_pricing_page(&query(target), auth).await.unwrap();
        assert_eq!(t.total, 1);
        assert_eq!(t.pricing[0].id, owned.pricing_id);
        assert_eq!(t.pricing[0].tenant_id, Some(b.id.to_string()));
    }
    let patch = UpdatePricingRequest::new()
        .with_expected_version(owned.version)
        .with_input_price_per_1k("0.0000000003");
    assert!(
        api.update_pricing(
            PricingTarget::Tenant { tenant_id: a.id },
            &owned.pricing_id,
            &patch,
            &root_in_a
        )
        .await
        .is_err()
    );
    let changed = api
        .update_pricing(target, &owned.pricing_id, &patch, &root_in_a)
        .await
        .unwrap();
    assert!(changed.version > owned.version);
    assert!(
        api.update_pricing(target, &owned.pricing_id, &patch, &root_in_a)
            .await
            .is_err()
    );
    let d = api
        .make_pricing_default(target, &owned.pricing_id, &root_global)
        .await
        .unwrap();
    assert!(d.version >= changed.version);
    let group = api
        .set_default_pricing(
            target,
            &SetDefaultPricingRequest {
                model_ids: vec![owned.pricing_id.clone()],
            },
            &root_global,
        )
        .await
        .unwrap();
    assert_eq!(group.pricing_ids, vec![owned.pricing_id.clone()]);
    assert!(
        api.delete_pricing(PricingTarget::Platform, &global.pricing_id, &root_global)
            .await
            .is_err()
    );
    let row = api
        .list_pricing_page(&query(target), &root_global)
        .await
        .unwrap()
        .pricing
        .remove(0);
    assert_eq!(
        row.input_price_per_1k
            .parse::<bigdecimal::BigDecimal>()
            .unwrap(),
        "0.0000000003".parse::<bigdecimal::BigDecimal>().unwrap()
    );
    assert!(
        api.delete_pricing(target, &owned.pricing_id, &root_global)
            .await
            .unwrap()
            .success
    );
    assert_eq!(
        api.list_pricing_page(&query(target), &root_global)
            .await
            .unwrap()
            .total,
        0
    );
    let audits=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT COUNT(*)::bigint AS n FROM tenant_audit_events WHERE actor_user_id=$1 AND resource_id=$2",[a.owner_user_id.into(),owned.pricing_id.into()])).await.unwrap().unwrap();
    assert!(audits.try_get::<i64>("", "n").unwrap() >= 4);
    // Remove only this test's unique platform price. The test wrapper owns the entire disposable DB.
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"DELETE FROM pricing_models WHERE id=$1 AND scope_type='platform' AND tenant_id IS NULL AND model_name=$2",[Uuid::parse_str(&global.pricing_id).unwrap().into(),model.into()])).await.unwrap();
    cleanup.cleanup().await.unwrap();
}
