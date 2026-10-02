use super::*;
use crate::{AuthService, AuthorizationAction, Permission, ProduceAiKeyValidator};
use jsonwebtoken::Algorithm;
use serde_json::{Value, json};

const SECRET: &str = "unit-test-only-identity-signing-key-00000000";
const ISSUER: &str = "unit-test-identity";

fn validator() -> JwtValidator {
    JwtValidator::new(SECRET, ISSUER)
}
fn claims() -> Value {
    let now = Utc::now().timestamp();
    json!({"sub":Uuid::new_v4(), "iss":ISSUER, "iat":now-1,
        "exp":now+3600, "token_version":7})
}
fn sign(value: &Value, algorithm: Algorithm) -> String {
    encode(
        &Header::new(algorithm),
        value,
        &EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .unwrap()
}

#[test]
fn global_identity_roundtrip_has_no_authority_before_database_validation() {
    let user = Uuid::new_v4();
    let v = validator();
    let token = v
        .generate_identity_token(user, None, 7, None, None, 3600)
        .unwrap();
    let c = v.validate(&token).unwrap();
    assert_eq!(c.user_id, user);
    assert_eq!(c.selected_tenant_id, None);
    assert_eq!(c.token_version, 7);
    assert_eq!(c.platform_role, PlatformRole::None);
    assert_eq!(c.tenant_role, None);
    assert!(c.permissions.is_empty());
    assert!(!c.has_permission(&Permission::AccessConsole));
    assert!(
        c.require_platform(AuthorizationAction::ManagePlatform)
            .is_err()
    );
}

#[test]
fn selected_identity_and_refresh_preserve_every_signed_version() {
    let (user, tenant) = (Uuid::new_v4(), Uuid::new_v4());
    let v = validator().with_expiration(1800);
    let token = v
        .generate_identity_token(user, Some(tenant), 9, Some(12), Some(35), 3600)
        .unwrap();
    let refreshed = v.refresh_token(&token).unwrap();
    let c = v.validate(&refreshed).unwrap();
    assert_eq!((c.user_id, c.selected_tenant_id), (user, Some(tenant)));
    assert_eq!(
        (c.token_version, c.authz_version, c.membership_authz_version),
        (9, Some(12), Some(35))
    );
    assert!(c.permissions.is_empty());
    let raw = v.validate_claims(&refreshed).unwrap();
    assert_eq!(raw.exp - raw.iat, 1800);
    let encoded = serde_json::to_value(raw).unwrap();
    for field in ["role", "platform_role", "tenant_role", "permissions"] {
        assert!(encoded.get(field).is_none());
    }
}

#[test]
fn wrong_signing_key_issuer_algorithm_and_signature_are_rejected() {
    let v = validator();
    let token = sign(&claims(), Algorithm::HS256);
    assert!(
        JwtValidator::new("different-unit-test-key", ISSUER)
            .validate(&token)
            .is_err()
    );
    assert!(
        JwtValidator::new(SECRET, "other-service")
            .validate(&token)
            .is_err()
    );
    assert!(v.validate(&sign(&claims(), Algorithm::HS512)).is_err());
    let mut bytes = token.into_bytes();
    let index = bytes.iter().rposition(|b| *b == b'.').unwrap() + 1;
    bytes[index] = if bytes[index] == b'A' { b'B' } else { b'A' };
    assert!(v.validate(std::str::from_utf8(&bytes).unwrap()).is_err());
    for invalid in ["", "not-a-jwt", "a.b.c"] {
        assert!(v.validate(invalid).is_err());
    }
}

#[test]
fn expired_tokens_and_invalid_issue_times_are_rejected() {
    let now = Utc::now().timestamp();
    for (iat, exp) in [
        (now - 100, now - 1),
        (now - 100, now),
        (now + 120, now + 3600),
        (now + 3600, now + 3600),
    ] {
        let mut c = claims();
        c["iat"] = json!(iat);
        c["exp"] = json!(exp);
        let token = sign(&c, Algorithm::HS256);
        assert!(validator().validate(&token).is_err(), "iat={iat} exp={exp}");
        assert!(validator().refresh_token(&token).is_err());
    }
}

#[test]
fn legacy_authority_claims_and_missing_identity_version_are_rejected() {
    for field in ["role", "platform_role", "tenant_role", "permissions"] {
        let mut c = claims();
        c[field] = json!("root");
        assert!(validator().validate(&sign(&c, Algorithm::HS256)).is_err());
    }
    let mut c = claims();
    c.as_object_mut().unwrap().remove("token_version");
    assert!(validator().validate(&sign(&c, Algorithm::HS256)).is_err());
}

#[test]
fn malformed_subjects_and_partial_or_nonpositive_tenant_versions_are_rejected() {
    let mut invalid = Vec::new();
    for sub in ["not-a-uuid", "00000000-0000-0000-0000-000000000000"] {
        let mut c = claims();
        c["sub"] = json!(sub);
        invalid.push(c);
    }
    let mut c = claims();
    c["token_version"] = json!(-1);
    invalid.push(c);
    for tenant in [
        "not-a-uuid".to_string(),
        Uuid::nil().to_string(),
        Uuid::new_v4().to_string(),
    ] {
        let mut c = claims();
        c["tenant_id"] = json!(tenant);
        invalid.push(c);
    }
    for (av, mv) in [(0, 1), (1, 0), (-1, 1), (1, -1)] {
        let mut c = claims();
        c["tenant_id"] = json!(Uuid::new_v4());
        c["authz_version"] = json!(av);
        c["membership_authz_version"] = json!(mv);
        invalid.push(c);
    }
    for field in ["authz_version", "membership_authz_version"] {
        let mut c = claims();
        c[field] = json!(1);
        invalid.push(c);
        let mut c = claims();
        c["tenant_id"] = json!(Uuid::new_v4());
        c[field] = json!(1);
        invalid.push(c);
    }
    for c in invalid {
        assert!(
            validator().validate(&sign(&c, Algorithm::HS256)).is_err(),
            "{c}"
        );
    }
}

#[test]
fn signer_rejects_inconsistent_identity_and_tenant_arguments() {
    let v = validator();
    let user = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    for (u, t, tv, av, mv) in [
        (Uuid::nil(), None, 0, None, None),
        (user, None, -1, None, None),
        (user, Some(Uuid::nil()), 0, Some(1), Some(1)),
        (user, Some(tenant), 0, None, Some(1)),
        (user, Some(tenant), 0, Some(1), None),
        (user, None, 0, Some(1), Some(1)),
        (user, Some(tenant), 0, Some(0), Some(1)),
    ] {
        assert!(v.generate_identity_token(u, t, tv, av, mv, 3600).is_err());
    }
}

#[tokio::test]
async fn structurally_valid_jwt_cannot_authenticate_without_identity_storage() {
    let v = validator();
    let token = v
        .generate_identity_token(Uuid::new_v4(), None, 0, None, None, 3600)
        .unwrap();
    let auth = AuthService::new(ProduceAiKeyValidator::new()).with_jwt(v);
    assert!(auth.verify_jwt(&token).unwrap().permissions.is_empty());
    assert!(matches!(
        auth.verify_token(&token).await,
        Err(KeyComputeError::ServiceUnavailable(_))
    ));
    let with_missing_pool = auth.with_user_service(crate::UserService::new());
    assert!(with_missing_pool.verify_token(&token).await.is_err());
}
