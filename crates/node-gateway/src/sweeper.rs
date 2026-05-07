//! Node Gateway Sweeper 模块
//!
//! 后台维护任务：节点 TTL 过期、任务过期、Redis 补推

use crate::config::NodeGatewayAppConfig;
use crate::redis::NodeGatewayRedis;
use keycompute_db::DbError;
use keycompute_db::models::node_task::*;
use sqlx::PgPool;
use tracing;

/// Node Gateway Sweeper
pub struct NodeGatewaySweeper {
    pool: PgPool,
    redis: NodeGatewayRedis,
    config: NodeGatewayAppConfig,
}

impl NodeGatewaySweeper {
    /// 创建新的 Sweeper
    pub fn new(pool: PgPool, redis: NodeGatewayRedis, config: NodeGatewayAppConfig) -> Self {
        Self {
            pool,
            redis,
            config,
        }
    }

    /// 运行一次 sweeper 周期
    pub async fn run_once(&self) -> Result<(), anyhow::Error> {
        tracing::debug!("Running node gateway sweeper");

        // 1. 将超时的 online 节点标记为 offline
        self.expire_offline_nodes().await?;

        // 2. 将过期任务标记为 expired
        let expired_tasks = self.expire_overdue_tasks().await?;

        // 3. 补推 queued 任务到 Redis
        self.repush_queued_tasks().await?;

        // 4. 通知等待方过期任务
        for task in &expired_tasks {
            if let Err(e) = self
                .redis
                .push_result_notification(task.id, "expired")
                .await
            {
                tracing::warn!(
                    "Failed to push expired notification for task {}: {}",
                    task.id,
                    e
                );
            }
        }

        Ok(())
    }

    /// 将超时的 online 节点标记为 offline
    async fn expire_offline_nodes(&self) -> Result<(), DbError> {
        let ttl = self.config.sweeper_heartbeat_ttl_secs as i64;

        let result = sqlx::query(
            r#"
            UPDATE nodes
            SET status = 'offline', updated_at = NOW()
            WHERE status = 'online'
              AND last_heartbeat_at < NOW() - ($1 || ' seconds')::interval
            "#,
        )
        .bind(ttl.to_string())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() > 0 {
            tracing::info!("Marked {} nodes as offline", result.rows_affected());
        }

        Ok(())
    }

    /// 将过期任务标记为 expired
    async fn expire_overdue_tasks(&self) -> Result<Vec<NodeTask>, DbError> {
        let expired_tasks = NodeTask::expire_overdue_tasks(&self.pool).await?;

        if !expired_tasks.is_empty() {
            tracing::info!("Marked {} tasks as expired", expired_tasks.len());
        }

        Ok(expired_tasks)
    }

    /// 补推 queued 任务到 Redis
    async fn repush_queued_tasks(&self) -> Result<(), DbError> {
        let repush_interval = self.config.sweeper_repush_interval_secs as i64;

        // 查询需要补推的任务
        let tasks_to_repush = sqlx::query_as::<_, NodeTask>(
            r#"
            SELECT * FROM node_tasks
            WHERE status = 'queued'
              AND deadline_at > NOW()
              AND queued_at < NOW() - ($1 || ' seconds')::interval
            "#,
        )
        .bind(repush_interval.to_string())
        .fetch_all(&self.pool)
        .await?;

        for task in &tasks_to_repush {
            if let Err(e) = self.redis.repush_queued_task(&task.model, task.id).await {
                tracing::warn!("Failed to repush task {} to queue: {}", task.id, e);
            }
        }

        if !tasks_to_repush.is_empty() {
            tracing::info!("Repushed {} queued tasks to Redis", tasks_to_repush.len());
        }

        Ok(())
    }
}
