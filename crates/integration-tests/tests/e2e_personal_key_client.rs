//! Actual personal-Key SDK -> Axum -> isolated PostgreSQL; no real inference/payment.
use client_api::{
    ApiClient, ClientConfig, ClientError,
    api::api_key::{ApiKeyApi, CreateApiKeyRequest},
};
use integration_tests::{
    common::generate_test_id,
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{DbRouter, Tenant, User};
use keycompute_server::{AppState, create_router};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use uuid::Uuid;
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn console(state: &AppState, db: &DatabaseConnection, user: Uuid, tenant: &Tenant) -> String {
    let u = User::find_by_id(db, user).await.unwrap().unwrap();
    let global = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(user, None, u.token_version, None, None, 3600)
        .unwrap();
    let auth = state.auth.verify_token(&global).await.unwrap();
    state
        .auth
        .select_tenant(&auth, Some(tenant.id))
        .await
        .unwrap()
        .access_token
}
#[tokio::test]
async fn real_personal_key_client_creates_only_the_current_owners_key_with_server_expiration() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let mut cleanup = TestDataGuard::new(db.clone(), run.clone());
    let a = create_test_tenant(&db, "personal-client-a", &run).await;
    let b = create_test_tenant(&db, "personal-client-b", &run).await;
    let owner = create_test_user(&db, a.id, "personal-key-owner", &run).await;
    let peer = create_test_user(&db, a.id, "personal-key-peer", &run).await;
    let state = AppState::with_pool(DbRouter::single(db.clone()));
    let token = console(&state, &db, owner.id, &a).await;
    let admin = console(&state, &db, a.owner_user_id, &a).await;
    let foreign = console(&state, &db, b.owner_user_id, &b).await;
    let peer = console(&state, &db, peer.id, &a).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, create_router(state)).await.unwrap();
    }));
    let client =
        ApiClient::new(ClientConfig::new(format!("http://{addr}")).with_no_proxy(true)).unwrap();
    let api = ApiKeyApi::new(&client);
    let before = chrono::Utc::now();
    // This exact call previously sent the unsupported expires_at:null field.
    let created = api
        .create_api_key(
            &CreateApiKeyRequest::new("personal-client-contract"),
            &token,
        )
        .await;
    assert!(
        created.is_ok(),
        "personal SDK must match the actual strict server DTO: {created:?}"
    );
    let created = created.unwrap();
    let id: Uuid = created.id.parse().unwrap();
    let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT tenant_id,user_id,expires_at,produce_ai_key_hash FROM produce_ai_keys WHERE tenant_id=$1 AND user_id=$2 AND id=$3",
        [a.id.into(),owner.id.into(),id.into()])).await.unwrap().unwrap();
    assert_eq!(row.try_get::<Uuid>("", "tenant_id").unwrap(), a.id);
    assert_eq!(row.try_get::<Uuid>("", "user_id").unwrap(), owner.id);
    let expiry = row
        .try_get::<chrono::DateTime<chrono::Utc>>("", "expires_at")
        .unwrap();
    assert!(expiry >= before + chrono::Duration::days(180));
    assert!(expiry <= chrono::Utc::now() + chrono::Duration::days(180));
    assert_eq!(
        row.try_get::<String>("", "produce_ai_key_hash").unwrap(),
        keycompute_auth::ProduceAiKeyValidator::hash_key(&created.api_key)
    );
    assert_eq!(api.list_my_api_keys(false, &token).await.unwrap().len(), 1);
    assert!(
        api.list_my_api_keys(false, &admin)
            .await
            .unwrap()
            .is_empty(),
        "admin personal list must not widen"
    );
    for denied in [&foreign, &peer] {
        assert!(api.delete_api_key(&created.id, denied).await.is_err());
    }
    assert!(matches!(
        api.list_my_api_keys(false, &created.api_key).await,
        Err(ClientError::Forbidden(_) | ClientError::Unauthorized(_))
    ));
    assert!(api.delete_api_key(&created.id, &token).await.is_ok());
    let revoked = api.list_my_api_keys(true, &token).await.unwrap();
    assert_eq!(
        revoked.len(),
        1,
        "first personal removal preserves revoked metadata"
    );
    assert_eq!(revoked[0].id, created.id);
    assert!(revoked[0].revoked());
    assert!(
        api.list_my_api_keys(false, &token)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(api.delete_api_key(&created.id, &token).await.is_ok());
    assert!(api.list_my_api_keys(true, &token).await.unwrap().is_empty());
    // The only supported alternate lifetime is never_expires, not a custom date.
    let permanent = api
        .create_api_key(
            &CreateApiKeyRequest::new("personal-never-expires").with_never_expires(true),
            &token,
        )
        .await
        .unwrap();
    assert!(permanent.never_expires);
    assert!(permanent.expires_at.is_none());
    assert!(!format!("{permanent:?}").contains(&permanent.api_key));
    let transport = reqwest::Client::builder().no_proxy().build().unwrap();
    let rejected = transport
        .post(format!("http://{addr}/api/v1/keys"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name":"unsupported-date","expires_at":"2030-01-01T00:00:00Z"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        rejected.status().as_u16(),
        422,
        "old DTO is not a compatibility path"
    );
    let raw = transport
        .post(format!("http://{addr}/api/v1/keys"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name":"header-contract","never_expires":true}))
        .send()
        .await
        .unwrap();
    assert!(raw.status().is_success());
    assert!(
        raw.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    assert_eq!(raw.headers()["pragma"], "no-cache");
    let raw: serde_json::Value = raw.json().await.unwrap();
    assert!(
        api.delete_api_key(raw["key_id"].as_str().unwrap(), &token)
            .await
            .is_ok()
    );
    assert!(api.delete_api_key(&permanent.id, &token).await.is_ok());
    cleanup.cleanup().await.unwrap();
}
