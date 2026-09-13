//! Postgres 实际数据限流测试
//!
//! 验证数据库中的 accounts 账号配置会被路由限流链路命中。

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use integration_tests::common::generate_test_id;
use integration_tests::db::{
    TestDataGuard, cleanup_test_data, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_auth::ProduceAiKeyValidator;
use keycompute_db::{
    CreateAccountRequest, CreateProduceAiKeyRequest, DbRouter, UpdateTenantRequest,
};
use keycompute_ratelimit::{RateLimitConfig, RateLimitKey};
use keycompute_server::{create_router, state::AppState};
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

async fn post_generation_json(
    app: &axum::Router,
    token: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    post_generation_json_with_headers(app, token, uri, body, &[]).await
}

async fn post_generation_json_with_headers(
    app: &axum::Router,
    token: &str,
    uri: &str,
    body: serde_json::Value,
    headers: &[(&str, &str)],
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", token));

    for (name, value) in headers {
        request = request.header(*name, *value);
    }

    let request = request
        .body(Body::from(body.to_string()))
        .expect("generation request should build");

    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("generation request should complete");

    let status = response.status();
    let body = to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("generation response should be readable");
    let body = serde_json::from_slice(&body).expect("generation response should contain JSON");

    (status, body)
}

fn assert_rate_limit_error(body: &serde_json::Value) {
    let canonical_code = body
        .pointer("/error/code")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            body.pointer("/error/type")
                .and_then(serde_json::Value::as_str)
                .filter(|ty| *ty == "rate_limit_error")
                .map(|_| "rate_limit_exceeded")
        });

    assert_eq!(
        canonical_code,
        Some("rate_limit_exceeded"),
        "rate limit reject should return canonical code"
    );
}

#[tokio::test]
async fn postgres_account_rpm_limit_from_db_is_enforced_for_all_generation_endpoints() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    let mut test_data_guard = TestDataGuard::new(pool.clone(), test_id.clone());
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("postgres rate test cleanup should succeed");

    let tenant = create_test_tenant(&pool, "db-rate-all-routes", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "db-rate-all-routes", &test_id).await;

    let api_key = ProduceAiKeyValidator::generate_key();
    let api_key_hash = ProduceAiKeyValidator::hash_key(&api_key);
    let api_key_row = keycompute_db::ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: format!("postgres-rate-limit-all-routes-{test_id}"),
            produce_ai_key_hash: api_key_hash,
            produce_ai_key_preview: build_preview(&api_key),
            expires_at: None,
        },
    )
    .await
    .expect("test API key should be created");

    let chat_model = format!("gpt-all-routes-chat-{test_id}");
    let anthropic_model = format!("claude-all-routes-{test_id}");
    let responses_model = format!("gpt-all-routes-responses-{test_id}");
    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "openai".to_string(),
            name: format!("db-all-routes-openai-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-openai-key".to_string(),
            upstream_api_key_preview: "test-openai-key".to_string(),
            rpm_limit: Some(1),
            tpm_limit: Some(1),
            priority: Some(0),
            models_supported: vec![chat_model.clone(), responses_model.clone()],
            api_capabilities: vec!["chat_completions".to_string(), "responses".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test openai account should be created");

    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "anthropic".to_string(),
            name: format!("db-all-routes-anthropic-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-anthropic-key".to_string(),
            upstream_api_key_preview: "test-anthropic-key".to_string(),
            rpm_limit: Some(1),
            tpm_limit: Some(1),
            priority: Some(0),
            models_supported: vec![anthropic_model.clone()],
            api_capabilities: vec!["messages".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test anthropic account should be created");

    let state = AppState::with_pool(DbRouter::single(pool.clone()));
    let app = create_router(state.clone());
    let request_key = RateLimitKey::new(tenant.id, user.id, api_key_row.id);
    let null_key = RateLimitKey::new(tenant.id, user.id, Uuid::nil());

    // The account fixtures use the smallest valid positive limits. Prefill
    // the caller bucket so the first request is deterministically rejected by
    // RPM rather than relying on the historical zero-value clamp.
    state
        .rate_limiter
        .check_and_record_with_config(&request_key, &RateLimitConfig::new(1, 1))
        .await
        .expect("prefill should consume the one-request RPM bucket");

    let before_count = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should be readable");
    let before_nil_count = state
        .rate_limiter
        .get_rpm_count(&null_key)
        .await
        .expect("rpm counter should be readable for nil key");

    assert_eq!(before_count, 1);
    assert_eq!(before_nil_count, 0);

    let (chat_status, chat_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/chat/completions",
        json!({
            "model": chat_model,
            "messages": [{"role": "user", "content": "hello"}],
        }),
    )
    .await;
    assert_eq!(chat_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&chat_body);

    let (messages_status, messages_body) = post_generation_json_with_headers(
        &app,
        &api_key,
        "/v1/messages",
        json!({
            "model": anthropic_model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hello"}],
        }),
        &[("anthropic-version", "2023-06-01")],
    )
    .await;
    assert_eq!(messages_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&messages_body);

    let (responses_status, responses_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/responses",
        json!({
            "model": responses_model,
            "input": "hello",
            "max_output_tokens": 16,
        }),
    )
    .await;
    assert_eq!(responses_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&responses_body);

    let (compact_status, compact_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/responses/compact",
        json!({
            "model": responses_model,
            "input": "hello",
            "max_output_tokens": 16,
        }),
    )
    .await;
    assert_eq!(compact_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&compact_body);

    let after_count = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should still be readable");
    let after_nil_count = state
        .rate_limiter
        .get_rpm_count(&null_key)
        .await
        .expect("rpm counter should still be readable for nil key");

    assert_eq!(
        after_count, before_count,
        "rejected generation requests must not consume additional RPM"
    );
    assert_eq!(after_nil_count, before_nil_count);

    assert_eq!(
        api_key_row.produce_ai_key_preview,
        build_preview(&api_key),
        "stored key preview should match test key prefix"
    );
    test_data_guard
        .cleanup()
        .await
        .expect("postgres rate test teardown should succeed");
}

#[tokio::test]
async fn postgres_tenant_rpm_cap_is_not_widened_by_a_provider_account() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    let mut test_data_guard = TestDataGuard::new(pool.clone(), test_id.clone());
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("postgres rate test cleanup should succeed");

    let tenant = create_test_tenant(&pool, "db-rate-tenant-cap", &test_id).await;
    let tenant = tenant
        .update(
            &pool,
            &UpdateTenantRequest {
                name: None,
                description: None,
                status: None,
                default_rpm_limit: Some(1),
                default_tpm_limit: None,
            },
        )
        .await
        .expect("tenant RPM limit should be updated");
    let user = create_test_user(&pool, tenant.id, "db-rate-tenant-cap", &test_id).await;
    let api_key = ProduceAiKeyValidator::generate_key();
    let api_key_row = keycompute_db::ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: format!("postgres-tenant-cap-{test_id}"),
            produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&api_key),
            produce_ai_key_preview: build_preview(&api_key),
            expires_at: None,
        },
    )
    .await
    .expect("test API key should be created");
    let model = format!("gpt-tenant-cap-{test_id}");
    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "openai".to_string(),
            name: format!("db-tenant-cap-account-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-openai-key".to_string(),
            upstream_api_key_preview: "test-openai-key".to_string(),
            rpm_limit: Some(10_000),
            tpm_limit: Some(10_000),
            priority: Some(0),
            models_supported: vec![model.clone()],
            api_capabilities: vec!["chat_completions".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test provider account should be created");

    let state = AppState::with_pool(DbRouter::single(pool));
    let request_key = RateLimitKey::new(tenant.id, user.id, api_key_row.id);
    state
        .rate_limiter
        .check_and_record_with_config(&request_key, &RateLimitConfig::new(1, 10_000))
        .await
        .expect("request key should consume the tenant RPM bucket");

    let (status, body) = post_generation_json(
        &create_router(state),
        &api_key,
        "/v1/chat/completions",
        json!({
            "model": model,
            "messages": [{"role": "user", "content": "hello"}],
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "unexpected pre-admission response: {body}"
    );
    assert_rate_limit_error(&body);
    test_data_guard
        .cleanup()
        .await
        .expect("postgres rate test teardown should succeed");
}

#[tokio::test]
async fn postgres_account_rpm_limit_from_db_is_account_specific_for_same_triplet() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    let mut test_data_guard = TestDataGuard::new(pool.clone(), test_id.clone());
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("postgres rate test cleanup should succeed");

    let tenant = create_test_tenant(&pool, "db-rate-triplet", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "db-rate-triplet", &test_id).await;

    let api_key = ProduceAiKeyValidator::generate_key();
    let api_key_hash = ProduceAiKeyValidator::hash_key(&api_key);
    let api_key_row = keycompute_db::ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: format!("postgres-rate-limit-triplet-{test_id}"),
            produce_ai_key_hash: api_key_hash,
            produce_ai_key_preview: build_preview(&api_key),
            expires_at: None,
        },
    )
    .await
    .expect("test API key should be created");

    let pass_model = format!("gpt-triplet-pass-{test_id}");
    let block_model = format!("gpt-triplet-block-{test_id}");

    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "openai".to_string(),
            name: format!("db-triplet-pass-account-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-openai-key".to_string(),
            upstream_api_key_preview: "test-openai-key".to_string(),
            rpm_limit: Some(10_000),
            tpm_limit: Some(10_000),
            priority: Some(10),
            models_supported: vec![pass_model.clone()],
            api_capabilities: vec!["chat_completions".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test pass account should be created");

    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "openai".to_string(),
            name: format!("db-triplet-block-account-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-openai-key".to_string(),
            upstream_api_key_preview: "test-openai-key".to_string(),
            rpm_limit: Some(1),
            tpm_limit: Some(10_000),
            priority: Some(5),
            models_supported: vec![block_model.clone()],
            api_capabilities: vec!["chat_completions".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test block account should be created");

    let state = AppState::with_pool(DbRouter::single(pool.clone()));
    let app = create_router(state.clone());
    let request_key = RateLimitKey::new(tenant.id, user.id, api_key_row.id);
    let null_key = RateLimitKey::new(tenant.id, user.id, Uuid::nil());

    let before_count = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should be readable");

    let before_nil_count = state
        .rate_limiter
        .get_rpm_count(&null_key)
        .await
        .expect("rpm counter should be readable for nil key");

    let (pass_status, _pass_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/chat/completions",
        json!({
            "model": pass_model,
            "messages": [{"role": "user", "content": "hello"}],
        }),
    )
    .await;

    assert_ne!(pass_status, StatusCode::TOO_MANY_REQUESTS);

    let after_pass_count = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should be readable");
    assert_eq!(after_pass_count, before_count + 1);

    let (block_status, block_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/chat/completions",
        json!({
            "model": block_model,
            "messages": [{"role": "user", "content": "hello"}],
        }),
    )
    .await;

    assert_eq!(block_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&block_body);

    let after_block_count = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should still be readable");
    let after_nil_count = state
        .rate_limiter
        .get_rpm_count(&null_key)
        .await
        .expect("rpm counter should still be readable for nil key");

    assert_eq!(after_block_count, after_pass_count);
    assert_eq!(after_nil_count, before_nil_count);
    test_data_guard
        .cleanup()
        .await
        .expect("postgres rate test teardown should succeed");
}

#[tokio::test]
async fn postgres_account_rpm_limit_from_db_is_account_specific_for_same_triplet_messages() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    let mut test_data_guard = TestDataGuard::new(pool.clone(), test_id.clone());
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("postgres rate test cleanup should succeed");

    let tenant = create_test_tenant(&pool, "db-rate-rpm-triplet-messages", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "db-rate-rpm-triplet-messages", &test_id).await;

    let api_key = ProduceAiKeyValidator::generate_key();
    let api_key_hash = ProduceAiKeyValidator::hash_key(&api_key);
    let api_key_row = keycompute_db::ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: format!("postgres-rate-limit-rpm-messages-triplet-{test_id}"),
            produce_ai_key_hash: api_key_hash,
            produce_ai_key_preview: build_preview(&api_key),
            expires_at: None,
        },
    )
    .await
    .expect("test API key should be created");

    let pass_model = format!("claude-rpm-triplet-pass-messages-{test_id}");
    let block_model = format!("claude-rpm-triplet-block-messages-{test_id}");

    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "anthropic".to_string(),
            name: format!("db-rpm-messages-pass-account-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-anthropic-key".to_string(),
            upstream_api_key_preview: "test-anthropic-key".to_string(),
            rpm_limit: Some(10_000),
            tpm_limit: Some(10_000),
            priority: Some(10),
            models_supported: vec![pass_model.clone()],
            api_capabilities: vec!["messages".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test pass messages account should be created");

    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "anthropic".to_string(),
            name: format!("db-rpm-messages-block-account-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-anthropic-key".to_string(),
            upstream_api_key_preview: "test-anthropic-key".to_string(),
            rpm_limit: Some(1),
            tpm_limit: Some(10_000),
            priority: Some(5),
            models_supported: vec![block_model.clone()],
            api_capabilities: vec!["messages".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test block messages account should be created");

    let state = AppState::with_pool(DbRouter::single(pool.clone()));
    let app = create_router(state.clone());
    let request_key = RateLimitKey::new(tenant.id, user.id, api_key_row.id);
    let null_key = RateLimitKey::new(tenant.id, user.id, Uuid::nil());

    let before_count = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should be readable");
    let before_nil_count = state
        .rate_limiter
        .get_rpm_count(&null_key)
        .await
        .expect("rpm counter should be readable for nil key");

    let (pass_status, _pass_body) = post_generation_json_with_headers(
        &app,
        &api_key,
        "/v1/messages",
        json!({
            "model": pass_model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hello"}],
        }),
        &[("anthropic-version", "2023-06-01")],
    )
    .await;

    assert_ne!(pass_status, StatusCode::TOO_MANY_REQUESTS);

    let after_pass_count = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should be readable");
    assert_eq!(after_pass_count, before_count + 1);

    let (block_status, block_body) = post_generation_json_with_headers(
        &app,
        &api_key,
        "/v1/messages",
        json!({
            "model": block_model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hello"}],
        }),
        &[("anthropic-version", "2023-06-01")],
    )
    .await;

    assert_eq!(block_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&block_body);

    let after_block_count = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should still be readable");
    let after_nil_count = state
        .rate_limiter
        .get_rpm_count(&null_key)
        .await
        .expect("rpm counter should still be readable for nil key");

    assert_eq!(after_block_count, after_pass_count);
    assert_eq!(after_nil_count, before_nil_count);
    test_data_guard
        .cleanup()
        .await
        .expect("postgres rate test teardown should succeed");
}

#[tokio::test]
async fn postgres_account_rpm_limit_from_db_is_account_specific_for_same_triplet_responses() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    let mut test_data_guard = TestDataGuard::new(pool.clone(), test_id.clone());
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("postgres rate test cleanup should succeed");

    let tenant = create_test_tenant(&pool, "db-rate-rpm-triplet-responses", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "db-rate-rpm-triplet-responses", &test_id).await;

    let api_key = ProduceAiKeyValidator::generate_key();
    let api_key_hash = ProduceAiKeyValidator::hash_key(&api_key);
    let api_key_row = keycompute_db::ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: format!("postgres-rate-limit-rpm-responses-triplet-{test_id}"),
            produce_ai_key_hash: api_key_hash,
            produce_ai_key_preview: build_preview(&api_key),
            expires_at: None,
        },
    )
    .await
    .expect("test API key should be created");

    let pass_model = format!("gpt-rpm-triplet-pass-responses-{test_id}");
    let block_model = format!("gpt-rpm-triplet-block-responses-{test_id}");

    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "openai".to_string(),
            name: format!("db-rpm-responses-pass-account-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-openai-key".to_string(),
            upstream_api_key_preview: "test-openai-key".to_string(),
            rpm_limit: Some(10_000),
            tpm_limit: Some(10_000),
            priority: Some(10),
            models_supported: vec![pass_model.clone()],
            api_capabilities: vec!["responses".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test pass responses account should be created");

    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "openai".to_string(),
            name: format!("db-rpm-responses-block-account-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-openai-key".to_string(),
            upstream_api_key_preview: "test-openai-key".to_string(),
            rpm_limit: Some(1),
            tpm_limit: Some(10_000),
            priority: Some(5),
            models_supported: vec![block_model.clone()],
            api_capabilities: vec!["responses".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test block responses account should be created");

    let state = AppState::with_pool(DbRouter::single(pool.clone()));
    let app = create_router(state.clone());
    let request_key = RateLimitKey::new(tenant.id, user.id, api_key_row.id);
    let null_key = RateLimitKey::new(tenant.id, user.id, Uuid::nil());

    let before_count = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should be readable");
    let before_nil_count = state
        .rate_limiter
        .get_rpm_count(&null_key)
        .await
        .expect("rpm counter should be readable for nil key");

    let (pass_status, _pass_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/responses",
        json!({
            "model": pass_model,
            "input": "hello",
        }),
    )
    .await;

    assert_ne!(pass_status, StatusCode::TOO_MANY_REQUESTS);

    let after_pass_count = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should be readable");
    assert_eq!(after_pass_count, before_count + 1);

    let (block_status, block_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/responses",
        json!({
            "model": block_model,
            "input": "hello",
        }),
    )
    .await;

    assert_eq!(block_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&block_body);

    let (compact_status, compact_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/responses/compact",
        json!({
            "model": block_model,
            "input": "hello",
        }),
    )
    .await;

    assert_eq!(compact_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&compact_body);

    let after_block_count = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should still be readable");
    let after_nil_count = state
        .rate_limiter
        .get_rpm_count(&null_key)
        .await
        .expect("rpm counter should still be readable for nil key");

    assert_eq!(after_block_count, after_pass_count);
    assert_eq!(after_nil_count, before_nil_count);
    test_data_guard
        .cleanup()
        .await
        .expect("postgres rate test teardown should succeed");
}

#[tokio::test]
async fn postgres_account_tpm_limit_from_db_is_enforced_for_all_generation_endpoints() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    let mut test_data_guard = TestDataGuard::new(pool.clone(), test_id.clone());
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("postgres rate test cleanup should succeed");

    let tenant = create_test_tenant(&pool, "db-rate-tpm-all-routes", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "db-rate-tpm-all-routes", &test_id).await;

    let api_key = ProduceAiKeyValidator::generate_key();
    let api_key_hash = ProduceAiKeyValidator::hash_key(&api_key);
    let api_key_row = keycompute_db::ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: format!("postgres-rate-limit-all-tpm-routes-{test_id}"),
            produce_ai_key_hash: api_key_hash,
            produce_ai_key_preview: build_preview(&api_key),
            expires_at: None,
        },
    )
    .await
    .expect("test API key should be created");

    let chat_model = format!("gpt-all-tpm-chat-{test_id}");
    let anthropic_model = format!("claude-all-tpm-{test_id}");
    let responses_model = format!("gpt-all-tpm-responses-{test_id}");
    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "openai".to_string(),
            name: format!("db-all-tpm-openai-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-openai-key".to_string(),
            upstream_api_key_preview: "test-openai-key".to_string(),
            rpm_limit: Some(10_000),
            tpm_limit: Some(1),
            priority: Some(0),
            models_supported: vec![chat_model.clone(), responses_model.clone()],
            api_capabilities: vec!["chat_completions".to_string(), "responses".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test openai account should be created");

    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "anthropic".to_string(),
            name: format!("db-all-tpm-anthropic-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-anthropic-key".to_string(),
            upstream_api_key_preview: "test-anthropic-key".to_string(),
            rpm_limit: Some(10_000),
            tpm_limit: Some(1),
            priority: Some(0),
            models_supported: vec![anthropic_model.clone()],
            api_capabilities: vec!["messages".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test anthropic account should be created");

    let state = AppState::with_pool(DbRouter::single(pool.clone()));
    let app = create_router(state.clone());
    let request_key = RateLimitKey::new(tenant.id, user.id, api_key_row.id);
    let null_key = RateLimitKey::new(tenant.id, user.id, Uuid::nil());

    let before_rpm = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should be readable");
    let before_tpm = state
        .rate_limiter
        .get_tpm_count(&request_key)
        .await
        .expect("tpm counter should be readable");
    let before_nil_tpm = state
        .rate_limiter
        .get_tpm_count(&null_key)
        .await
        .expect("tpm counter should be readable for nil key");
    assert_eq!(before_tpm, 0);
    assert_eq!(before_nil_tpm, 0);

    let (chat_status, chat_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/chat/completions",
        json!({
            "model": chat_model,
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 1,
        }),
    )
    .await;
    assert_eq!(chat_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&chat_body);

    let (messages_status, messages_body) = post_generation_json_with_headers(
        &app,
        &api_key,
        "/v1/messages",
        json!({
            "model": anthropic_model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hello"}],
        }),
        &[("anthropic-version", "2023-06-01")],
    )
    .await;
    assert_eq!(messages_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&messages_body);

    let (responses_status, responses_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/responses",
        json!({
            "model": responses_model,
            "input": "hello",
        }),
    )
    .await;
    assert_eq!(responses_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&responses_body);

    let (compact_status, compact_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/responses/compact",
        json!({
            "model": responses_model,
            "input": "hello",
        }),
    )
    .await;
    assert_eq!(compact_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&compact_body);

    let after_tpm = state
        .rate_limiter
        .get_tpm_count(&request_key)
        .await
        .expect("tpm counter should still be readable");
    let after_nil_tpm = state
        .rate_limiter
        .get_tpm_count(&null_key)
        .await
        .expect("tpm counter should still be readable for nil key");
    let after_rpm = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should still be readable");

    assert_eq!(after_rpm, before_rpm, "TPM rejects must not consume RPM");
    assert_eq!(after_tpm, before_tpm);
    assert_eq!(after_nil_tpm, before_nil_tpm);

    assert_eq!(
        api_key_row.produce_ai_key_preview,
        build_preview(&api_key),
        "stored key preview should match test key prefix"
    );
    test_data_guard
        .cleanup()
        .await
        .expect("postgres rate test teardown should succeed");
}

#[tokio::test]
async fn postgres_account_tpm_limit_from_db_is_account_specific_for_same_triplet() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    let mut test_data_guard = TestDataGuard::new(pool.clone(), test_id.clone());
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("postgres rate test cleanup should succeed");

    let tenant = create_test_tenant(&pool, "db-rate-tpm-triplet", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "db-rate-tpm-triplet", &test_id).await;

    let api_key = ProduceAiKeyValidator::generate_key();
    let api_key_hash = ProduceAiKeyValidator::hash_key(&api_key);
    let api_key_row = keycompute_db::ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: format!("postgres-rate-limit-tpm-triplet-{test_id}"),
            produce_ai_key_hash: api_key_hash,
            produce_ai_key_preview: build_preview(&api_key),
            expires_at: None,
        },
    )
    .await
    .expect("test API key should be created");

    let pass_model = format!("gpt-tpm-triplet-pass-{test_id}");
    let block_model = format!("gpt-tpm-triplet-block-{test_id}");

    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "openai".to_string(),
            name: format!("db-tpm-triplet-pass-account-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-openai-key".to_string(),
            upstream_api_key_preview: "test-openai-key".to_string(),
            rpm_limit: Some(10_000),
            tpm_limit: Some(10_000),
            priority: Some(10),
            models_supported: vec![pass_model.clone()],
            api_capabilities: vec!["chat_completions".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test pass account should be created");

    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "openai".to_string(),
            name: format!("db-tpm-triplet-block-account-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-openai-key".to_string(),
            upstream_api_key_preview: "test-openai-key".to_string(),
            rpm_limit: Some(10_000),
            tpm_limit: Some(1),
            priority: Some(5),
            models_supported: vec![block_model.clone()],
            api_capabilities: vec!["chat_completions".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test block account should be created");

    let state = AppState::with_pool(DbRouter::single(pool.clone()));
    let app = create_router(state.clone());
    let request_key = RateLimitKey::new(tenant.id, user.id, api_key_row.id);
    let null_key = RateLimitKey::new(tenant.id, user.id, Uuid::nil());

    let before_tpm = state
        .rate_limiter
        .get_tpm_count(&request_key)
        .await
        .expect("tpm counter should be readable");
    let before_nil_tpm = state
        .rate_limiter
        .get_tpm_count(&null_key)
        .await
        .expect("tpm counter should be readable for nil key");

    let (pass_status, _pass_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/chat/completions",
        json!({
            "model": pass_model,
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 1,
        }),
    )
    .await;

    assert_ne!(pass_status, StatusCode::TOO_MANY_REQUESTS);

    let after_pass_tpm = state
        .rate_limiter
        .get_tpm_count(&request_key)
        .await
        .expect("tpm counter should be readable");
    assert!(after_pass_tpm >= before_tpm);

    let (block_status, block_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/chat/completions",
        json!({
            "model": block_model,
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 1,
        }),
    )
    .await;

    assert_eq!(block_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&block_body);

    let after_block_tpm = state
        .rate_limiter
        .get_tpm_count(&request_key)
        .await
        .expect("tpm counter should still be readable");
    let after_nil_tpm = state
        .rate_limiter
        .get_tpm_count(&null_key)
        .await
        .expect("tpm counter should still be readable for nil key");

    assert_eq!(after_block_tpm, after_pass_tpm);
    assert_eq!(after_nil_tpm, before_nil_tpm);
    test_data_guard
        .cleanup()
        .await
        .expect("postgres rate test teardown should succeed");
}

#[tokio::test]
async fn postgres_account_tpm_limit_from_db_is_account_specific_for_same_triplet_messages() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    let mut test_data_guard = TestDataGuard::new(pool.clone(), test_id.clone());
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("postgres rate test cleanup should succeed");

    let tenant = create_test_tenant(&pool, "db-rate-tpm-triplet-messages", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "db-rate-tpm-triplet-messages", &test_id).await;

    let api_key = ProduceAiKeyValidator::generate_key();
    let api_key_hash = ProduceAiKeyValidator::hash_key(&api_key);
    let api_key_row = keycompute_db::ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: format!("postgres-rate-limit-tpm-triplet-messages-{test_id}"),
            produce_ai_key_hash: api_key_hash,
            produce_ai_key_preview: build_preview(&api_key),
            expires_at: None,
        },
    )
    .await
    .expect("test API key should be created");

    let pass_model = format!("claude-tpm-triplet-pass-messages-{test_id}");
    let block_model = format!("claude-tpm-triplet-block-messages-{test_id}");

    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "anthropic".to_string(),
            name: format!("db-tpm-triplet-pass-messages-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-anthropic-key".to_string(),
            upstream_api_key_preview: "test-anthropic-key".to_string(),
            rpm_limit: Some(10_000),
            tpm_limit: Some(10_000),
            priority: Some(10),
            models_supported: vec![pass_model.clone()],
            api_capabilities: vec!["messages".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test pass messages account should be created");

    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "anthropic".to_string(),
            name: format!("db-tpm-triplet-block-messages-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-anthropic-key".to_string(),
            upstream_api_key_preview: "test-anthropic-key".to_string(),
            rpm_limit: Some(10_000),
            tpm_limit: Some(1),
            priority: Some(5),
            models_supported: vec![block_model.clone()],
            api_capabilities: vec!["messages".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test block messages account should be created");

    let state = AppState::with_pool(DbRouter::single(pool.clone()));
    let app = create_router(state.clone());
    let request_key = RateLimitKey::new(tenant.id, user.id, api_key_row.id);
    let null_key = RateLimitKey::new(tenant.id, user.id, Uuid::nil());

    let before_tpm = state
        .rate_limiter
        .get_tpm_count(&request_key)
        .await
        .expect("tpm counter should be readable");
    let before_nil_tpm = state
        .rate_limiter
        .get_tpm_count(&null_key)
        .await
        .expect("tpm counter should be readable for nil key");

    let (pass_status, _pass_body) = post_generation_json_with_headers(
        &app,
        &api_key,
        "/v1/messages",
        json!({
            "model": pass_model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hello"}],
        }),
        &[("anthropic-version", "2023-06-01")],
    )
    .await;

    assert_ne!(pass_status, StatusCode::TOO_MANY_REQUESTS);

    let after_pass_tpm = state
        .rate_limiter
        .get_tpm_count(&request_key)
        .await
        .expect("tpm counter should be readable");
    assert!(after_pass_tpm >= before_tpm);

    let (block_status, block_body) = post_generation_json_with_headers(
        &app,
        &api_key,
        "/v1/messages",
        json!({
            "model": block_model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hello"}],
        }),
        &[("anthropic-version", "2023-06-01")],
    )
    .await;

    assert_eq!(block_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&block_body);

    let after_block_tpm = state
        .rate_limiter
        .get_tpm_count(&request_key)
        .await
        .expect("tpm counter should still be readable");
    let after_nil_tpm = state
        .rate_limiter
        .get_tpm_count(&null_key)
        .await
        .expect("tpm counter should still be readable for nil key");

    assert_eq!(after_block_tpm, after_pass_tpm);
    assert_eq!(after_nil_tpm, before_nil_tpm);
    test_data_guard
        .cleanup()
        .await
        .expect("postgres rate test teardown should succeed");
}

#[tokio::test]
async fn postgres_account_tpm_limit_from_db_is_account_specific_for_same_triplet_responses() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    let mut test_data_guard = TestDataGuard::new(pool.clone(), test_id.clone());
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("postgres rate test cleanup should succeed");

    let tenant = create_test_tenant(&pool, "db-rate-tpm-triplet-responses", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "db-rate-tpm-triplet-responses", &test_id).await;

    let api_key = ProduceAiKeyValidator::generate_key();
    let api_key_hash = ProduceAiKeyValidator::hash_key(&api_key);
    let api_key_row = keycompute_db::ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: format!("postgres-rate-limit-tpm-triplet-responses-{test_id}"),
            produce_ai_key_hash: api_key_hash,
            produce_ai_key_preview: build_preview(&api_key),
            expires_at: None,
        },
    )
    .await
    .expect("test API key should be created");

    let pass_model = format!("gpt-tpm-triplet-pass-responses-{test_id}");
    let block_model = format!("gpt-tpm-triplet-block-responses-{test_id}");

    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "openai".to_string(),
            name: format!("db-tpm-triplet-pass-responses-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-openai-key".to_string(),
            upstream_api_key_preview: "test-openai-key".to_string(),
            rpm_limit: Some(10_000),
            tpm_limit: Some(10_000),
            priority: Some(10),
            models_supported: vec![pass_model.clone()],
            api_capabilities: vec!["responses".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test pass responses account should be created");

    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "openai".to_string(),
            name: format!("db-tpm-triplet-block-responses-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-openai-key".to_string(),
            upstream_api_key_preview: "test-openai-key".to_string(),
            rpm_limit: Some(10_000),
            tpm_limit: Some(1),
            priority: Some(5),
            models_supported: vec![block_model.clone()],
            api_capabilities: vec!["responses".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test block responses account should be created");

    let state = AppState::with_pool(DbRouter::single(pool.clone()));
    let app = create_router(state.clone());
    let request_key = RateLimitKey::new(tenant.id, user.id, api_key_row.id);
    let null_key = RateLimitKey::new(tenant.id, user.id, Uuid::nil());

    let before_tpm = state
        .rate_limiter
        .get_tpm_count(&request_key)
        .await
        .expect("tpm counter should be readable");
    let before_nil_tpm = state
        .rate_limiter
        .get_tpm_count(&null_key)
        .await
        .expect("tpm counter should be readable for nil key");

    let (pass_status, _pass_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/responses",
        json!({
            "model": pass_model,
            "input": "hello",
            "max_output_tokens": 16,
        }),
    )
    .await;

    assert_ne!(pass_status, StatusCode::TOO_MANY_REQUESTS);

    let after_pass_tpm = state
        .rate_limiter
        .get_tpm_count(&request_key)
        .await
        .expect("tpm counter should be readable");
    assert!(after_pass_tpm >= before_tpm);

    let (block_status, block_body) = post_generation_json(
        &app,
        &api_key,
        "/v1/responses",
        json!({
            "model": block_model,
            "input": "hello",
            "max_output_tokens": 16,
        }),
    )
    .await;

    assert_eq!(block_status, StatusCode::TOO_MANY_REQUESTS);
    assert_rate_limit_error(&block_body);

    let after_block_tpm = state
        .rate_limiter
        .get_tpm_count(&request_key)
        .await
        .expect("tpm counter should still be readable");
    let after_nil_tpm = state
        .rate_limiter
        .get_tpm_count(&null_key)
        .await
        .expect("tpm counter should still be readable for nil key");

    assert_eq!(after_block_tpm, after_pass_tpm);
    assert_eq!(after_nil_tpm, before_nil_tpm);
    test_data_guard
        .cleanup()
        .await
        .expect("postgres rate test teardown should succeed");
}

fn build_preview(api_key: &str) -> String {
    api_key.chars().take(12).collect()
}

#[tokio::test]
async fn postgres_account_rpm_limit_from_db_is_enforced_for_request_key_triplet() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    let mut test_data_guard = TestDataGuard::new(pool.clone(), test_id.clone());
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("postgres rate test cleanup should succeed");

    let tenant = create_test_tenant(&pool, "db-rate-rpm", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "db-rate-rpm", &test_id).await;

    let api_key = ProduceAiKeyValidator::generate_key();
    let api_key_hash = ProduceAiKeyValidator::hash_key(&api_key);
    let api_key_row = keycompute_db::ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: format!("postgres-rate-limit-rpm-{test_id}"),
            produce_ai_key_hash: api_key_hash,
            produce_ai_key_preview: build_preview(&api_key),
            expires_at: None,
        },
    )
    .await
    .expect("test API key should be created");

    let model = format!("gpt-rpm-{test_id}");
    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "openai".to_string(),
            name: format!("db-rpm-account-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-openai-key".to_string(),
            upstream_api_key_preview: "test-openai-key".to_string(),
            rpm_limit: Some(1),
            tpm_limit: Some(10_000),
            priority: Some(0),
            models_supported: vec![model.clone()],
            api_capabilities: vec!["chat_completions".to_string(), "responses".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test account should be created");

    let state = AppState::with_pool(DbRouter::single(pool.clone()));
    let app = create_router(state.clone());
    let request_key = RateLimitKey::new(tenant.id, user.id, api_key_row.id);
    let null_key = RateLimitKey::new(tenant.id, user.id, Uuid::nil());

    state
        .rate_limiter
        .check_and_record_with_config(&request_key, &RateLimitConfig::new(1, 10000))
        .await
        .expect("request key should be pre-recorded at account rpm limit");

    let before_count = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should be readable");
    let before_nil_count = state
        .rate_limiter
        .get_rpm_count(&null_key)
        .await
        .expect("rpm counter should be readable for nil key");

    assert_eq!(before_count, 1, "pre-filled rpm count should be recorded");
    assert_eq!(
        before_nil_count, 0,
        "different produce_ai_key_id should be isolated"
    );

    let (status, body) = post_generation_json(
        &app,
        &api_key,
        "/v1/chat/completions",
        json!({
            "model": model,
            "messages": [{"role": "user", "content": "hello"}],
        }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "account-level rpm limit should reject request before provider call"
    );
    assert_eq!(
        body.pointer("/error/code")
            .and_then(serde_json::Value::as_str),
        Some("rate_limit_exceeded"),
        "rate limit reject should return canonical code"
    );

    let after_count = state
        .rate_limiter
        .get_rpm_count(&request_key)
        .await
        .expect("rpm counter should still be readable");
    let after_nil_count = state
        .rate_limiter
        .get_rpm_count(&null_key)
        .await
        .expect("rpm counter should still be readable for nil key");

    assert_eq!(
        after_count, before_count,
        "reject request should not increase rpm count"
    );
    assert_eq!(after_nil_count, 0, "nil key should remain isolated");
    test_data_guard
        .cleanup()
        .await
        .expect("postgres rate test teardown should succeed");
}

#[tokio::test]
async fn postgres_responses_rpm_rejection_releases_idempotency_claim_for_retry() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    let mut test_data_guard = TestDataGuard::new(pool.clone(), test_id.clone());
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("postgres rate test cleanup should succeed");

    let tenant = create_test_tenant(&pool, "db-rate-responses-cleanup", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "db-rate-responses-cleanup", &test_id).await;
    let api_key = ProduceAiKeyValidator::generate_key();
    let api_key_row = keycompute_db::ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: format!("postgres-rate-limit-responses-cleanup-{test_id}"),
            produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&api_key),
            produce_ai_key_preview: build_preview(&api_key),
            expires_at: None,
        },
    )
    .await
    .expect("test API key should be created");
    let model = format!("gpt-rpm-responses-cleanup-{test_id}");
    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "openai".to_string(),
            name: format!("db-rpm-responses-cleanup-account-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-openai-key".to_string(),
            upstream_api_key_preview: "test-openai-key".to_string(),
            rpm_limit: Some(1),
            tpm_limit: Some(10_000),
            priority: Some(0),
            models_supported: vec![model.clone()],
            api_capabilities: vec!["responses".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test account should be created");

    let state = AppState::with_pool(DbRouter::single(pool.clone()));
    let request_key = RateLimitKey::new(tenant.id, user.id, api_key_row.id);
    state
        .rate_limiter
        .check_and_record_with_config(&request_key, &RateLimitConfig::new(1, 10_000))
        .await
        .expect("request key should be pre-recorded at account rpm limit");
    let body = json!({
        "model": model,
        "input": "hello",
        "max_output_tokens": 16,
    });
    let (status, response) = post_generation_json_with_headers(
        &create_router(state.clone()),
        &api_key,
        "/v1/responses",
        body.clone(),
        &[("idempotency-key", "rpm-cleanup-retry")],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "unexpected pre-admission response: {response}"
    );
    assert_rate_limit_error(&response);

    // A fresh process-local limiter models the next request after the RPM
    // window is available. The same durable idempotency key must be claimable
    // immediately; otherwise the rejected request leaked an in-progress claim.
    let retry_state = AppState::with_pool(DbRouter::single(pool.clone()));
    let (retry_status, retry_body) = post_generation_json_with_headers(
        &create_router(retry_state),
        &api_key,
        "/v1/responses",
        body,
        &[("idempotency-key", "rpm-cleanup-retry")],
    )
    .await;
    assert_ne!(retry_status, StatusCode::CONFLICT);
    let retry_message = retry_body
        .pointer("/error/message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(!retry_message.contains("still executing"));
    test_data_guard
        .cleanup()
        .await
        .expect("postgres rate test teardown should succeed");
}

#[tokio::test]
async fn postgres_account_tpm_limit_from_db_is_enforced_for_triplet_key() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    let mut test_data_guard = TestDataGuard::new(pool.clone(), test_id.clone());
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("postgres rate test cleanup should succeed");

    let tenant = create_test_tenant(&pool, "db-rate-tpm", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "db-rate-tpm", &test_id).await;

    let api_key = ProduceAiKeyValidator::generate_key();
    let api_key_hash = ProduceAiKeyValidator::hash_key(&api_key);
    let api_key_row = keycompute_db::ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: format!("postgres-rate-limit-tpm-{test_id}"),
            produce_ai_key_hash: api_key_hash,
            produce_ai_key_preview: build_preview(&api_key),
            expires_at: None,
        },
    )
    .await
    .expect("test API key should be created");

    let model = format!("gpt-tpm-{test_id}");
    keycompute_db::Account::create(
        &pool,
        &CreateAccountRequest {
            tenant_id: tenant.id,
            provider: "openai".to_string(),
            name: format!("db-tpm-account-{test_id}"),
            endpoint: "http://127.0.0.1:1/v1".to_string(),
            upstream_api_key_encrypted: "test-openai-key".to_string(),
            upstream_api_key_preview: "test-openai-key".to_string(),
            rpm_limit: Some(10_000),
            tpm_limit: Some(1),
            priority: Some(0),
            models_supported: vec![model.clone()],
            api_capabilities: vec!["chat_completions".to_string(), "responses".to_string()],
            visibility: Some("tenant".to_string()),
        },
    )
    .await
    .expect("test account should be created");

    let state = AppState::with_pool(DbRouter::single(pool.clone()));
    let app = create_router(state.clone());
    let request_key = RateLimitKey::new(tenant.id, user.id, api_key_row.id);
    let null_key = RateLimitKey::new(tenant.id, user.id, Uuid::nil());

    let before_tpm = state
        .rate_limiter
        .get_tpm_count(&request_key)
        .await
        .expect("tpm counter should be readable");
    let before_nil_tpm = state
        .rate_limiter
        .get_tpm_count(&null_key)
        .await
        .expect("tpm counter should be readable for nil key");

    assert_eq!(before_tpm, 0, "no tpm reservation before request");
    assert_eq!(
        before_nil_tpm, 0,
        "different produce_ai_key_id should be isolated"
    );

    let (status, body) = post_generation_json(
        &app,
        &api_key,
        "/v1/chat/completions",
        json!({
            "model": model,
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 1,
        }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "account-level tpm limit should reject request before dispatch"
    );
    assert_eq!(
        body.pointer("/error/code")
            .and_then(serde_json::Value::as_str),
        Some("rate_limit_exceeded"),
        "tpm reject should return canonical code"
    );

    let after_tpm = state
        .rate_limiter
        .get_tpm_count(&request_key)
        .await
        .expect("tpm counter should still be readable");
    let after_nil_tpm = state
        .rate_limiter
        .get_tpm_count(&null_key)
        .await
        .expect("tpm counter should still be readable for nil key");

    assert_eq!(
        after_tpm, before_tpm,
        "reject request should not consume tpm"
    );
    assert_eq!(
        after_nil_tpm, before_nil_tpm,
        "nil key should remain isolated"
    );

    assert_eq!(
        api_key_row.produce_ai_key_preview,
        build_preview(&api_key),
        "stored key preview should match test key prefix"
    );
    test_data_guard
        .cleanup()
        .await
        .expect("postgres rate test teardown should succeed");
}
