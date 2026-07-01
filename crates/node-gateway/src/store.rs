//! Node Gateway Store 模块
//!
//! 数据库操作层，封装所有节点相关的数据库操作。

use crate::config::NodeGatewayAppConfig;
use chrono::Utc;
use keycompute_db::DbError;
use keycompute_db::models::{
    node::*, node_session::*, node_task::*, node_task_submission::*, user_node_gateway_token::*,
};
use keycompute_types::node::*;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, FromQueryResult,
    Statement, TransactionTrait,
};
use serde_json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Node Gateway Store
#[derive(Clone)]
pub struct NodeGatewayStore {
    pool: DatabaseConnection,
    config: NodeGatewayAppConfig,
}

impl NodeGatewayStore {
    /// 创建新的 Store 实例
    pub fn new(pool: DatabaseConnection, config: NodeGatewayAppConfig) -> Self {
        Self { pool, config }
    }

    /// 获取 pool 引用
    pub fn pool(&self) -> &DatabaseConnection {
        &self.pool
    }

    /// 计算 request_hash (canonical JSON hash)
    /// request_hash 只覆盖 task_id + lease_id + result
    fn compute_request_hash(
        task_id: Uuid,
        lease_id: Uuid,
        result: &NodeTaskResult,
    ) -> Result<String, DbError> {
        let hash_input = serde_json::json!({
            "task_id": task_id,
            "lease_id": lease_id,
            "result": result,
        });

        let canonical_json = serde_json::to_string(&hash_input)
            .map_err(|e| DbError::Other(format!("Failed to serialize hash input: {}", e)))?;

        let mut hasher = Sha256::new();
        hasher.update(canonical_json.as_bytes());
        let hash_bytes = hasher.finalize();

        // 转换为 hex 字符串
        Ok(format!("{:x}", hash_bytes))
    }

    /// 注册节点
    ///
    /// 认证策略：
    /// 1. HMAC 签名验证 → 解析 token_id
    /// 2. 查 DB 确认 token 状态为 `approved`
    /// 3. 在事务中原子消费 token（一次性）+ 创建节点
    ///
    /// 不再支持全局 fallback token。
    pub async fn register_node(
        &self,
        req: &NodeRegisterRequest,
    ) -> Result<NodeRegisterResponse, DbError> {
        // 0. HMAC 签名验证（O(1) 内存操作，零 DB 查询）
        let token_id = UserNodeGatewayToken::validate_hmac_token(
            &req.registration_token,
            self.config.registration_token_secret.as_bytes(),
        )
        .map_err(|e| DbError::Other(format!("Invalid registration token: {}", e)))?;

        let now = Utc::now();

        // 1. 开始事务（通过 FOR UPDATE 行级锁防止 TOCTOU，使用默认 READ COMMITTED 隔离级别）
        let tx = self.pool.begin().await?;

        // 2. 在事务内查询 token 并检查是否可消费（消除 TOCTOU 窗口）
        //    使用 FOR UPDATE 锁定行，防止并发修改
        let token = UserNodeGatewayToken::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT * FROM user_node_gateway_tokens
            WHERE id = $1
            FOR UPDATE
            "#,
            [token_id.into()],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| DbError::Other("Registration token not found".to_string()))?;

        if !token.is_consumable() {
            let status_msg = match token.status.as_str() {
                "consumed" =>
                    "Token has already been consumed by another node registration. Each token can only be used once."
                        .to_string(),
                "rejected" =>
                    "Token was rejected by admin. Please re-apply.".to_string(),
                _ => format!(
                    "Token is not approved (current status: {}). Please wait for admin approval.",
                    token.status
                ),
            };
            return Err(DbError::Other(status_msg));
        }

        let owner_user_id = token.user_id;

        // 3. 查找或创建节点(在事务中)
        let existing_node = Node::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT * FROM nodes
            WHERE owner_user_id = $1 AND client_instance_id = $2
            "#,
            [owner_user_id.into(), req.client_instance_id.as_str().into()],
        ))
        .one(&tx)
        .await?;

        let node = match existing_node {
            Some(existing_node) => {
                // 如果节点被排除，拒绝注册
                if existing_node.is_excluded() {
                    return Err(DbError::Other(
                        "Node is excluded, cannot re-register".to_string(),
                    ));
                }
                existing_node
            }
            None => {
                // 创建新节点(在事务中)
                let capabilities_json = serde_json::to_value(&req.capabilities)
                    .map_err(|e| DbError::Other(e.to_string()))?;

                Node::find_by_statement(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    r#"
                    INSERT INTO nodes (owner_user_id, client_instance_id, display_name, status, capabilities_json)
                    VALUES ($1, $2, $3, $4, $5)
                    RETURNING *
                    "#,
                    [
                        owner_user_id.into(),
                        req.client_instance_id.as_str().into(),
                        req.display_name.as_str().into(),
                        NODE_STATUS_OFFLINE.into(),
                        capabilities_json.clone().into(),
                    ],
                ))
                .one(&tx)
                .await?
                .ok_or_else(|| DbError::Other("Failed to create node".to_string()))?
            }
        };

        // 4. 在同一事务中：消费 token（一次性使用）+ 创建 session + 更新节点状态
        let consume_stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_node_gateway_tokens SET status = 'consumed', consumed_at = NOW(), consumed_node_id = $1, updated_at = NOW() WHERE id = $2 AND status = 'approved'"#,
            [node.id.into(), token_id.into()],
        );
        let consume_result = tx.execute(consume_stmt).await?;
        let consumed = consume_result.rows_affected() > 0;
        if !consumed {
            // Token 可能在事务外检查通过后、事务内 consume 之前被 admin reject 或并发消费
            return Err(DbError::Other(
                "Token is no longer valid (may have been rejected or consumed by another request). Please re-apply for a new token.".to_string(),
            ));
        }

        let session_token = Uuid::new_v4().to_string();
        let session_token_hash = UserNodeGatewayToken::hash_token(&session_token);
        let expires_at = now + self.config.session_ttl();

        // 提取注册能力中的模型名
        let accepted_models: Vec<String> = req
            .capabilities
            .models
            .iter()
            .map(|m| m.model.clone())
            .collect();

        let create_session_req = CreateNodeSessionRequest {
            node_id: node.id,
            session_token_hash,
            expires_at,
            accepted_models_json: serde_json::to_value(&accepted_models)
                .map_err(|e| DbError::Other(e.to_string()))?,
        };

        // 4.1 创建 session (在事务中)
        let session = NodeSession::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO node_sessions (node_id, session_token_hash, expires_at, accepted_models_json)
            VALUES ($1, $2, $3, $4)
            RETURNING *
            "#,
            [
                create_session_req.node_id.into(),
                create_session_req.session_token_hash.as_str().into(),
                create_session_req.expires_at.into(),
                create_session_req.accepted_models_json.clone().into(),
            ],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| DbError::Other("Failed to create session".to_string()))?;

        // 4.2 更新节点状态为 online (如果原来是 offline,在事务中)
        if node.status == NODE_STATUS_OFFLINE {
            tx.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"
                UPDATE nodes
                SET status = 'online', updated_at = NOW()
                WHERE id = $1
                "#,
                [node.id.into()],
            ))
            .await?;
        }

        // 4.3 更新节点心跳时间 (在事务中)
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE nodes
            SET last_heartbeat_at = NOW(), updated_at = NOW()
            WHERE id = $1
            "#,
            [node.id.into()],
        ))
        .await?;

        // 提交事务
        tx.commit().await?;

        Ok(NodeRegisterResponse {
            protocol_version: "node.v1".to_string(),
            node_id: node.id,
            session_id: session.id,
            session_token,
            heartbeat_interval_secs: self.config.heartbeat_interval_secs,
            poll_timeout_secs: self.config.poll_timeout_secs,
        })
    }

    /// 认证 session token
    pub async fn authenticate_session(
        &self,
        session_token: &str,
    ) -> Result<(Node, NodeSession), DbError> {
        let token_hash = UserNodeGatewayToken::hash_token(session_token);

        let session = NodeSession::find_by_token_hash(&self.pool, &token_hash).await?;

        match session {
            Some(s) => {
                // 检查 session 是否被撤销
                if s.is_revoked() {
                    return Err(DbError::Other("Session revoked".to_string()));
                }

                let node = Node::find_by_id(&self.pool, s.node_id)
                    .await?
                    .ok_or_else(|| DbError::not_found("Node", s.node_id.to_string()))?;

                Ok((node, s))
            }
            None => Err(DbError::not_found("Session", "token")),
        }
    }

    /// Admin 把 excluded 节点恢复为 online
    /// 同时清零 consecutive_failure_count, 节点可重新接收任务。
    pub async fn recover_node(&self, node_id: Uuid) -> Result<Node, DbError> {
        Node::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE nodes
            SET status = 'online',
                consecutive_failure_count = 0,
                updated_at = NOW()
            WHERE id = $1
            RETURNING *
            "#,
            [node_id.into()],
        ))
        .one(&self.pool)
        .await?
        .ok_or_else(|| DbError::not_found("Node", node_id.to_string()))
    }

    pub async fn heartbeat(
        &self,
        node_id: Uuid,
        session_id: Uuid,
        accepted_models: Vec<String>,
    ) -> Result<NodeHeartbeatResponse, DbError> {
        let tx = self.pool.begin().await?;
        let now = Utc::now();
        let expires_at = now + self.config.session_ttl();

        // 1. 获取节点和会话(FOR UPDATE)
        let node = Node::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM nodes WHERE id = $1 FOR UPDATE",
            [node_id.into()],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| DbError::not_found("Node", node_id.to_string()))?;

        let session = NodeSession::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_sessions WHERE id = $1 FOR UPDATE",
            [session_id.into()],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| DbError::not_found("Session", session_id.to_string()))?;

        // 2. 校验请求体与认证结果一致
        if session.node_id != node_id {
            return Err(DbError::Other("Session node_id mismatch".to_string()));
        }

        // 3. 根据节点状态分支处理
        if node.is_excluded() {
            // excluded 节点:只更新会话可见性,不改变节点状态
            tx.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"
                UPDATE node_sessions
                SET last_seen_at = NOW(), expires_at = $1
                WHERE id = $2
                "#,
                [expires_at.into(), session_id.into()],
            ))
            .await?;
        } else {
            // 非 excluded 节点:校验并持久化 accepted_models
            let capabilities: NodeCapabilities =
                serde_json::from_value(node.capabilities_json.clone())
                    .map_err(|e| DbError::Other(format!("Invalid capabilities_json: {}", e)))?;

            let registered_models: Vec<String> = capabilities
                .models
                .iter()
                .map(|m| m.model.clone())
                .collect();

            // 校验 accepted_models 是 registered_models 的子集
            for model in &accepted_models {
                if !registered_models.contains(model) {
                    return Err(DbError::Other(format!(
                        "Model {} is not in registered capabilities",
                        model
                    )));
                }
            }

            // 在同一事务中:
            // 1) 更新会话的 accepted_models 和可见性
            let accepted_models_value = serde_json::to_value(&accepted_models)
                .map_err(|e| DbError::Other(e.to_string()))?;
            tx.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"
                UPDATE node_sessions
                SET accepted_models_json = $1, last_seen_at = NOW(), expires_at = $2
                WHERE id = $3
                "#,
                [
                    accepted_models_value.into(),
                    expires_at.into(),
                    session_id.into(),
                ],
            ))
            .await?;

            // 2) 更新节点状态为 online(如果原来是 offline)
            if node.status != NODE_STATUS_ONLINE {
                tx.execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    r#"
                    UPDATE nodes
                    SET status = 'online', updated_at = NOW()
                    WHERE id = $1
                    "#,
                    [node_id.into()],
                ))
                .await?;
            }

            // 3) 更新节点心跳时间
            tx.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"
                UPDATE nodes
                SET last_heartbeat_at = NOW(), updated_at = NOW()
                WHERE id = $1
                "#,
                [node_id.into()],
            ))
            .await?;
        }

        // 提交事务
        tx.commit().await?;

        // 查询最新节点状态用于返回
        let updated_node = Node::find_by_id(&self.pool, node_id)
            .await?
            .ok_or_else(|| DbError::not_found("Node", node_id.to_string()))?;

        Ok(NodeHeartbeatResponse {
            protocol_version: "node.v1".to_string(),
            accepted: true,
            node_status: updated_node.status,
            server_failure_count: updated_node.consecutive_failure_count as u32,
            failure_threshold: updated_node.failure_threshold as u32,
        })
    }

    /// 创建任务并推入队列
    pub async fn create_and_enqueue_task(
        &self,
        user_id: Uuid,
        model: String,
        payload: NodeTaskPayload,
    ) -> Result<NodeTask, DbError> {
        let now = Utc::now();
        let deadline_at = now + self.config.task_deadline();
        let complete_grace_until = deadline_at + self.config.complete_grace();

        let create_req = CreateNodeTaskRequest {
            request_id: payload.request_id,
            user_id,
            model: model.clone(),
            payload_json: serde_json::to_value(&payload)
                .map_err(|e| DbError::Other(e.to_string()))?,
            deadline_at,
            complete_grace_until,
        };

        let task = NodeTask::create(&self.pool, &create_req).await?;

        // 注意：Redis 推送由上层调用方负责
        Ok(task)
    }

    /// 原子领取任务（claim）
    pub async fn claim_task(
        &self,
        task_id: Uuid,
        node_id: Uuid,
        session_id: Uuid,
    ) -> Result<Option<(NodeTask, NodeTaskEnvelope)>, DbError> {
        let lease_id = Uuid::new_v4();

        let task = NodeTask::claim(&self.pool, task_id, node_id, session_id, lease_id).await?;

        match task {
            Some(t) => {
                let payload: NodeTaskPayload = serde_json::from_value(t.payload_json.clone())
                    .map_err(|e| DbError::Other(format!("Invalid payload: {}", e)))?;

                let envelope = NodeTaskEnvelope {
                    task_id: t.id,
                    lease_id,
                    model: t.model.clone(),
                    deadline_unix_ms: t.deadline_at.timestamp_millis(),
                    complete_grace_until_unix_ms: t.complete_grace_until.timestamp_millis(),
                    payload,
                };

                Ok(Some((t, envelope)))
            }
            None => Ok(None),
        }
    }

    /// 完成任务提交（复杂的事务逻辑）
    pub async fn complete_task(
        &self,
        task_id: Uuid,
        lease_id: Uuid,
        authenticated_node_id: Uuid,
        authenticated_session_id: Uuid,
        result: NodeTaskResult,
    ) -> Result<NodeTaskCompleteResponse, DbError> {
        let tx = self.pool.begin().await?;

        // 1. 查询任务（FOR UPDATE）
        let task = NodeTask::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_tasks WHERE id = $1 FOR UPDATE",
            [task_id.into()],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| DbError::not_found("NodeTask", task_id.to_string()))?;

        // 2. 查询已有 submission
        let existing_submission =
            NodeTaskSubmission::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT * FROM node_task_submissions WHERE task_id = $1 AND lease_id = $2",
                [task_id.into(), lease_id.into()],
            ))
            .one(&tx)
            .await?;

        // 3. 如果已有 submission,处理幂等逻辑
        if let Some(submission) = existing_submission {
            // 先检查 session 是否被撤销
            let session = NodeSession::find_by_id(&tx, authenticated_session_id).await?;
            if session.map(|s| s.is_revoked()).unwrap_or(true) {
                return Err(DbError::Other("Session has been revoked".to_string()));
            }

            // 校验 session 身份
            if submission.node_id != authenticated_node_id
                || submission.session_id != authenticated_session_id
            {
                return Err(DbError::Other(
                    "duplicate_submission_session_mismatch".to_string(),
                ));
            }

            // 检查 submission 是否未归档（24 小时内且任务未终态）
            let is_not_archived =
                NodeTaskSubmission::is_not_archived(&tx, task_id, lease_id).await?;

            if is_not_archived {
                // 未归档，检查 request_hash
                let current_request_hash = Self::compute_request_hash(task_id, lease_id, &result)?;

                if submission.request_hash == current_request_hash {
                    // request_hash 相同,直接返回已保存的 ACK
                    let action = parse_action(&submission.action)?;

                    let node = Node::find_by_statement(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "SELECT * FROM nodes WHERE id = $1",
                        [authenticated_node_id.into()],
                    ))
                    .one(&tx)
                    .await?
                    .ok_or_else(|| DbError::not_found("Node", authenticated_node_id.to_string()))?;

                    return Ok(NodeTaskCompleteResponse {
                        action,
                        task_status: submission.action.clone(),
                        node_status: node.status,
                        server_failure_count: node.consecutive_failure_count as u32,
                        failure_threshold: node.failure_threshold as u32,
                    });
                } else {
                    // request_hash 不同，冲突
                    return Err(DbError::Other("duplicate_submission_conflict".to_string()));
                }
            } else {
                // 已归档,仍然返回已保存的 ACK (幂等)
                let action = parse_action(&submission.action)?;

                let node = Node::find_by_statement(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT * FROM nodes WHERE id = $1",
                    [authenticated_node_id.into()],
                ))
                .one(&tx)
                .await?
                .ok_or_else(|| DbError::not_found("Node", authenticated_node_id.to_string()))?;

                return Ok(NodeTaskCompleteResponse {
                    action,
                    task_status: submission.action.clone(),
                    node_status: node.status,
                    server_failure_count: node.consecutive_failure_count as u32,
                    failure_threshold: node.failure_threshold as u32,
                });
            }
        }

        // 4. 无 submission，检查 session 状态
        let session = NodeSession::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_sessions WHERE id = $1",
            [authenticated_session_id.into()],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| DbError::not_found("Session", authenticated_session_id.to_string()))?;

        if session.is_revoked() {
            return Err(DbError::Other("Session revoked".to_string()));
        }

        let node = Node::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM nodes WHERE id = $1",
            [authenticated_node_id.into()],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| DbError::not_found("Node", authenticated_node_id.to_string()))?;

        let session_expired = session.is_expired();
        let task_expired = task.is_expired();
        let now = Utc::now();

        // 5. 决策树：优先级 1 - Late Expired 判定
        if task_expired
            || (task.status == TASK_STATUS_LEASED && task.deadline_at < now)
            || (session_expired && !node.is_excluded())
        {
            // 条件 (a) 或 (b)：任务已过期
            if task.status == TASK_STATUS_EXPIRED
                || (task.status == TASK_STATUS_LEASED && task.deadline_at < now)
            {
                if now > task.complete_grace_until {
                    return Err(DbError::Other("grace_period_expired".to_string()));
                }

                // 在宽限期内，写入 expired submission
                let result_for_hash = NodeTaskResult::Failed {
                    code: "expired".to_string(),
                    message: "Task expired".to_string(),
                    is_client_error: false,
                };
                let request_hash = Self::compute_request_hash(task_id, lease_id, &result_for_hash)?;

                let submission_req = CreateNodeTaskSubmissionRequest {
                    task_id,
                    lease_id,
                    node_id: authenticated_node_id,
                    session_id: authenticated_session_id,
                    result_kind: "expired".to_string(),
                    request_hash,
                    action: "expired".to_string(),
                };

                NodeTaskSubmission::find_by_statement(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    r#"
                    INSERT INTO node_task_submissions (task_id, lease_id, node_id, session_id, result_kind, request_hash, action)
                    VALUES ($1, $2, $3, $4, $5, $6, $7)
                    RETURNING *
                    "#,
                    [
                        submission_req.task_id.into(),
                        submission_req.lease_id.into(),
                        submission_req.node_id.into(),
                        submission_req.session_id.into(),
                        submission_req.result_kind.as_str().into(),
                        submission_req.request_hash.as_str().into(),
                        submission_req.action.as_str().into(),
                    ],
                ))
                .one(&tx)
                .await?
                .ok_or_else(|| DbError::Other("Failed to insert expired submission".to_string()))?;

                // 如果任务不是终态，标记为 expired
                if !task.is_terminal() {
                    NodeTask::find_by_statement(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        r#"
                        UPDATE node_tasks
                        SET status = $1,
                            finished_at = NOW(),
                            updated_at = NOW()
                        WHERE id = $2
                        RETURNING *
                        "#,
                        [TASK_STATUS_EXPIRED.into(), task_id.into()],
                    ))
                    .one(&tx)
                    .await?;
                }

                tx.commit().await?;

                return Ok(NodeTaskCompleteResponse {
                    action: NodeTaskCompleteAction::Expired,
                    task_status: TASK_STATUS_EXPIRED.to_string(),
                    node_status: node.status,
                    server_failure_count: node.consecutive_failure_count as u32,
                    failure_threshold: node.failure_threshold as u32,
                });
            }
            // 条件 (c)：任务未过期但 session 过期，继续到优先级 2-4
        }

        // 6. 决策树：优先级 2 - 任务状态校验
        if task.status != TASK_STATUS_LEASED {
            return Err(DbError::Other("invalid_task_state".to_string()));
        }

        // 7. 决策树：优先级 3 - Lease 校验
        if task.assigned_node_id != Some(authenticated_node_id)
            || task.assigned_session_id != Some(authenticated_session_id)
            || task.lease_id != Some(lease_id)
        {
            return Err(DbError::Other("lease_mismatch".to_string()));
        }

        // 8. 决策树：优先级 4 - 正常成功/失败流程
        let response = match result {
            NodeTaskResult::Succeeded { response } => {
                self.handle_success_submission(
                    &tx,
                    &task,
                    &node,
                    authenticated_node_id,
                    authenticated_session_id,
                    lease_id,
                    response,
                )
                .await
            }
            NodeTaskResult::ImageSucceeded { image_response } => {
                self.handle_image_success_submission(
                    &tx,
                    &task,
                    &node,
                    authenticated_node_id,
                    authenticated_session_id,
                    lease_id,
                    image_response,
                )
                .await
            }
            NodeTaskResult::Failed {
                code,
                message,
                is_client_error,
            } => {
                self.handle_failed_submission(
                    &tx,
                    &task,
                    &node,
                    authenticated_node_id,
                    authenticated_session_id,
                    lease_id,
                    code,
                    message,
                    is_client_error,
                )
                .await
            }
        }?;

        // 9. 提交事务
        tx.commit().await?;

        Ok(response)
    }

    /// 处理成功提交（Chat 完成）
    #[allow(clippy::too_many_arguments)]
    async fn handle_success_submission(
        &self,
        tx: &DatabaseTransaction,
        task: &NodeTask,
        node: &Node,
        node_id: Uuid,
        session_id: Uuid,
        lease_id: Uuid,
        response: keycompute_types::ChatCompletionResponse,
    ) -> Result<NodeTaskCompleteResponse, DbError> {
        let response_json =
            serde_json::to_value(&response).map_err(|e| DbError::Other(e.to_string()))?;
        let result_for_hash = NodeTaskResult::Succeeded { response };
        self.handle_success_submission_inner(
            tx,
            task,
            node,
            node_id,
            session_id,
            lease_id,
            response_json,
            "succeeded",
            result_for_hash,
        )
        .await
    }

    /// 处理图片成功提交
    ///
    /// 注意：`ImageGenerationResponse` 中的 `b64_json` 字段可能携带大量 base64 图片数据
    ///（单张可达数 MB），直接存入 `node_tasks.result_json` JSONB 列存在存储膨胀风险。
    /// TODO: 后续考虑将图片数据上传至对象存储（S3/MinIO），DB 仅保留 URL 引用。
    #[allow(clippy::too_many_arguments)]
    async fn handle_image_success_submission(
        &self,
        tx: &DatabaseTransaction,
        task: &NodeTask,
        node: &Node,
        node_id: Uuid,
        session_id: Uuid,
        lease_id: Uuid,
        image_response: keycompute_types::node::ImageGenerationResponse,
    ) -> Result<NodeTaskCompleteResponse, DbError> {
        // 对大体积图片响应记录告警日志
        let b64_total_chars: usize = image_response
            .data
            .iter()
            .filter_map(|d| d.b64_json.as_ref())
            .map(|s| s.len())
            .sum();
        if b64_total_chars > 512 * 1024 {
            tracing::warn!(
                task_id = %task.id,
                b64_json_chars = b64_total_chars,
                image_count = image_response.data.len(),
                "Image response b64_json exceeds 512KB (base64-encoded), ~384KB raw; may cause DB storage bloat"
            );
        }

        let response_json = serde_json::to_value(&image_response)
            .map_err(|e| DbError::Other(format!("Failed to serialize image response: {}", e)))?;
        let result_for_hash = NodeTaskResult::ImageSucceeded { image_response };
        self.handle_success_submission_inner(
            tx,
            task,
            node,
            node_id,
            session_id,
            lease_id,
            response_json,
            "image_succeeded",
            result_for_hash,
        )
        .await
    }

    /// 成功提交的公共逻辑：更新任务状态、清零失败计数、写入 submission ACK
    #[allow(clippy::too_many_arguments)]
    async fn handle_success_submission_inner(
        &self,
        tx: &DatabaseTransaction,
        task: &NodeTask,
        node: &Node,
        node_id: Uuid,
        session_id: Uuid,
        lease_id: Uuid,
        response_json: serde_json::Value,
        result_kind: &str,
        result_for_hash: NodeTaskResult,
    ) -> Result<NodeTaskCompleteResponse, DbError> {
        let updated_task = NodeTask::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE node_tasks
            SET status = $1,
                result_json = $2,
                finished_at = NOW(),
                updated_at = NOW()
            WHERE id = $3
              AND assigned_node_id = $4
              AND assigned_session_id = $5
              AND lease_id = $6
              AND status = 'leased'
              AND deadline_at >= NOW()
            RETURNING *
            "#,
            [
                TASK_STATUS_SUCCEEDED.into(),
                response_json.clone().into(),
                task.id.into(),
                node_id.into(),
                session_id.into(),
                lease_id.into(),
            ],
        ))
        .one(tx)
        .await?;

        let updated_task = match updated_task {
            Some(t) => t,
            None => {
                let current_task = NodeTask::find_by_statement(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT * FROM node_tasks WHERE id = $1",
                    [task.id.into()],
                ))
                .one(tx)
                .await?
                .ok_or_else(|| DbError::not_found("Task", task.id.to_string()))?;

                if current_task.status == TASK_STATUS_EXPIRED
                    || current_task.deadline_at < Utc::now()
                {
                    return Err(DbError::Other("task_expired_during_complete".to_string()));
                } else {
                    return Err(DbError::Other("concurrent_task_update_failed".to_string()));
                }
            }
        };

        // 清零节点连续失败计数（仅非 excluded 节点）
        if !node.is_excluded() {
            tx.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"
                UPDATE nodes
                SET consecutive_failure_count = 0, updated_at = NOW()
                WHERE id = $1
                "#,
                [node_id.into()],
            ))
            .await?;
        }

        // 写入 submission ACK
        let request_hash = Self::compute_request_hash(task.id, lease_id, &result_for_hash)?;

        let submission_req = CreateNodeTaskSubmissionRequest {
            task_id: task.id,
            lease_id,
            node_id,
            session_id,
            result_kind: result_kind.to_string(),
            request_hash,
            action: "succeeded".to_string(),
        };

        NodeTaskSubmission::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO node_task_submissions (task_id, lease_id, node_id, session_id, result_kind, request_hash, action)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING *
            "#,
            [
                submission_req.task_id.into(),
                submission_req.lease_id.into(),
                submission_req.node_id.into(),
                submission_req.session_id.into(),
                submission_req.result_kind.as_str().into(),
                submission_req.request_hash.as_str().into(),
                submission_req.action.as_str().into(),
            ],
        ))
        .one(tx)
        .await?
        .ok_or_else(|| DbError::Other("Failed to insert submission".to_string()))?;

        let updated_node = Node::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM nodes WHERE id = $1",
            [node_id.into()],
        ))
        .one(tx)
        .await?
        .ok_or_else(|| DbError::not_found("Node", node_id.to_string()))?;

        Ok(NodeTaskCompleteResponse {
            action: NodeTaskCompleteAction::Succeeded,
            task_status: updated_task.status,
            node_status: updated_node.status,
            server_failure_count: updated_node.consecutive_failure_count as u32,
            failure_threshold: updated_node.failure_threshold as u32,
        })
    }

    /// 处理失败提交
    #[allow(clippy::too_many_arguments)]
    async fn handle_failed_submission(
        &self,
        tx: &DatabaseTransaction,
        task: &NodeTask,
        _node: &Node,
        node_id: Uuid,
        session_id: Uuid,
        lease_id: Uuid,
        code: String,
        message: String,
        is_client_error: bool,
    ) -> Result<NodeTaskCompleteResponse, DbError> {
        let error_json = serde_json::json!({
            "code": code,
            "message": message,
            "is_client_error": is_client_error,
        });

        let updated_task = if is_client_error {
            NodeTask::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"
                UPDATE node_tasks
                SET status = 'failed',
                    failure_count = failure_count + 1,
                    error_json = $1,
                    updated_at = NOW()
                WHERE id = $2
                  AND assigned_node_id = $3
                  AND assigned_session_id = $4
                  AND lease_id = $5
                  AND status = 'leased'
                  AND deadline_at >= NOW()
                RETURNING *
                "#,
                [
                    error_json.clone().into(),
                    task.id.into(),
                    node_id.into(),
                    session_id.into(),
                    lease_id.into(),
                ],
            ))
            .one(tx)
            .await?
        } else {
            NodeTask::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"
                UPDATE node_tasks
                SET status = CASE
                    WHEN failure_count + 1 < failure_threshold THEN 'queued'
                    ELSE 'failed'
                  END,
                  failure_count = failure_count + 1,
                  assigned_node_id = CASE
                    WHEN failure_count + 1 < failure_threshold THEN NULL
                    ELSE assigned_node_id
                  END,
                  assigned_session_id = CASE
                    WHEN failure_count + 1 < failure_threshold THEN NULL
                    ELSE assigned_session_id
                  END,
                  lease_id = CASE
                    WHEN failure_count + 1 < failure_threshold THEN NULL
                    ELSE lease_id
                  END,
                  claimed_at = CASE
                    WHEN failure_count + 1 < failure_threshold THEN NULL
                    ELSE claimed_at
                  END,
                  error_json = CASE
                    WHEN failure_count + 1 >= failure_threshold THEN $1
                    ELSE error_json
                  END,
                  updated_at = NOW()
                WHERE id = $2
                  AND assigned_node_id = $3
                  AND assigned_session_id = $4
                  AND lease_id = $5
                  AND status = 'leased'
                  AND deadline_at >= NOW()
                RETURNING *
                "#,
                [
                    error_json.clone().into(),
                    task.id.into(),
                    node_id.into(),
                    session_id.into(),
                    lease_id.into(),
                ],
            ))
            .one(tx)
            .await?
        };

        let updated_task = match updated_task {
            Some(t) => t,
            None => {
                let current_task = NodeTask::find_by_statement(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT * FROM node_tasks WHERE id = $1",
                    [task.id.into()],
                ))
                .one(tx)
                .await?
                .ok_or_else(|| DbError::not_found("Task", task.id.to_string()))?;

                if current_task.status == TASK_STATUS_EXPIRED
                    || current_task.deadline_at < Utc::now()
                {
                    return Err(DbError::Other("task_expired_during_complete".to_string()));
                } else {
                    return Err(DbError::Other("concurrent_task_update_failed".to_string()));
                }
            }
        };

        // 增加节点连续失败计数并检查排除
        if !is_client_error {
            tx.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"
                UPDATE nodes
                SET consecutive_failure_count = consecutive_failure_count + 1,
                    status = CASE
                        WHEN consecutive_failure_count + 1 >= failure_threshold THEN 'excluded'
                        ELSE status
                    END,
                    updated_at = NOW()
                WHERE id = $1
                "#,
                [node_id.into()],
            ))
            .await?;
        }

        let action = if updated_task.status == TASK_STATUS_QUEUED {
            "requeued"
        } else {
            "failed"
        };

        let result_for_hash = NodeTaskResult::Failed {
            code: code.clone(),
            message: message.clone(),
            is_client_error,
        };
        let request_hash = Self::compute_request_hash(task.id, lease_id, &result_for_hash)?;

        let submission_req = CreateNodeTaskSubmissionRequest {
            task_id: task.id,
            lease_id,
            node_id,
            session_id,
            result_kind: "failed".to_string(),
            request_hash,
            action: action.to_string(),
        };

        NodeTaskSubmission::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO node_task_submissions (task_id, lease_id, node_id, session_id, result_kind, request_hash, action)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING *
            "#,
            [
                submission_req.task_id.into(),
                submission_req.lease_id.into(),
                submission_req.node_id.into(),
                submission_req.session_id.into(),
                submission_req.result_kind.as_str().into(),
                submission_req.request_hash.as_str().into(),
                submission_req.action.as_str().into(),
            ],
        ))
        .one(tx)
        .await?
        .ok_or_else(|| DbError::Other("Failed to insert submission".to_string()))?;

        let updated_node = Node::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM nodes WHERE id = $1",
            [node_id.into()],
        ))
        .one(tx)
        .await?
        .ok_or_else(|| DbError::not_found("Node", node_id.to_string()))?;

        let complete_action = if updated_task.status == TASK_STATUS_QUEUED {
            NodeTaskCompleteAction::Requeued
        } else {
            NodeTaskCompleteAction::Failed
        };

        Ok(NodeTaskCompleteResponse {
            action: complete_action,
            task_status: updated_task.status,
            node_status: updated_node.status,
            server_failure_count: updated_node.consecutive_failure_count as u32,
            failure_threshold: updated_node.failure_threshold as u32,
        })
    }
}

/// 解析 action 字符串
fn parse_action(action: &str) -> Result<NodeTaskCompleteAction, DbError> {
    match action {
        "succeeded" => Ok(NodeTaskCompleteAction::Succeeded),
        "requeued" => Ok(NodeTaskCompleteAction::Requeued),
        "failed" => Ok(NodeTaskCompleteAction::Failed),
        "expired" => Ok(NodeTaskCompleteAction::Expired),
        _ => Err(DbError::Other(format!("Unknown action: {}", action))),
    }
}
