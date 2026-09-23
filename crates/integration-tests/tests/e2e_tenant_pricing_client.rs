//! Actual tenant pricing client -> Axum -> PostgreSQL contract, no upstream/payment IO.
use client_api::{
    ApiClient, ClientConfig, ClientError,
    api::tenant_pricing::{
        BillingDimension, CreateTenantPrice, TenantPricingApi, UpdateTenantPrice,
    },
};
use integration_tests::{
    common::generate_test_id,
    db::{
        TestDataGuard, cleanup_test_data, create_test_api_key, create_test_pool,
        create_test_tenant, create_test_user,
    },
};
use keycompute_auth::ProduceAiKeyValidator;
use keycompute_db::{CreateProduceAiKeyRequest, DbRouter, Tenant, User};
use keycompute_server::{AppState, create_router};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use uuid::Uuid;
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn console(
    state: &AppState,
    db: &DatabaseConnection,
    tenant: &Tenant,
    user_id: Uuid,
) -> String {
    let user = User::find_by_id(db, user_id).await.unwrap().unwrap();
    let raw = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(user.id, None, user.token_version, None, None, 3600)
        .unwrap();
    let actor = state.auth.verify_token(&raw).await.unwrap();
    state
        .auth
        .select_tenant(&actor, Some(tenant.id))
        .await
        .unwrap()
        .access_token
}
#[tokio::test]
async fn real_tenant_pricing_client_retains_exact_scope_revisions_and_backend_authority() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let _cleanup = TestDataGuard::new(db.clone(), run.clone());
    let tenant = create_test_tenant(&db, "pricing-client-a", &run).await;
    let foreign = create_test_tenant(&db, "pricing-client-b", &run).await;
    let member = create_test_user(&db, tenant.id, "pricing-client-member", &run).await;
    let state = AppState::with_pool(DbRouter::single(db.clone()));
    let admin = console(&state, &db, &tenant, tenant.owner_user_id).await;
    let peer = console(&state, &db, &tenant, member.id).await;
    let outsider = console(&state, &db, &foreign, foreign.owner_user_id).await;
    let raw_key = ProduceAiKeyValidator::generate_key();
    create_test_api_key(
        &db,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: tenant.owner_user_id,
            name: "pricing-client".into(),
            produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&raw_key),
            produce_ai_key_preview: "test-only".into(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = create_router(state);
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    let client =
        ApiClient::new(ClientConfig::new(format!("http://{address}")).with_no_proxy(true)).unwrap();
    let api = TenantPricingApi::new(&client, tenant.id).unwrap();
    let other_api = TenantPricingApi::new(&client, foreign.id).unwrap();
    for denied in [&peer, &outsider, &raw_key] {
        assert!(matches!(
            api.list(1, 20, "", denied).await,
            Err(ClientError::Forbidden(_) | ClientError::Unauthorized(_))
        ));
    }
    let request = CreateTenantPrice {
        model_name: format!("pricing-client-{run}"),
        billing_dimension: BillingDimension::ProviderAccount,
        currency: "CNY".into(),
        input_price_per_1k: "0.0000000001".into(),
        output_price_per_1k: "9999999999.9999999999".into(),
        is_default: false,
        effective_from: None,
        effective_until: None,
    };
    let created = api.create(&request, &admin).await.unwrap();
    assert_eq!(created.tenant_id, tenant.id);
    assert!(created.version > 0);
    assert_eq!(
        created
            .input_price_per_1k
            .parse::<bigdecimal::BigDecimal>()
            .unwrap(),
        "0.0000000001".parse::<bigdecimal::BigDecimal>().unwrap()
    );
    let result = api.list(1, 20, &request.model_name, &admin).await.unwrap();
    assert_eq!(result.total, 1);
    assert_eq!(result.pricing[0].id, created.id);
    assert!(other_api.detail(created.id, &outsider).await.is_err());
    let patch = UpdateTenantPrice {
        expected_version: created.version,
        input_price_per_1k: Some("0.0000000002".into()),
        output_price_per_1k: None,
        effective_until: None,
    };
    let updated = api.update(created.id, &patch, &admin).await.unwrap();
    assert_eq!(updated.tenant_id, tenant.id);
    assert!(updated.version > created.version);
    assert!(api.update(created.id, &patch, &admin).await.is_err());
    assert_eq!(
        api.detail(created.id, &admin).await.unwrap().version,
        updated.version
    );
    let default = api.make_default(created.id, &admin).await.unwrap();
    assert!(default.is_default);
    assert!(api.delete(created.id, &admin).await.unwrap().success);
    assert_eq!(
        api.list(1, 20, &request.model_name, &admin)
            .await
            .unwrap()
            .total,
        0
    );
    assert!(
        other_api
            .list(1, 20, &request.model_name, &outsider)
            .await
            .unwrap()
            .pricing
            .is_empty()
    );
    let rows=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT COUNT(*)::bigint AS n FROM tenant_audit_events WHERE tenant_id=$1 AND actor_user_id=$2 AND resource_id=$3",[tenant.id.into(),tenant.owner_user_id.into(),created.id.to_string().into()])).await.unwrap().unwrap();
    assert!(
        rows.try_get::<i64>("", "n").unwrap() >= 4,
        "successful pricing changes must be audited"
    );
    cleanup_test_data(&db, &run).await.unwrap();
}
