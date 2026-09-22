//! JWT identity tokens are invalidated by the global user's token version.

use integration_tests::{
    common::generate_test_id,
    db::{cleanup_test_data, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_auth::{AuthService, JwtValidator, ProduceAiKeyValidator, UserService};
use keycompute_db::{DbRouter, User};
use std::sync::Arc;

fn auth(router: Arc<DbRouter>) -> AuthService {
    AuthService::new(ProduceAiKeyValidator::with_pool(Arc::clone(&router)))
        .with_jwt(JwtValidator::new("test-secret", "keycompute"))
        .with_user_service(UserService::with_pool(router))
}

#[tokio::test]
async fn stale_identity_token_is_rejected_after_version_bump() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let tenant = create_test_tenant(&db, "token-version", &run).await;
    let actor = create_test_user(&db, tenant.id, "token-version", &run).await;
    let router = Arc::new(DbRouter::single(db.clone()));
    let service = auth(Arc::clone(&router));
    let jwt = service.get_jwt_validator().unwrap();
    let membership = keycompute_db::TenantMembership::find(&db, tenant.id, actor.id)
        .await
        .unwrap()
        .unwrap();
    let token = jwt
        .generate_identity_token(
            actor.id,
            Some(tenant.id),
            actor.token_version,
            Some(tenant.authz_version),
            Some(membership.authz_version),
            3600,
        )
        .unwrap();
    service.verify_token(&token).await.unwrap();
    User::increment_token_version(&db, actor.id).await.unwrap();
    assert!(service.verify_token(&token).await.is_err());
    cleanup_test_data(&db, &run).await.unwrap();
}

#[tokio::test]
async fn structural_validator_accepts_global_identity_without_tenant() {
    let jwt = JwtValidator::new("test-secret", "keycompute");
    let user_id = uuid::Uuid::new_v4();
    let token = jwt
        .generate_identity_token(user_id, None, 42, None, None, 3600)
        .unwrap();
    let context = jwt.validate(&token).unwrap();
    assert_eq!(context.user_id, user_id);
    assert!(context.selected_tenant_id.is_none());
    assert_eq!(context.token_version, 42);
}

#[tokio::test]
async fn membership_authz_version_is_part_of_selected_tenant_identity() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let tenant = create_test_tenant(&db, "membership-version", &run).await;
    let actor = create_test_user(&db, tenant.id, "membership-version", &run).await;
    let membership = keycompute_db::TenantMembership::find(&db, tenant.id, actor.id)
        .await
        .unwrap()
        .unwrap();
    let jwt = JwtValidator::new("test-secret", "keycompute");
    let token = jwt
        .generate_identity_token(
            actor.id,
            Some(tenant.id),
            actor.token_version,
            Some(tenant.authz_version),
            Some(membership.authz_version),
            3600,
        )
        .unwrap();
    let claims = jwt.validate_claims(&token).unwrap();
    assert_eq!(claims.tenant_id().unwrap(), Some(tenant.id));
    assert_eq!(
        claims.membership_authz_version,
        Some(membership.authz_version)
    );
    cleanup_test_data(&db, &run).await.unwrap();
}
