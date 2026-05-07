//! Node Gateway Service 模块
//!
//! 业务逻辑层，提供 enqueue_and_wait 等核心接口

use crate::config::NodeGatewayAppConfig;
use crate::redis::NodeGatewayRedis;
use crate::store::NodeGatewayStore;
use keycompute_db::models::node_task::*;
use keycompute_db::DbError;
use keycompute_types::node::*;
use keycompute_types::ChatCompletionResponse;
use std::time::Duration;
use tracing;
use uuid::Uuid;

/// Node Gateway Service
pub struct NodeGatewayService {
    store: NodeGatewayStore,
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
        if let Err(e) = self
            .redis
            .push_to_model_queue(&model, task.id)
            .await
        {
            tracing::warn!(
                "Failed to push task {} to Redis queue: {}",
                task.id,
                e
            );
            // Redis 失败不影响，sweeper 会补推
        }

        // 3. 等待结果（使用 Redis 通知 + Postgres 轮询兜底）
        let wait_timeout = Duration::from_secs(deadline_secs);
        let result = tokio::time::timeout(wait_timeout, async {
            loop {
                // 3.1 尝试从 Redis 获取结果通知
                if let Ok(Some(status)) = self
                    .redis
                    .wait_for_result(task.id, 1)
                    .await
                {
                    // 3.2 从 Postgres 查询任务结果
                    return self.query_task_result(task.id, &status).await;
                }

                // 3.3 直接查询 Postgres（兜底）
                if let Ok(Some(task)) = NodeTask::find_by_id(self.store.pool(), task.id).await {
                    if task.is_terminal() {
                        return self.query_task_result(task.id, &task.status).await;
                    }
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
                let response: ChatCompletionResponse =
                    serde_json::from_value(task.result_json.ok_or_else(|| {
                        anyhow::anyhow!("Task succeeded but no result_json")
                    })?)?;
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
        self.store.heartbeat(node_id, session_id, accepted_models).await
    }

    /// 领取任务（长轮询）
    pub async fn poll_task(
        &self,
        node_id: Uuid,
        session_id: Uuid,
    ) -> Result<NodePollResponse, anyhow::Error> {
        // 1. 获取 session 和节点信息
        let (_, session) = self
            .store
            .authenticate_session(&format!("dummy-token")) // 实际应从认证中间件获取
            .await?;

        // 2. 从 session 获取 accepted_models
        let accepted_models: Vec<String> =
            serde_json::from_value(session.accepted_models_json.clone()).unwrap_or_default();

        // 3. 对每个 accepted_model 尝试 poll
        for model in accepted_models {
            match self
                .redis
                .pop_from_model_queue(&model, self.config.poll_timeout_secs)
                .await
            {
                Ok(Some(task_id)) => {
                    // 4. 原子 claim 任务
                    match self
                        .store
                        .claim_task(task_id, node_id, session_id)
                        .await?
                    {
                        Some((_, envelope)) => {
                            return Ok(NodePollResponse {
                                protocol_version: "node.v1".to_string(),
                                task: Some(envelope),
                                retry_after_ms: None,
                            });
                        }
                        None => {
                            // claim 失败，任务已过期或被其他节点领取
                            continue;
                        }
                    }
                }
                Ok(None) => {
                    // 超时，尝试下一个模型
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

        // 推送结果通知到 Redis（best-effort）
        let status = match response.action {
            NodeTaskCompleteAction::Succeeded => "succeeded",
            NodeTaskCompleteAction::Requeued => {
                // requeued 需要重新推送到模型队列
                // 注意：这里需要获取 task 的 model，暂时跳过
                "requeued"
            }
            NodeTaskCompleteAction::Failed => "failed",
            NodeTaskCompleteAction::Expired => "expired",
        };

        if status != "requeued" {
            if let Err(e) = self
                .redis
                .push_result_notification(task_id, status)
                .await
            {
                tracing::warn!(
                    "Failed to push result notification for task {}: {}",
                    task_id,
                    e
                );
            }
        }

        Ok(response)
    }
}
