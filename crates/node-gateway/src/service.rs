//! Node Gateway Service 模块
//!
//! 业务逻辑层，提供 enqueue_and_wait 等核心接口

use crate::config::NodeGatewayAppConfig;
use crate::redis::NodeGatewayRedis;
use crate::store::NodeGatewayStore;
use keycompute_db::DbError;
use keycompute_db::models::node_task::*;
use keycompute_types::ChatCompletionResponse;
use keycompute_types::node::*;
use std::time::Duration;
use tracing;
use uuid::Uuid;

/// Node Gateway Service
#[derive(Clone)]
pub struct NodeGatewayService {
    pub store: NodeGatewayStore,
    redis: NodeGatewayRedis,
    config: NodeGatewayAppConfig,
}

impl NodeGatewayService {
    /// 创建新的 Service 实例
    pub fn new(
        store: NodeGatewayStore,
        redis: NodeGatewayRedis,
        config: NodeGatewayAppConfig,
    ) -> Self {
        Self {
            store,
            redis,
            config,
        }
    }

    /// 入队并等待任务完成（核心接口）
    pub async fn enqueue_and_wait(
        &self,
        user_id: Uuid,
        model: String,
        payload: NodeTaskPayload,
    ) -> Result<ChatCompletionResponse, anyhow::Error> {
        let deadline_secs = self.config.task_deadline_secs;

        // 1. 创建任务并入队
        let task = self
            .store
            .create_and_enqueue_task(user_id, model.clone(), payload)
            .await?;

        // 2. 推送到 Redis 队列
        if let Err(e) = self.redis.push_to_model_queue(&model, task.id).await {
            tracing::warn!("Failed to push task {} to Redis queue: {}", task.id, e);
            // Redis 失败不影响，sweeper 会补推
        }

        // 3. 等待结果（使用 Redis 通知 + Postgres 轮询兜底）
        let wait_timeout = Duration::from_secs(deadline_secs);
        let result = tokio::time::timeout(wait_timeout, async {
            loop {
                // 3.1 尝试从 Redis 获取结果通知
                if let Ok(Some(status)) = self.redis.wait_for_result(task.id, 1).await {
                    // 3.2 从 Postgres 查询任务结果
                    return self.query_task_result(task.id, &status).await;
                }

                // 3.3 直接查询 Postgres（兜底）
                if let Ok(Some(task)) = NodeTask::find_by_id(self.store.pool(), task.id).await
                    && task.is_terminal()
                {
                    return self.query_task_result(task.id, &task.status).await;
                }

                // 短暂休眠后继续轮询
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;

        match result {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(e)) => Err(anyhow::anyhow!("Task failed: {}", e)),
            Err(_) => {
                // 超时
                Err(anyhow::anyhow!(
                    "Task {} timed out after {} seconds",
                    task.id,
                    deadline_secs
                ))
            }
        }
    }

    /// 查询任务结果
    async fn query_task_result(
        &self,
        task_id: Uuid,
        status: &str,
    ) -> Result<ChatCompletionResponse, anyhow::Error> {
        let task = NodeTask::find_by_id(self.store.pool(), task_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Task not found"))?;

        match status {
            "succeeded" => {
                let response: ChatCompletionResponse = serde_json::from_value(
                    task.result_json
                        .ok_or_else(|| anyhow::anyhow!("Task succeeded but no result_json"))?,
                )?;
                Ok(response)
            }
            "failed" => {
                let error = task.error_json.unwrap_or(serde_json::json!({}));
                Err(anyhow::anyhow!("Task failed: {}", error))
            }
            "expired" => Err(anyhow::anyhow!("Task expired")),
            _ => Err(anyhow::anyhow!("Unknown task status: {}", status)),
        }
    }

    /// 注册节点
    pub async fn register_node(
        &self,
        req: &NodeRegisterRequest,
        owner_user_id: Uuid,
    ) -> Result<NodeRegisterResponse, DbError> {
        self.store.register_node(req, owner_user_id).await
    }

    /// 心跳
    pub async fn heartbeat(
        &self,
        node_id: Uuid,
        session_id: Uuid,
        accepted_models: Vec<String>,
    ) -> Result<NodeHeartbeatResponse, DbError> {
        self.store
            .heartbeat(node_id, session_id, accepted_models)
            .await
    }

    /// 领取任务(长轮询)
    pub async fn poll_task(
        &self,
        node_id: Uuid,
        session_id: Uuid,
        accepted_models: Vec<String>,
    ) -> Result<NodePollResponse, anyhow::Error> {
        // 1. 检查节点状态
        let node = keycompute_db::models::node::Node::find_by_id(self.store.pool(), node_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Node not found"))?;

        // 如果节点状态不是 online,直接返回空 task (不参与 poll)
        if node.status != "online" {
            return Ok(NodePollResponse {
                protocol_version: "node.v1".to_string(),
                task: None,
                retry_after_ms: Some(5000),
            });
        }

        // 2. 检查 session 是否过期或撤销 (ready predicate)
        let session = keycompute_db::models::node_session::NodeSession::find_by_id(
            self.store.pool(),
            session_id,
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

        let now = chrono::Utc::now();
        if session.is_revoked() || session.expires_at < now {
            // session 已撤销或过期,不允许 poll
            return Ok(NodePollResponse {
                protocol_version: "node.v1".to_string(),
                task: None,
                retry_after_ms: Some(5000),
            });
        }

        // 3. 对每个 accepted_model 尝试 poll（所有模型共享同一个 poll_timeout）
        // 设计意图：防止多个模型队列依次等待导致总超时时间过长
        let poll_deadline = tokio::time::Instant::now() + self.config.poll_timeout();

        // 随机打乱模型顺序，避免固定顺序导致的队列饥饿问题
        let mut shuffled_models = accepted_models;
        fastrand::shuffle(&mut shuffled_models);

        for model in shuffled_models {
            // 检查是否已超过 poll 总超时时间
            let remaining = poll_deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break; // 超时，不再尝试更多模型
            }

            // 当前模型的等待时间：取剩余时间和单个模型最小等待时间（2秒）的较大值
            let model_timeout = remaining.as_secs().max(2);

            match self.redis.pop_from_model_queue(&model, model_timeout).await {
                Ok(Some(task_id)) => {
                    // 3. 原子 claim 任务
                    match self.store.claim_task(task_id, node_id, session_id).await? {
                        Some((_, envelope)) => {
                            return Ok(NodePollResponse {
                                protocol_version: "node.v1".to_string(),
                                task: Some(envelope),
                                retry_after_ms: None,
                            });
                        }
                        None => {
                            // claim 失败,任务已过期或被其他节点领取
                            continue;
                        }
                    }
                }
                Ok(None) => {
                    // 超时,尝试下一个模型
                    continue;
                }
                Err(e) => {
                    tracing::warn!("Failed to pop from queue for model {}: {}", model, e);
                    continue;
                }
            }
        }

        // 没有任务
        Ok(NodePollResponse {
            protocol_version: "node.v1".to_string(),
            task: None,
            retry_after_ms: Some(1000), // 建议 1 秒后重试
        })
    }

    /// 完成任务提交
    pub async fn complete_task(
        &self,
        task_id: Uuid,
        lease_id: Uuid,
        node_id: Uuid,
        session_id: Uuid,
        result: NodeTaskResult,
    ) -> Result<NodeTaskCompleteResponse, DbError> {
        let response = self
            .store
            .complete_task(task_id, lease_id, node_id, session_id, result)
            .await?;

        // 推送结果通知到 Redis(best-effort)
        match response.action {
            NodeTaskCompleteAction::Succeeded => {
                if let Err(e) = self
                    .redis
                    .push_result_notification(task_id, "succeeded")
                    .await
                {
                    tracing::warn!(
                        "Failed to push succeeded notification for task {}: {}",
                        task_id,
                        e
                    );
                }
            }
            NodeTaskCompleteAction::Requeued => {
                // requeued 需要重新推送到模型队列
                // 注意:这里需要获取 task 的 model,从任务中查询
                if let Ok(Some(t)) = keycompute_db::models::node_task::NodeTask::find_by_id(
                    self.store.pool(),
                    task_id,
                )
                .await
                    && let Err(e) = self.redis.push_to_model_queue(&t.model, task_id).await
                {
                    tracing::warn!("Failed to repush requeued task {} to queue: {}", task_id, e);
                }
            }
            NodeTaskCompleteAction::Failed => {
                if let Err(e) = self.redis.push_result_notification(task_id, "failed").await {
                    tracing::warn!(
                        "Failed to push failed notification for task {}: {}",
                        task_id,
                        e
                    );
                }
            }
            NodeTaskCompleteAction::Expired => {
                if let Err(e) = self
                    .redis
                    .push_result_notification(task_id, "expired")
                    .await
                {
                    tracing::warn!(
                        "Failed to push expired notification for task {}: {}",
                        task_id,
                        e
                    );
                }
            }
        }

        Ok(response)
    }
}
