//! Node Gateway Redis 模块
//!
//! 负责任务队列管理和结果通知

use deadpool_redis::redis::AsyncCommands;
use keycompute_runtime::redis_store::RedisRuntimeStore;
use std::sync::Arc;
use tracing;
use uuid::Uuid;

/// Node Gateway Redis 管理器
pub struct NodeGatewayRedis {
    redis: Arc<RedisRuntimeStore>,
}

impl NodeGatewayRedis {
    /// 创建新的 Redis 管理器
    pub fn new(redis: Arc<RedisRuntimeStore>) -> Self {
        Self { redis }
    }

    /// 推送任务到模型队列
    pub async fn push_to_model_queue(&self, model: &str, task_id: Uuid) -> Result<(), anyhow::Error> {
        let queue_key = format!("queue:node:model:{}", model);
        let mut conn = self.redis.pool().get().await?;
        let _: () = conn.lpush(&queue_key, &[task_id.to_string()]).await?;
        tracing::debug!("Pushed task {} to queue {}", task_id, queue_key);
        Ok(())
    }

    /// 从模型队列中阻塞弹出任务
    pub async fn pop_from_model_queue(
        &self,
        model: &str,
        timeout_secs: u64,
    ) -> Result<Option<Uuid>, anyhow::Error> {
        let queue_key = format!("queue:node:model:{}", model);
        
        let mut conn = self.redis.pool().get().await?;
        let result: Option<(String, String)> = conn
            .brpop(&[queue_key], timeout_secs as f64)
            .await?;

        match result {
            Some((_, task_id_str)) => {
                let task_id = Uuid::parse_str(&task_id_str)?;
                Ok(Some(task_id))
            }
            None => Ok(None),
        }
    }

    /// 推送任务结果通知
    pub async fn push_result_notification(
        &self,
        task_id: Uuid,
        status: &str,
    ) -> Result<(), anyhow::Error> {
        let result_key = format!("task:result:{}", task_id);
        let mut conn = self.redis.pool().get().await?;
        let _: () = conn.lpush(&result_key, &[status.to_string()]).await?;
        tracing::debug!(
            "Pushed result notification for task {}: {}",
            task_id,
            status
        );
        Ok(())
    }

    /// 等待任务结果通知
    pub async fn wait_for_result(
        &self,
        task_id: Uuid,
        timeout_secs: u64,
    ) -> Result<Option<String>, anyhow::Error> {
        let result_key = format!("task:result:{}", task_id);
        
        let mut conn = self.redis.pool().get().await?;
        let result: Option<(String, String)> = conn
            .brpop(&[result_key], timeout_secs as f64)
            .await?;

        match result {
            Some((_, status)) => Ok(Some(status)),
            None => Ok(None),
        }
    }

    /// 补推 queued 任务到模型队列
    pub async fn repush_queued_task(&self, model: &str, task_id: Uuid) -> Result<(), anyhow::Error> {
        self.push_to_model_queue(model, task_id).await
    }
}
