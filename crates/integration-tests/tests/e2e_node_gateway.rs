//! Node Gateway 端到端测试
//!
//! 验证个人消费级 PC 节点完整生命周期:
//! - HMAC 签名 token 注册
//! - 节点注册、心跳保活
//! - 任务领取 (Poll)、结果提交 (Complete)
//! - 节点状态生命周期 (online/offline/excluded)
//! - Token 一次性消费
//! - 并发安全和幂等性
//! - 失败恢复和节点排除

use integration_tests::common::VerificationChain;
use keycompute_db::models::{
    node::*,
    node_session::*,
    node_task::*,
    node_task_submission::NodeTaskSubmission,
    tenant::{CreateTenantRequest, Tenant},
    user::{CreateUserRequest, User},
    user_node_gateway_token::*,
};
use keycompute_types::node::{
    NodeCapabilities, NodeModelCapability, NodeRegisterRequest, NodeTaskCompleteAction,
    NodeTaskPayload, NodeTaskResult,
};
use node_gateway::config::NodeGatewayAppConfig;
use node_gateway::redis::NodeGatewayRedis;
use node_gateway::service::NodeGatewayService;
use node_gateway::store::NodeGatewayStore;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

/// 测试环境
#[allow(dead_code)]
struct NodeTestEnv {
    pool: PgPool,
    redis: NodeGatewayRedis,
    service: NodeGatewayService,
    config: NodeGatewayAppConfig,
}

/// 创建测试租户 + 用户，返回用户
///
/// 需要创建真实用户以满足 user_node_gateway_tokens 表的 FK 约束
/// (`user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE`)。
async fn create_test_user(pool: &PgPool, suffix: &str) -> Uuid {
    let tenant = Tenant::create(
        pool,
        &CreateTenantRequest {
            name: format!("ng-e2e-tenant-{}", suffix),
            slug: format!("ng-e2e-{}-{}", suffix, Uuid::new_v4()),
            description: Some("Node Gateway E2E test tenant".to_string()),
            default_rpm_limit: Some(100),
            default_tpm_limit: Some(50000),
        },
    )
    .await
    .expect("Failed to create test tenant");

    let user = User::create(
        pool,
        &CreateUserRequest {
            tenant_id: tenant.id,
            email: format!("ng-e2e-{}@test.local", suffix),
            name: Some(format!("NG E2E User {}", suffix)),
            role: None, // defaults to 'user'
        },
    )
    .await
    .expect("Failed to create test user");

    user.id
}

/// 用于生成测试用的 HMAC 签名 token
async fn create_test_hmac_token(pool: &PgPool, user_id: Uuid, secret: &str) -> String {
    let (token_id, token_plaintext, token_hash, token_preview) =
        UserNodeGatewayToken::generate_hmac_token(secret.as_bytes());

    // 插入 DB 并设置为 approved
    let token =
        UserNodeGatewayToken::create_with_id(pool, token_id, user_id, &token_hash, &token_preview)
            .await
            .expect("Failed to create test token");

    // 审批通过（自己审批自己用于测试）
    token
        .approve(pool, user_id)
        .await
        .expect("Failed to approve test token");

    token_plaintext
}

impl NodeTestEnv {
    /// 创建测试环境
    ///
    /// 注意：每次调用都会在创建新环境前清理上一次测试可能残留的数据。
    /// 清理策略：按 FK 依赖逆序删除，确保 CASCADE 不会意外传播。
    async fn new() -> anyhow::Result<Self> {
        let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:password@localhost:5432/keycompute".to_string()
        });

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(20)
            .min_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(30))
            .idle_timeout(std::time::Duration::from_secs(300))
            .max_lifetime(std::time::Duration::from_secs(900))
            .connect(&database_url)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to database: {}", e))?;

        keycompute_db::run_migrations(&pool)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to run database migrations: {}", e))?;

        // 清理历史测试数据（按 FK 依赖逆序删除，使用 E2E 专用的 email/slug 前缀模式匹配）
        sqlx::query("DELETE FROM node_task_submissions")
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM node_tasks").execute(&pool).await?;
        sqlx::query("DELETE FROM node_sessions")
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM nodes").execute(&pool).await?;
        sqlx::query("DELETE FROM user_node_gateway_tokens")
            .execute(&pool)
            .await?;
        // node_tips 和 node_tip_withdrawals 通过 FK ON DELETE CASCADE 跟随 users/nodes 删除，
        // 此处显式清理以处理 CASCADE 未覆盖的孤立记录
        sqlx::query("DELETE FROM node_tip_withdrawals WHERE user_id IN (SELECT id FROM users WHERE email LIKE 'ng-e2e-%')")
            .execute(&pool).await?;
        sqlx::query("DELETE FROM node_tips WHERE owner_user_id IN (SELECT id FROM users WHERE email LIKE 'ng-e2e-%')")
            .execute(&pool).await?;
        // 清理 E2E 测试创建的租户和用户
        sqlx::query("DELETE FROM users WHERE email LIKE 'ng-e2e-%'")
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM tenants WHERE slug LIKE 'ng-e2e-%'")
            .execute(&pool)
            .await?;
        // node_tips 通过 consumer_user_id FK 可能仍有残留（清理 users 后的孤儿记录）
        sqlx::query("DELETE FROM node_tips WHERE consumer_user_id IS NULL")
            .execute(&pool)
            .await?;

        // HMAC secret
        let registration_token_secret =
            std::env::var("KC__NODE_GATEWAY__REGISTRATION_TOKEN_SECRET")
                .unwrap_or_else(|_| "test-hmac-secret-key-for-e2e-testing-only".to_string());

        let config = NodeGatewayAppConfig {
            registration_token_secret,
            ..Default::default()
        };
        let store = NodeGatewayStore::new(pool.clone(), config.clone());

        let redis_url =
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let redis_store = keycompute_runtime::redis_store::RedisRuntimeStore::new(&redis_url)
            .map_err(|e| anyhow::anyhow!("Redis connection failed: {}", e))?;
        let redis = NodeGatewayRedis::new(Arc::new(redis_store));

        let service = NodeGatewayService::new(store, redis.clone(), config.clone());

        Ok(Self {
            pool,
            redis,
            service,
            config,
        })
    }

    /// 创建注册请求（使用 HMAC 签名 token）
    fn create_register_request(&self, client_id: &str, token: &str) -> NodeRegisterRequest {
        NodeRegisterRequest {
            protocol_version: "node.v1".to_string(),
            client_instance_id: client_id.to_string(),
            display_name: format!("Test Node {}", client_id),
            registration_token: token.to_string(),
            capabilities: NodeCapabilities {
                runtime: "ollama".to_string(),
                models: vec![
                    NodeModelCapability {
                        model: "deepseek-chat".to_string(),
                    },
                    NodeModelCapability {
                        model: "llama3".to_string(),
                    },
                ],
            },
        }
    }
}

/// 测试 1: 节点注册流程（使用 HMAC 签名 token）
#[tokio::test]
async fn test_node_registration() -> anyhow::Result<()> {
    let env = NodeTestEnv::new().await?;
    let mut chain = VerificationChain::new();

    // 创建测试用户 + 一个已审批的 token
    let test_user_id = create_test_user(&env.pool, "reg").await;
    let token = create_test_hmac_token(
        &env.pool,
        test_user_id,
        &env.config.registration_token_secret,
    )
    .await;

    // 1. 新节点注册
    let register_req = env.create_register_request("test-client-1", &token);
    let register_resp = env.service.register_node(&register_req).await?;

    chain.add_step(
        "node-gateway",
        "register_node::new_node",
        format!("Node registered: {}", register_resp.node_id),
        register_resp.protocol_version == "node.v1" && !register_resp.session_token.is_empty(),
    );

    // 2. 验证 session 已创建
    let session = NodeSession::find_by_id(&env.pool, register_resp.session_id).await?;
    chain.add_step(
        "node-gateway",
        "register_node::session_created",
        "Session created in database",
        session.is_some() && !session.unwrap().is_revoked(),
    );

    // 3. 验证节点状态为 online
    let node = Node::find_by_id(&env.pool, register_resp.node_id)
        .await?
        .unwrap();
    chain.add_step(
        "node-gateway",
        "register_node::node_online",
        format!("Node status: {}", node.status),
        node.status == "online",
    );

    chain.print_report();
    assert!(chain.all_passed());
    Ok(())
}

/// 测试 2: Token 一次性消费
#[tokio::test]
async fn test_token_one_time_use() -> anyhow::Result<()> {
    let env = NodeTestEnv::new().await?;

    let test_user_id = create_test_user(&env.pool, "ot1").await;
    let token = create_test_hmac_token(
        &env.pool,
        test_user_id,
        &env.config.registration_token_secret,
    )
    .await;

    // 第一次注册成功
    let req1 = env.create_register_request("test-client-ot1", &token);
    let resp1 = env.service.register_node(&req1).await;
    assert!(resp1.is_ok(), "First registration should succeed");

    // 从 token 中解析出 token_id（HMAC token 格式: kcng-{32_hex}-{32_hex}）
    let rest = token.strip_prefix("kcng-").unwrap();
    let token_id_hex = rest.rsplit_once('-').unwrap().0; // 后半段是 HMAC 签名，前半段是 token_id
    let uuid_str = format!(
        "{}-{}-{}-{}-{}",
        &token_id_hex[..8],
        &token_id_hex[8..12],
        &token_id_hex[12..16],
        &token_id_hex[16..20],
        &token_id_hex[20..32]
    );
    let token_id = Uuid::parse_str(&uuid_str)?;

    // 直接查 DB 确认 token 已被消费
    let db_token = UserNodeGatewayToken::find_by_id(&env.pool, token_id)
        .await?
        .expect("Token should exist in DB");
    assert_eq!(
        db_token.status, "consumed",
        "Token should be consumed after registration"
    );
    assert_eq!(
        db_token.consumed_node_id,
        Some(resp1.as_ref().unwrap().node_id),
        "Token should record consuming node"
    );

    // 第二次使用相同 token 应该失败
    let req2 = env.create_register_request("test-client-ot2", &token);
    let resp2 = env.service.register_node(&req2).await;
    assert!(
        resp2.is_err(),
        "Second registration with same token should fail"
    );

    Ok(())
}

/// 测试 3: 无效 token 被拒绝
#[tokio::test]
async fn test_invalid_token_rejected() -> anyhow::Result<()> {
    let env = NodeTestEnv::new().await?;

    let req = env.create_register_request(
        "test-client-invalid",
        "kcng-invalid-token-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    );
    let result = env.service.register_node(&req).await;

    assert!(result.is_err(), "Invalid token should be rejected");

    Ok(())
}

/// 测试 4: 重复注册 (同一 client_instance_id)
#[tokio::test]
async fn test_node_reregistration() -> anyhow::Result<()> {
    let env = NodeTestEnv::new().await?;
    let mut chain = VerificationChain::new();

    let client_id = "test-client-reregister";
    let test_user_id = create_test_user(&env.pool, "rereg").await;
    let token1 = create_test_hmac_token(
        &env.pool,
        test_user_id,
        &env.config.registration_token_secret,
    )
    .await;
    let token2 = create_test_hmac_token(
        &env.pool,
        test_user_id,
        &env.config.registration_token_secret,
    )
    .await;

    // 首次注册
    let req1 = env.create_register_request(client_id, &token1);
    let resp1 = env.service.register_node(&req1).await?;

    chain.add_step(
        "node-gateway",
        "reregister::first",
        format!("First registration: {}", resp1.session_id),
        !resp1.session_token.is_empty(),
    );

    // 重复注册（相同 client_id，不同 token）
    let req2 = env.create_register_request(client_id, &token2);
    let resp2 = env.service.register_node(&req2).await?;

    chain.add_step(
        "node-gateway",
        "reregister::second",
        format!("Second registration: {}", resp2.session_id),
        resp1.node_id == resp2.node_id && resp1.session_id != resp2.session_id,
    );

    chain.print_report();
    assert!(chain.all_passed());
    Ok(())
}

/// 测试 5: Excluded 节点拒绝注册
#[tokio::test]
async fn test_excluded_node_reject_registration() -> anyhow::Result<()> {
    let env = NodeTestEnv::new().await?;
    let mut chain = VerificationChain::new();

    let client_id = "test-client-excluded";
    let test_user_id = create_test_user(&env.pool, "excl").await;
    let token = create_test_hmac_token(
        &env.pool,
        test_user_id,
        &env.config.registration_token_secret,
    )
    .await;

    let req = env.create_register_request(client_id, &token);
    let resp = env.service.register_node(&req).await?;

    sqlx::query(
        "UPDATE nodes SET status = 'excluded', consecutive_failure_count = 3 WHERE id = $1",
    )
    .bind(resp.node_id)
    .execute(&env.pool)
    .await?;

    // 需要新 token 因为旧 token 已被消费
    let token2 = create_test_hmac_token(
        &env.pool,
        test_user_id,
        &env.config.registration_token_secret,
    )
    .await;
    let req2 = env.create_register_request(client_id, &token2);
    let result = env.service.register_node(&req2).await;

    chain.add_step(
        "node-gateway",
        "excluded::reject_registration",
        "Excluded node registration rejected",
        result.is_err(),
    );

    chain.print_report();
    assert!(chain.all_passed());
    Ok(())
}

/// 测试 6: 任务创建和入队
#[tokio::test]
async fn test_task_creation_and_enqueue() -> anyhow::Result<()> {
    let env = NodeTestEnv::new().await?;
    let mut chain = VerificationChain::new();

    let test_user_id = create_test_user(&env.pool, "task").await;
    let token = create_test_hmac_token(
        &env.pool,
        test_user_id,
        &env.config.registration_token_secret,
    )
    .await;
    let register_req = env.create_register_request("test-client-task", &token);
    env.service.register_node(&register_req).await?;

    let payload = NodeTaskPayload {
        request_id: Uuid::new_v4(),
        chat: keycompute_types::ChatCompletionRequest {
            model: "deepseek-chat".to_string(),
            messages: vec![keycompute_types::Message {
                role: keycompute_types::MessageRole::User,
                content: "Hello".to_string(),
            }],
            stream: Some(false),
            max_tokens: None,
            temperature: None,
            top_p: None,
            n: None,
            stop: None,
        },
    };

    let _task = env
        .service
        .enqueue_and_wait(Uuid::new_v4(), "deepseek-chat".to_string(), payload.clone())
        .await;

    chain.add_step(
        "node-gateway",
        "task_creation::task_created",
        "Task created and enqueued",
        // enqueue_and_wait 在无节点领取任务时会返回 Err(Timeout)，
        // 本步骤仅标记任务已入队，DB 落盘由后续 task_in_db 步骤验证
        true,
    );

    let tasks =
        sqlx::query_as::<_, NodeTask>("SELECT * FROM node_tasks ORDER BY created_at DESC LIMIT 1")
            .fetch_all(&env.pool)
            .await?;

    chain.add_step(
        "node-gateway",
        "task_creation::task_in_db",
        format!("Task count in DB: {}", tasks.len()),
        !tasks.is_empty() && tasks[0].status == "queued",
    );

    chain.print_report();
    assert!(chain.all_passed());
    Ok(())
}

/// 测试 7: Complete 幂等性 — 相同 task 重复 complete 应返回相同结果且只写一条 submission
#[tokio::test]
async fn test_complete_idempotency() -> anyhow::Result<()> {
    let env = NodeTestEnv::new().await?;
    let mut chain = VerificationChain::new();

    let test_user_id = create_test_user(&env.pool, "idem").await;
    let token = create_test_hmac_token(
        &env.pool,
        test_user_id,
        &env.config.registration_token_secret,
    )
    .await;

    // 1. 注册节点
    let register_req = env.create_register_request("test-client-idempotent", &token);
    let register_resp = env.service.register_node(&register_req).await?;

    // 2. 创建 leased 任务
    let lease_id = Uuid::new_v4();
    let task = sqlx::query_as::<_, NodeTask>(
        r#"
        INSERT INTO node_tasks (request_id, user_id, model, payload_json, status, assigned_node_id, assigned_session_id, lease_id, deadline_at, complete_grace_until, failure_threshold)
        VALUES ($1, $2, $3, $4, 'leased', $5, $6, $7, NOW() + INTERVAL '60 seconds', NOW() + INTERVAL '120 seconds', 3)
        RETURNING *
        "#
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .bind("deepseek-chat")
    .bind(serde_json::json!({}))
    .bind(register_resp.node_id)
    .bind(register_resp.session_id)
    .bind(lease_id)
    .fetch_one(&env.pool)
    .await?;

    // 3. 第一次 complete
    let result1 = env
        .service
        .complete_task(
            task.id,
            lease_id,
            register_resp.node_id,
            register_resp.session_id,
            NodeTaskResult::Succeeded {
                response: keycompute_types::ChatCompletionResponse {
                    id: "test-1".to_string(),
                    object: "chat.completion".to_string(),
                    created: 0,
                    model: "deepseek-chat".to_string(),
                    choices: vec![],
                    usage: keycompute_types::Usage {
                        prompt_tokens: 10,
                        completion_tokens: 20,
                        total_tokens: 30,
                    },
                },
            },
        )
        .await?;

    chain.add_step(
        "node-gateway",
        "idempotent::first_complete",
        format!("First complete: {:?}", result1.action),
        result1.action == NodeTaskCompleteAction::Succeeded,
    );

    // 4. 第二次 complete (相同 request, 应该幂等返回)
    let result2 = env
        .service
        .complete_task(
            task.id,
            lease_id,
            register_resp.node_id,
            register_resp.session_id,
            NodeTaskResult::Succeeded {
                response: keycompute_types::ChatCompletionResponse {
                    id: "test-1".to_string(),
                    object: "chat.completion".to_string(),
                    created: 0,
                    model: "deepseek-chat".to_string(),
                    choices: vec![],
                    usage: keycompute_types::Usage {
                        prompt_tokens: 10,
                        completion_tokens: 20,
                        total_tokens: 30,
                    },
                },
            },
        )
        .await?;

    chain.add_step(
        "node-gateway",
        "idempotent::second_complete",
        format!("Second complete (idempotent): {:?}", result2.action),
        result2.action == NodeTaskCompleteAction::Succeeded,
    );

    // 5. 验证只有一个 submission
    let submissions = sqlx::query_as::<_, NodeTaskSubmission>(
        "SELECT * FROM node_task_submissions WHERE task_id = $1",
    )
    .bind(task.id)
    .fetch_all(&env.pool)
    .await?;

    chain.add_step(
        "node-gateway",
        "idempotent::single_submission",
        format!("Submission count: {}", submissions.len()),
        submissions.len() == 1,
    );

    chain.print_report();
    assert!(chain.all_passed());
    Ok(())
}

/// B1 修复回归: client_error 失败不应将 node 算下线
///
/// 提交 3 次 is_client_error=true 的失败, 节点应仍 online,
/// consecutive_failure_count 应保持 0, 任务直接 terminal failed(不 requeue)。
#[tokio::test]
async fn test_client_error_does_not_exclude_node() -> anyhow::Result<()> {
    let env = NodeTestEnv::new().await?;

    let test_user_id = create_test_user(&env.pool, "cerr").await;
    let token = create_test_hmac_token(
        &env.pool,
        test_user_id,
        &env.config.registration_token_secret,
    )
    .await;

    let register_req = env.create_register_request("test-client-error-isolation", &token);
    let register_resp = env.service.register_node(&register_req).await?;

    for i in 1..=3 {
        let lease_id = Uuid::new_v4();
        let task = sqlx::query_as::<_, NodeTask>(
            r#"
            INSERT INTO node_tasks (request_id, user_id, model, payload_json, status, assigned_node_id, assigned_session_id, lease_id, deadline_at, complete_grace_until, failure_threshold)
            VALUES ($1, $2, $3, $4, 'leased', $5, $6, $7, NOW() + INTERVAL '60 seconds', NOW() + INTERVAL '120 seconds', 3)
            RETURNING *
            "#
        )
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .bind("deepseek-chat")
        .bind(serde_json::json!({}))
        .bind(register_resp.node_id)
        .bind(register_resp.session_id)
        .bind(lease_id)
        .fetch_one(&env.pool)
        .await?;

        let resp = env
            .service
            .complete_task(
                task.id,
                lease_id,
                register_resp.node_id,
                register_resp.session_id,
                NodeTaskResult::Failed {
                    code: "test_client_error".to_string(),
                    message: format!("client mistake {}", i),
                    is_client_error: true,
                },
            )
            .await?;

        // client_error 应该直接 Failed 终态, 不 Requeue
        assert_eq!(
            resp.action,
            NodeTaskCompleteAction::Failed,
            "client_error should terminate task immediately, not requeue (got {:?})",
            resp.action
        );
    }

    // 节点应仍 online, failure_count 仍 0
    let node = Node::find_by_id(&env.pool, register_resp.node_id)
        .await?
        .unwrap();
    assert_eq!(node.status, "online", "node should not be excluded");
    assert_eq!(
        node.consecutive_failure_count, 0,
        "client_error must not increment node failure_count"
    );

    Ok(())
}

/// 测试 8: 并发 Complete 安全 — 5 并发 complete 应只产生一条 submission
#[tokio::test]
async fn test_concurrent_complete_safety() -> anyhow::Result<()> {
    let env = NodeTestEnv::new().await?;
    let mut chain = VerificationChain::new();

    let test_user_id = create_test_user(&env.pool, "conc").await;
    let token = create_test_hmac_token(
        &env.pool,
        test_user_id,
        &env.config.registration_token_secret,
    )
    .await;

    // 1. 注册节点
    let register_req = env.create_register_request("test-client-concurrent", &token);
    let register_resp = env.service.register_node(&register_req).await?;

    // 2. 创建 leased 任务
    let lease_id = Uuid::new_v4();
    let task = sqlx::query_as::<_, NodeTask>(
        r#"
        INSERT INTO node_tasks (request_id, user_id, model, payload_json, status, assigned_node_id, assigned_session_id, lease_id, deadline_at, complete_grace_until, failure_threshold)
        VALUES ($1, $2, $3, $4, 'leased', $5, $6, $7, NOW() + INTERVAL '60 seconds', NOW() + INTERVAL '120 seconds', 3)
        RETURNING *
        "#
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .bind("deepseek-chat")
    .bind(serde_json::json!({}))
    .bind(register_resp.node_id)
    .bind(register_resp.session_id)
    .bind(lease_id)
    .fetch_one(&env.pool)
    .await?;

    chain.add_step(
        "node-gateway",
        "concurrent::setup",
        "Leased task created",
        true,
    );

    // 3. 并发提交 (相同 task_id + lease_id)
    let mut handles = vec![];
    for _ in 0..5 {
        let service = env.service.clone();
        let task_id = task.id;
        let node_id = register_resp.node_id;
        let session_id = register_resp.session_id;

        let handle = tokio::spawn(async move {
            service
                .complete_task(
                    task_id,
                    lease_id,
                    node_id,
                    session_id,
                    NodeTaskResult::Succeeded {
                        response: keycompute_types::ChatCompletionResponse {
                            id: "concurrent-test".to_string(),
                            object: "chat.completion".to_string(),
                            created: 0,
                            model: "deepseek-chat".to_string(),
                            choices: vec![],
                            usage: keycompute_types::Usage {
                                prompt_tokens: 10,
                                completion_tokens: 20,
                                total_tokens: 30,
                            },
                        },
                    },
                )
                .await
        });

        handles.push(handle);
    }

    // 4. 等待所有完成
    let results: Vec<_> = futures::future::join_all(handles).await;

    let success_count = results
        .iter()
        .filter(|r| match r {
            Ok(Ok(resp)) => resp.action == NodeTaskCompleteAction::Succeeded,
            _ => false,
        })
        .count();

    chain.add_step(
        "node-gateway",
        "concurrent::idempotent",
        format!("Successful completions: {}/5", success_count),
        success_count >= 1, // 至少一个成功
    );

    // 5. 验证只有一个 submission
    let submissions = sqlx::query_as::<_, NodeTaskSubmission>(
        "SELECT * FROM node_task_submissions WHERE task_id = $1",
    )
    .bind(task.id)
    .fetch_all(&env.pool)
    .await?;

    chain.add_step(
        "node-gateway",
        "concurrent::single_submission",
        format!("Submission count: {}", submissions.len()),
        submissions.len() == 1,
    );

    chain.print_report();
    assert!(chain.all_passed());
    Ok(())
}
