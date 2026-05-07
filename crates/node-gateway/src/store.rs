//! Node Gateway Store 模块
//!
//! 数据库操作层，封装所有节点相关的数据库操作。

use crate::config::NodeGatewayAppConfig;
use chrono::Utc;
use keycompute_db::DbError;
use keycompute_db::models::{node::*, node_session::*, node_task::*, node_task_submission::*};
use keycompute_types::node::*;
use serde_json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Node Gateway Store
#[derive(Clone)]
pub struct NodeGatewayStore {
    pool: PgPool,
    config: NodeGatewayAppConfig,
}

impl NodeGatewayStore {
    /// 创建新的 Store 实例
    pub fn new(pool: PgPool, config: NodeGatewayAppConfig) -> Self {
        Self { pool, config }
    }

    /// 获取 pool 引用
    pub fn pool(&self) -> &PgPool {
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
    pub async fn register_node(
        &self,
        req: &NodeRegisterRequest,
        owner_user_id: Uuid,
    ) -> Result<NodeRegisterResponse, DbError> {
        // 0. 校验 registration_token
        if req.registration_token != self.config.registration_token {
            return Err(DbError::Other("Invalid registration token".to_string()));
        }

        let now = Utc::now();

        // 1. 查找或创建节点
        let node = match Node::find_by_owner_and_client(
            &self.pool,
            owner_user_id,
            &req.client_instance_id,
        )
        .await?
        {
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
                // 创建新节点
                let create_req = CreateNodeRequest {
                    owner_user_id,
                    client_instance_id: req.client_instance_id.clone(),
                    display_name: req.display_name.clone(),
                    capabilities_json: serde_json::to_value(&req.capabilities)
                        .map_err(|e| DbError::Other(e.to_string()))?,
                };
                Node::create(&self.pool, &create_req).await?
            }
        };

        // 2. 在同一事务中:创建 session + 更新节点状态 + 更新心跳
        let mut tx = self.pool.begin().await?;

        let session_token = Uuid::new_v4().to_string();
        let session_token_hash = hash_token(&session_token);
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

        // 2.1 创建 session (在事务中)
        let session = sqlx::query_as::<_, NodeSession>(
            r#"
            INSERT INTO node_sessions (node_id, session_token_hash, expires_at, accepted_models_json)
            VALUES ($1, $2, $3, $4)
            RETURNING *
            "#
        )
        .bind(create_session_req.node_id)
        .bind(&create_session_req.session_token_hash)
        .bind(create_session_req.expires_at)
        .bind(&create_session_req.accepted_models_json)
        .fetch_one(&mut *tx)
        .await?;

        // 2.2 更新节点状态为 online (如果原来是 offline,在事务中)
        if node.status == NODE_STATUS_OFFLINE {
            sqlx::query(
                r#"
                UPDATE nodes
                SET status = 'online', updated_at = NOW()
                WHERE id = $1
                "#,
            )
            .bind(node.id)
            .execute(&mut *tx)
            .await?;
        }

        // 2.3 更新节点心跳时间 (在事务中)
        sqlx::query(
            r#"
            UPDATE nodes
            SET last_heartbeat_at = NOW(), updated_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(node.id)
        .execute(&mut *tx)
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
        let token_hash = hash_token(session_token);

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

    /// 处理心跳(在同一事务中完成所有操作)
    pub async fn heartbeat(
        &self,
        node_id: Uuid,
        session_id: Uuid,
        accepted_models: Vec<String>,
    ) -> Result<NodeHeartbeatResponse, DbError> {
        let mut tx = self.pool.begin().await?;
        let now = Utc::now();
        let expires_at = now + self.config.session_ttl();

        // 1. 获取节点和会话(FOR UPDATE)
        let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = $1 FOR UPDATE")
            .bind(node_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_e| DbError::not_found("Node", node_id.to_string()))?;

        let session = sqlx::query_as::<_, NodeSession>(
            "SELECT * FROM node_sessions WHERE id = $1 FOR UPDATE",
        )
        .bind(session_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_e| DbError::not_found("Session", session_id.to_string()))?;

        // 2. 校验请求体与认证结果一致
        if session.node_id != node_id {
            return Err(DbError::Other("Session node_id mismatch".to_string()));
        }

        // 3. 根据节点状态分支处理
        if node.is_excluded() {
            // excluded 节点:只更新会话可见性,不改变节点状态
            sqlx::query(
                r#"
                UPDATE node_sessions
                SET last_seen_at = NOW(), expires_at = $1
                WHERE id = $2
                "#,
            )
            .bind(expires_at)
            .bind(session_id)
            .execute(&mut *tx)
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
            sqlx::query(
                r#"
                UPDATE node_sessions
                SET accepted_models_json = $1, last_seen_at = NOW(), expires_at = $2
                WHERE id = $3
                "#,
            )
            .bind(
                &serde_json::to_value(&accepted_models)
                    .map_err(|e| DbError::Other(e.to_string()))?,
            )
            .bind(expires_at)
            .bind(session_id)
            .execute(&mut *tx)
            .await?;

            // 2) 更新节点状态为 online(如果原来是 offline)
            if node.status != NODE_STATUS_ONLINE {
                sqlx::query(
                    r#"
                    UPDATE nodes
                    SET status = 'online', updated_at = NOW()
                    WHERE id = $1
                    "#,
                )
                .bind(node_id)
                .execute(&mut *tx)
                .await?;
            }

            // 3) 更新节点心跳时间
            sqlx::query(
                r#"
                UPDATE nodes
                SET last_heartbeat_at = NOW(), updated_at = NOW()
                WHERE id = $1
                "#,
            )
            .bind(node_id)
            .execute(&mut *tx)
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
        let mut tx = self.pool.begin().await?;

        // 1. 查询任务（FOR UPDATE）
        let task =
            sqlx::query_as::<_, NodeTask>("SELECT * FROM node_tasks WHERE id = $1 FOR UPDATE")
                .bind(task_id)
                .fetch_one(&mut *tx)
                .await?;

        // 2. 查询已有 submission
        let existing_submission = sqlx::query_as::<_, NodeTaskSubmission>(
            "SELECT * FROM node_task_submissions WHERE task_id = $1 AND lease_id = $2",
        )
        .bind(task_id)
        .bind(lease_id)
        .fetch_optional(&mut *tx)
        .await?;

        // 3. 如果已有 submission,处理幂等逻辑
        if let Some(submission) = existing_submission {
            // 先检查 session 是否被撤销(AGENTS.md: 已撤销 session 一律拒绝,包括查询已有 submission)
            let session = NodeSession::find_by_id(&self.pool, authenticated_session_id).await?;
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
                NodeTaskSubmission::is_not_archived(&self.pool, task_id, lease_id).await?;

            if is_not_archived {
                // 未归档，检查 request_hash
                // 计算当前请求的 request_hash
                let current_request_hash = Self::compute_request_hash(task_id, lease_id, &result)?;

                if submission.request_hash == current_request_hash {
                    // request_hash 相同,直接返回已保存的 ACK
                    let action = parse_action(&submission.action)?;

                    // 查询节点状态(从事务中查询,保证读到最新数据)
                    let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = $1")
                        .bind(authenticated_node_id)
                        .fetch_optional(&mut *tx)
                        .await?
                        .ok_or_else(|| {
                            DbError::not_found("Node", authenticated_node_id.to_string())
                        })?;

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

                let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = $1")
                    .bind(authenticated_node_id)
                    .fetch_optional(&mut *tx)
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
        let session = sqlx::query_as::<_, NodeSession>("SELECT * FROM node_sessions WHERE id = $1")
            .bind(authenticated_session_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| DbError::not_found("Session", authenticated_session_id.to_string()))?;

        if session.is_revoked() {
            return Err(DbError::Other("Session revoked".to_string()));
        }

        let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = $1")
            .bind(authenticated_node_id)
            .fetch_optional(&mut *tx)
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
                    response_json: serde_json::json!({}),
                };

                sqlx::query_as::<_, NodeTaskSubmission>(
                    r#"
                    INSERT INTO node_task_submissions (task_id, lease_id, node_id, session_id, result_kind, request_hash, action, response_json)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                    RETURNING *
                    "#,
                )
                .bind(submission_req.task_id)
                .bind(submission_req.lease_id)
                .bind(submission_req.node_id)
                .bind(submission_req.session_id)
                .bind(&submission_req.result_kind)
                .bind(&submission_req.request_hash)
                .bind(&submission_req.action)
                .bind(&submission_req.response_json)
                .fetch_one(&mut *tx)
                .await?;

                // 如果任务不是终态，标记为 expired
                if !task.is_terminal() {
                    sqlx::query_as::<_, NodeTask>(
                        r#"
                        UPDATE node_tasks
                        SET status = $1,
                            finished_at = NOW(),
                            updated_at = NOW()
                        WHERE id = $2
                        RETURNING *
                        "#,
                    )
                    .bind(TASK_STATUS_EXPIRED)
                    .bind(task_id)
                    .fetch_one(&mut *tx)
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
                    &mut tx,
                    &task,
                    &node,
                    authenticated_node_id,
                    authenticated_session_id,
                    lease_id,
                    response,
                )
                .await
            }
            NodeTaskResult::Failed { code, message } => {
                self.handle_failed_submission(
                    &mut tx,
                    &task,
                    &node,
                    authenticated_node_id,
                    authenticated_session_id,
                    lease_id,
                    code,
                    message,
                )
                .await
            }
        }?;

        // 9. 提交事务
        tx.commit().await?;

        Ok(response)
    }

    /// 处理成功提交
    #[allow(clippy::too_many_arguments)]
    async fn handle_success_submission(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        task: &NodeTask,
        node: &Node,
        node_id: Uuid,
        session_id: Uuid,
        lease_id: Uuid,
        response: keycompute_types::ChatCompletionResponse,
    ) -> Result<NodeTaskCompleteResponse, DbError> {
        // 1. 更新任务状态为 succeeded (包含完整的 WHERE 条件保证并发安全)
        let response_json =
            serde_json::to_value(&response).map_err(|e| DbError::Other(e.to_string()))?;
        let updated_task = sqlx::query_as::<_, NodeTask>(
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
        )
        .bind(TASK_STATUS_SUCCEEDED)
        .bind(&response_json)
        .bind(task.id)
        .bind(node_id)
        .bind(session_id)
        .bind(lease_id)
        .fetch_optional(&mut **tx)
        .await?;

        // 并发安全检查: 如果 UPDATE 返回 0 行,说明任务可能已被 sweeper 标记为 expired
        let updated_task = match updated_task {
            Some(t) => t,
            None => {
                // 查询任务当前状态,判断是否已过期
                let current_task =
                    sqlx::query_as::<_, NodeTask>("SELECT * FROM node_tasks WHERE id = $1")
                        .bind(task.id)
                        .fetch_optional(&mut **tx)
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

        // 2. 清零节点连续失败计数（仅非 excluded 节点）
        if !node.is_excluded() {
            sqlx::query(
                r#"
                UPDATE nodes
                SET consecutive_failure_count = 0, updated_at = NOW()
                WHERE id = $1
                "#,
            )
            .bind(node_id)
            .execute(&mut **tx)
            .await?;
        }

        // 3. 写入 submission ACK
        let result_for_hash = NodeTaskResult::Succeeded { response };
        let request_hash = Self::compute_request_hash(task.id, lease_id, &result_for_hash)?;

        let submission_req = CreateNodeTaskSubmissionRequest {
            task_id: task.id,
            lease_id,
            node_id,
            session_id,
            result_kind: "succeeded".to_string(),
            request_hash,
            action: "succeeded".to_string(),
            response_json,
        };

        sqlx::query_as::<_, NodeTaskSubmission>(
            r#"
            INSERT INTO node_task_submissions (task_id, lease_id, node_id, session_id, result_kind, request_hash, action, response_json)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING *
            "#,
        )
        .bind(submission_req.task_id)
        .bind(submission_req.lease_id)
        .bind(submission_req.node_id)
        .bind(submission_req.session_id)
        .bind(&submission_req.result_kind)
        .bind(&submission_req.request_hash)
        .bind(&submission_req.action)
        .bind(&submission_req.response_json)
        .fetch_one(&mut **tx)
        .await?;

        // 不在这里 commit,由调用方 commit

        // 查询最新节点状态(从事务中查询)
        let updated_node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = $1")
            .bind(node_id)
            .fetch_optional(&mut **tx)
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
        tx: &mut Transaction<'_, Postgres>,
        task: &NodeTask,
        _node: &Node,
        node_id: Uuid,
        session_id: Uuid,
        lease_id: Uuid,
        code: String,
        message: String,
    ) -> Result<NodeTaskCompleteResponse, DbError> {
        let error_json = serde_json::json!({ "code": code, "message": message });

        // 1. 更新任务状态（单条 SQL 包含 failure_count 自增和 CASE 逻辑）
        // 注意：使用 failure_count + 1 与 failure_threshold 比较，这是更新前的旧值加 1
        let updated_task = sqlx::query_as::<_, NodeTask>(
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
        )
        .bind(&error_json)
        .bind(task.id)
        .bind(node_id)
        .bind(session_id)
        .bind(lease_id)
        .fetch_optional(&mut **tx)
        .await?;

        // 并发安全检查: 如果 UPDATE 返回 0 行,说明任务可能已被 sweeper 标记为 expired
        let updated_task = match updated_task {
            Some(t) => t,
            None => {
                // 查询任务当前状态,判断是否已过期
                let current_task =
                    sqlx::query_as::<_, NodeTask>("SELECT * FROM node_tasks WHERE id = $1")
                        .bind(task.id)
                        .fetch_optional(&mut **tx)
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

        // 2. 增加节点连续失败计数并检查排除
        sqlx::query(
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
        )
        .bind(node_id)
        .execute(&mut **tx)
        .await?;

        // 3. 写入 submission ACK (根据 updated_task.status 判断 action)
        let action = if updated_task.status == TASK_STATUS_QUEUED {
            "requeued"
        } else {
            "failed"
        };

        let result_for_hash = NodeTaskResult::Failed {
            code: code.clone(),
            message: message.clone(),
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
            response_json: error_json.clone(),
        };

        sqlx::query_as::<_, NodeTaskSubmission>(
            r#"
            INSERT INTO node_task_submissions (task_id, lease_id, node_id, session_id, result_kind, request_hash, action, response_json)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING *
            "#,
        )
        .bind(submission_req.task_id)
        .bind(submission_req.lease_id)
        .bind(submission_req.node_id)
        .bind(submission_req.session_id)
        .bind(&submission_req.result_kind)
        .bind(&submission_req.request_hash)
        .bind(&submission_req.action)
        .bind(&submission_req.response_json)
        .fetch_one(&mut **tx)
        .await?;

        // 不在这里 commit,由调用方 commit

        // 查询最新节点状态(从事务中查询)
        let updated_node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = $1")
            .bind(node_id)
            .fetch_optional(&mut **tx)
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

/// 计算 token 的 SHA-256 hash
fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)
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
