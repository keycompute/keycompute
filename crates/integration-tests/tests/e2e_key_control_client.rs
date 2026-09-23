//! Real owner-only issuance client -> HTTP -> isolated PostgreSQL. No external providers.
use client_api::{ApiClient, ClientConfig, ClientError, api::key_control::*};
use integration_tests::{
    common::generate_test_id,
    db::{
        TestDataGuard, create_test_api_key, create_test_pool, create_test_tenant, create_test_user,
    },
};
use keycompute_auth::ProduceAiKeyValidator;
use keycompute_db::{CreateProduceAiKeyRequest, DbRouter, User};
use keycompute_server::{AppState, create_router};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use uuid::Uuid;
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn token(state: &AppState, db: &DatabaseConnection, tenant: Uuid, user: Uuid) -> String {
    let u = User::find_by_id(db, user).await.unwrap().unwrap();
    let raw = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(user, None, u.token_version, None, None, 3600)
        .unwrap();
    let auth = state.auth.verify_token(&raw).await.unwrap();
    state
        .auth
        .select_tenant(&auth, Some(tenant))
        .await
        .unwrap()
        .access_token
}
#[tokio::test]
async fn real_key_clients_keep_admin_metadata_and_owner_claims_in_separate_scopes() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let mut cleanup = TestDataGuard::new(db.clone(), run.clone());
    let a = create_test_tenant(&db, "key-client-a", &run).await;
    let b = create_test_tenant(&db, "key-client-b", &run).await;
    let owner = create_test_user(&db, a.id, "key-client-owner", &run).await;
    let peer = create_test_user(&db, a.id, "key-client-peer", &run).await;
    let state = AppState::with_pool(DbRouter::single(db.clone()));
    let admin = token(&state, &db, a.id, a.owner_user_id).await;
    let own = token(&state, &db, a.id, owner.id).await;
    let other = token(&state, &db, a.id, peer.id).await;
    let foreign = token(&state, &db, b.id, b.owner_user_id).await;
    let inference = ProduceAiKeyValidator::generate_key();
    create_test_api_key(
        &db,
        &CreateProduceAiKeyRequest {
            tenant_id: a.id,
            user_id: owner.id,
            name: "key-client-inference-only".into(),
            produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&inference),
            produce_ai_key_preview: "fixture***".into(),
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
    let client = ApiClient::new(
        ClientConfig::new(format!("http://{addr}"))
            .with_no_proxy(true)
            .with_console_display_cache(true),
    )
    .unwrap();
    let api = TenantKeyApi::new(&client, a.id).unwrap();
    let mine = OwnerKeyIssuanceApi::new(&client, a.id, owner.id).unwrap();
    for denied in [&own, &other, &foreign, &inference] {
        assert!(matches!(
            api.list(&KeyQuery::default(), denied).await,
            Err(ClientError::Unauthorized(_) | ClientError::Forbidden(_))
        ));
    }
    let requested = api
        .request(
            &NewIssuance {
                owner_user_id: owner.id,
                name: "owner-issued".into(),
                expires_at: None,
            },
            &admin,
        )
        .await
        .unwrap();
    assert_eq!(requested.outcome, IssuanceOutcome::Created);
    let pending = mine.list(1, 20, &own).await.unwrap();
    let intent = pending
        .intents
        .iter()
        .find(|i| i.id == requested.intent.id)
        .unwrap()
        .clone();
    // The expected owner in this client is not an authority grant: wrong JWTs still reach server denial.
    for denied in [&admin, &other, &inference] {
        assert!(mine.claim(&intent, denied).await.is_err());
    }
    let claimed = mine.claim(&intent, &own).await.unwrap();
    assert!(claimed.secret_returned_once);
    assert!(!format!("{claimed:?}").contains(claimed.key.expose()));
    assert!(mine.claim(&intent, &own).await.is_err());
    let row = api.detail(claimed.key_id, &admin).await.unwrap();
    assert_eq!(row.owner_user_id, owner.id);
    assert_eq!(row.tenant_id, a.id);
    assert!(!row.revoked);
    let foreign_api = TenantKeyApi::new(&client, b.id).unwrap();
    assert!(foreign_api.detail(row.id, &foreign).await.is_err());
    let patch = KeyPatch {
        expected_updated_at: row.updated_at.clone(),
        name: Some("owner-updated".into()),
        expires_at: Some(None),
    };
    let updated = api.patch(row.id, &patch, &admin).await.unwrap();
    assert_eq!(updated.name, "owner-updated");
    assert!(api.patch(row.id, &patch, &admin).await.is_err());
    let rotation = api
        .rotate(
            row.id,
            &RotateKey {
                name: "owner-rotated".into(),
                expires_at: None,
            },
            &admin,
        )
        .await
        .unwrap();
    assert_eq!(rotation.intent.owner_user_id, owner.id);
    assert_eq!(rotation.intent.replaces_key_id, Some(row.id));
    assert!(
        !api.detail(row.id, &admin).await.unwrap().revoked,
        "rotation request is not activation"
    );
    let replacement = mine.claim(&rotation.intent, &own).await.unwrap();
    assert_ne!(replacement.key_id, row.id);
    assert!(api.detail(row.id, &admin).await.unwrap().revoked);
    assert_eq!(
        api.detail(replacement.key_id, &admin)
            .await
            .unwrap()
            .owner_user_id,
        owner.id
    );
    let revoked = api.revoke(replacement.key_id, &admin).await.unwrap();
    assert!(revoked.key.unwrap().revoked);
    let removed = api.delete(replacement.key_id, &admin).await.unwrap();
    assert_eq!(removed.key_id, replacement.key_id);
    assert!(removed.deleted || removed.key.as_ref().is_some_and(|key| key.revoked));
    let decline = api
        .request(
            &NewIssuance {
                owner_user_id: owner.id,
                name: "owner-declines".into(),
                expires_at: None,
            },
            &admin,
        )
        .await
        .unwrap();
    assert_eq!(
        mine.decline(&decline.intent, &own).await.unwrap().outcome,
        IssuanceOutcome::Declined
    );
    assert!(mine.claim(&decline.intent, &own).await.is_err());
    let cancel = api
        .request(
            &NewIssuance {
                owner_user_id: owner.id,
                name: "admin-cancels".into(),
                expires_at: None,
            },
            &admin,
        )
        .await
        .unwrap();
    let page = api.issuances(1, 20, Some(owner.id), &admin).await.unwrap();
    assert!(page.intents.iter().any(|i| i.id == cancel.intent.id));
    assert_eq!(
        api.cancel(cancel.intent.id, &admin).await.unwrap().outcome,
        IssuanceOutcome::Cancelled
    );
    assert!(mine.claim(&cancel.intent, &own).await.is_err());
    let listed = api
        .list(
            &KeyQuery {
                owner_user_id: Some(owner.id),
                include_revoked: true,
                ..Default::default()
            },
            &admin,
        )
        .await
        .unwrap();
    assert!(
        listed
            .keys
            .iter()
            .all(|k| k.tenant_id == a.id && k.owner_user_id == owner.id)
    );
    let audit=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT COUNT(*)::bigint AS n FROM tenant_audit_events WHERE tenant_id=$1 AND actor_user_id=$2",[a.id.into(),owner.id.into()])).await.unwrap().unwrap();
    assert!(audit.try_get::<i64>("", "n").unwrap() >= 3);
    cleanup.cleanup().await.unwrap();
}
