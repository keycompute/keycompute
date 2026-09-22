//! PostgreSQL + HTTP invitation acceptance regressions.

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use integration_tests::{
    common::generate_test_id,
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{DbRouter, Tenant, TenantMembership, User};
use keycompute_server::{AppState, AppStateConfig, create_router};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::{Value, json};
use tower::ServiceExt;

struct InvitationFixture {
    db: DatabaseConnection,
    guard: TestDataGuard,
    state: AppState,
    tenant: Tenant,
    inviter: User,
    inviter_token: String,
    invitee: User,
    invitee_global_token: String,
    run: String,
}

impl InvitationFixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run.clone());
        let tenant = create_test_tenant(&db, "invite", &run).await;
        let inviter = User::find_by_id(&db, tenant.owner_user_id)
            .await
            .unwrap()
            .unwrap();
        let invitee = create_test_user(&db, tenant.id, "invitee", &run).await.user;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET status='removed' WHERE tenant_id=$1 AND user_id=$2",
            [tenant.id.into(), invitee.id.into()],
        ))
        .await
        .unwrap();
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE user_credentials SET email_verified=TRUE,email_verified_at=NOW() WHERE user_id=$1",
            [invitee.id.into()],
        ))
        .await
        .unwrap();
        // The fixture helper may not create a credential row; insert one only
        // when the local database has not already provisioned it.
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO user_credentials(user_id,password_hash,email_verified,email_verified_at) VALUES($1,'fixture',TRUE,NOW()) ON CONFLICT(user_id) DO UPDATE SET email_verified=TRUE,email_verified_at=NOW()",
            [invitee.id.into()],
        ))
        .await
        .unwrap();
        let config = AppStateConfig {
            app_base_url: Some("http://localhost:3000".into()),
            ..Default::default()
        };
        let state = AppState::try_with_pool_and_config(DbRouter::single(db.clone()), config)
            .await
            .unwrap();
        let inviter_membership = TenantMembership::find(&db, tenant.id, inviter.id)
            .await
            .unwrap()
            .unwrap();
        let inviter_token = state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(
                inviter.id,
                Some(tenant.id),
                inviter.token_version,
                Some(tenant.authz_version),
                Some(inviter_membership.authz_version),
                3600,
            )
            .unwrap();
        let invitee_global_token = state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(invitee.id, None, invitee.token_version, None, None, 3600)
            .unwrap();
        Self {
            db,
            guard,
            state,
            tenant,
            inviter,
            inviter_token,
            invitee,
            invitee_global_token,
            run,
        }
    }

    async fn request(
        &self,
        method: Method,
        path: impl Into<String>,
        token: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(path.into())
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                body.map(|value| value.to_string()).unwrap_or_default(),
            ))
            .unwrap();
        let response = create_router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

fn token_from_fragment(value: &Value) -> String {
    value["acceptance_link"]
        .as_str()
        .and_then(|link| link.split("#token=").nth(1))
        .expect("created invite must return a fragment link")
        .to_owned()
}

#[tokio::test]
async fn invitation_create_reports_notification_state_and_never_lists_secret() {
    let mut f = InvitationFixture::new().await;
    let (status, body) = f
        .request(
            Method::POST,
            format!("/api/v1/tenants/{}/invitations", f.tenant.id),
            &f.inviter_token,
            Some(json!({
                "email": f.invitee.email,
                "tenant_role": "member",
                "expires_in_seconds": 3600
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["outcome"], "created");
    assert_eq!(body["invitation"]["invited_by"], f.inviter.id.to_string());
    assert_eq!(body["notification"], "unconfigured");
    assert!(body["invitation"].get("token_hash").is_none());
    assert!(body["invitation"].get("token").is_none());
    assert!(body["acceptance_link"].is_string());
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn invitation_acceptance_is_one_time_and_email_verified() {
    let mut f = InvitationFixture::new().await;
    let (_, created) = f
        .request(
            Method::POST,
            format!("/api/v1/tenants/{}/invitations", f.tenant.id),
            &f.inviter_token,
            Some(json!({
                "email": f.invitee.email,
                "tenant_role": "member",
                "expires_in_seconds": 3600
            })),
        )
        .await;
    let token = token_from_fragment(&created);
    let (status, body) = f
        .request(
            Method::POST,
            format!("/api/v1/invitations/{token}/accept"),
            &f.invitee_global_token,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["membership"]["user_id"], f.invitee.id.to_string());
    assert_eq!(body["membership"]["membership_status"], "active");

    let (status, _) = f
        .request(
            Method::POST,
            format!("/api/v1/invitations/{token}/accept"),
            &f.invitee_global_token,
            None,
        )
        .await;
    assert!(matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::CONFLICT
    ));
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn concurrent_acceptance_has_one_winner_and_foreign_revoke_has_no_effect() {
    let mut f = InvitationFixture::new().await;
    let (_, created) = f
        .request(
            Method::POST,
            format!("/api/v1/tenants/{}/invitations", f.tenant.id),
            &f.inviter_token,
            Some(json!({
                "email": f.invitee.email,
                "tenant_role": "member",
                "expires_in_seconds": 3600
            })),
        )
        .await;
    let token = token_from_fragment(&created);
    let app_a = create_router(f.state.clone());
    let app_b = create_router(f.state.clone());
    let request = |app: axum::Router| async {
        app.oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/api/v1/invitations/{token}/accept"))
                .header(
                    "authorization",
                    format!("Bearer {}", f.invitee_global_token),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
    };
    let (left, right) = tokio::join!(request(app_a), request(app_b));
    assert_eq!(
        [left == StatusCode::OK, right == StatusCode::OK]
            .into_iter()
            .filter(|winner| *winner)
            .count(),
        1
    );

    let foreign_tenant = create_test_tenant(&f.db, "invite-foreign", &f.run).await;
    let (status, _) = f
        .request(
            Method::POST,
            format!(
                "/api/v1/tenants/{}/invitations/{}/revoke",
                foreign_tenant.id, created["invitation"]["id"]
            ),
            &f.inviter_token,
            None,
        )
        .await;
    assert!(matches!(
        status,
        StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    ));
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn malformed_invitation_payload_and_verified_email_mismatch_are_rejected() {
    let mut f = InvitationFixture::new().await;
    let (status, _) = f
        .request(
            Method::POST,
            format!("/api/v1/tenants/{}/invitations", f.tenant.id),
            &f.inviter_token,
            Some(json!({
                "email": f.invitee.email,
                "tenant_role": "member",
                "expires_in_seconds": 3600,
                "invited_by": f.invitee.id
            })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (_, created) = f
        .request(
            Method::POST,
            format!("/api/v1/tenants/{}/invitations", f.tenant.id),
            &f.inviter_token,
            Some(json!({
                "email": f.invitee.email,
                "tenant_role": "member",
                "expires_in_seconds": 3600
            })),
        )
        .await;
    let token = token_from_fragment(&created);
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET email='different@example.com' WHERE id=$1",
        [f.invitee.id.into()],
    ))
    .await
    .unwrap();
    let (status, _) = f
        .request(
            Method::POST,
            format!("/api/v1/invitations/{token}/accept"),
            &f.invitee_global_token,
            None,
        )
        .await;
    assert!(matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::FORBIDDEN
    ));
    f.guard.cleanup().await.unwrap();
}

async fn local_smtp() -> (
    keycompute_emailserver::EmailConfig,
    tokio::task::JoinHandle<String>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (read, mut write) = socket.into_split();
        let mut read = BufReader::new(read);
        write
            .write_all(b"220 localhost ESMTP test\r\n")
            .await
            .unwrap();
        loop {
            let mut line = String::new();
            assert!(read.read_line(&mut line).await.unwrap() > 0);
            let reply: &[u8] = if line.starts_with("EHLO") {
                b"250-localhost\r\n250-AUTH PLAIN\r\n250 SIZE 131072\r\n"
            } else if line.starts_with("AUTH") {
                b"235 authenticated\r\n"
            } else if line.starts_with("MAIL") || line.starts_with("RCPT") {
                b"250 ok\r\n"
            } else if line.starts_with("DATA") {
                write.write_all(b"354 end with dot\r\n").await.unwrap();
                let mut message = String::new();
                loop {
                    let mut part = String::new();
                    assert!(read.read_line(&mut part).await.unwrap() > 0);
                    if part == ".\r\n" {
                        break;
                    }
                    message.push_str(&part);
                    assert!(message.len() < 131072);
                }
                write.write_all(b"250 queued\r\n").await.unwrap();
                return message;
            } else {
                panic!("unexpected local SMTP command");
            };
            write.write_all(reply).await.unwrap();
        }
    });
    let config = keycompute_emailserver::EmailConfig {
        smtp_host: "127.0.0.1".into(),
        smtp_port: port,
        smtp_username: "local-test".into(),
        smtp_password: "local-test-only".into(),
        from_address: "test@fixture.invalid".into(),
        use_tls: false,
        timeout_secs: 3,
        ..Default::default()
    };
    (config, task)
}

#[tokio::test]
async fn notification_uses_real_smtp_and_duplicate_invites_do_not_expose_another_token() {
    let mut f = InvitationFixture::new().await;
    let (config, smtp) = local_smtp().await;
    f.state.email_service = std::sync::Arc::new(keycompute_emailserver::EmailService::new(config));
    let path = format!("/api/v1/tenants/{}/invitations", f.tenant.id);
    let payload = json!({"email":f.invitee.email,"tenant_role":"member","expires_in_seconds":3600});
    let (status, first) = f
        .request(Method::POST, &path, &f.inviter_token, Some(payload.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["notification"], "sent");
    let delivered = tokio::time::timeout(std::time::Duration::from_secs(5), smtp)
        .await
        .unwrap()
        .unwrap();
    assert!(delivered.contains(&f.invitee.email));
    let readable = delivered.replace("=\r\n", "").replace("=3D", "=");
    assert!(readable.contains(&token_from_fragment(&first)));
    let (status, duplicate) = f
        .request(Method::POST, &path, &f.inviter_token, Some(payload))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(duplicate["outcome"], "already_pending");
    assert_eq!(duplicate["notification"], "not_applicable");
    assert!(duplicate["acceptance_link"].is_null());
    assert_eq!(first["invitation"]["id"], duplicate["invitation"]["id"]);
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn smtp_failure_is_explicit_and_does_not_undo_the_committed_invitation() {
    use tokio::io::AsyncWriteExt;
    let mut f = InvitationFixture::new().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let smtp = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket
            .write_all(b"421 local test unavailable\r\n")
            .await
            .unwrap();
    });
    f.state.email_service = std::sync::Arc::new(keycompute_emailserver::EmailService::new(
        keycompute_emailserver::EmailConfig {
            smtp_host: "127.0.0.1".into(),
            smtp_port: port,
            smtp_username: "local".into(),
            smtp_password: "local-test-only".into(),
            from_address: "test@fixture.invalid".into(),
            use_tls: false,
            timeout_secs: 3,
            ..Default::default()
        },
    ));
    let (status, created) = f
        .request(
            Method::POST,
            format!("/api/v1/tenants/{}/invitations", f.tenant.id),
            &f.inviter_token,
            Some(json!({"email":f.invitee.email,"tenant_role":"member","expires_in_seconds":3600})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["notification"], "failed");
    tokio::time::timeout(std::time::Duration::from_secs(5), smtp)
        .await
        .unwrap()
        .unwrap();
    let token = token_from_fragment(&created);
    let (status, _) = f
        .request(
            Method::POST,
            format!("/api/v1/invitations/{token}/accept"),
            &f.invitee_global_token,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn existing_members_and_empty_configuration_changes_are_business_rejections() {
    let mut f = InvitationFixture::new().await;
    let path = format!("/api/v1/tenants/{}/invitations", f.tenant.id);
    let (status, _) = f
        .request(
            Method::POST,
            &path,
            &f.inviter_token,
            Some(json!({"email":f.inviter.email,"tenant_role":"member","expires_in_seconds":3600})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, page) = f.request(Method::GET, &path, &f.inviter_token, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 0);
    let (status, _) = f
        .request(
            Method::PATCH,
            format!("/api/v1/tenants/{}", f.tenant.id),
            &f.inviter_token,
            Some(json!({"expected_authz_version":f.tenant.authz_version})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let current = Tenant::find_by_id(&f.db, f.tenant.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.authz_version, f.tenant.authz_version);
    f.guard.cleanup().await.unwrap();
}
