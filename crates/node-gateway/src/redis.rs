//! Node Gateway Redis 模块
//!
//! 负责任务队列管理和结果通知

use deadpool_redis::{Connection, Pool, redis::AsyncCommands};
use keycompute_config::RedisConfig;
use keycompute_runtime::redis_store::RedisRuntimeStore;
use std::{sync::Arc, time::Duration};
use tokio::time::{Instant, timeout_at};
use tracing;
use uuid::Uuid;

const RESULT_NOTIFICATION_TTL_SECS: u64 = 5 * 60;
// Redis checks blocked-client timeouts on its event loop. Allow finite
// transport/scheduling grace, without turning a broken socket into an
// indefinite wait. The caller's outer request deadline still wins.
const BLOCKING_RESPONSE_GRACE: Duration = Duration::from_millis(250);

/// A pending BRPOP must never return to the pool on task cancellation or a
/// transport timeout: dropping only its future does not cancel the server-side
/// command. Detach and drop the last multiplexed connection handle instead,
/// which aborts redis-rs's canonical connection driver and closes the socket.
struct BlockingConnection {
    connection: Option<Connection>,
}

impl BlockingConnection {
    fn new(connection: Connection) -> Self {
        Self {
            connection: Some(connection),
        }
    }

    fn connection(&mut self) -> &mut Connection {
        self.connection
            .as_mut()
            .expect("blocking connection is owned")
    }

    fn recycle(mut self) {
        // Only a fully received successful reply makes recycling safe.
        drop(self.connection.take());
    }
}

impl Drop for BlockingConnection {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            drop(Connection::take(connection));
        }
    }
}

// Keep the existing Redis List representation for deployment compatibility,
// but make recovery publication idempotent. The script is atomic with BRPOP:
// it publishes one fresh occurrence after removing every stale queued copy
// (a blocked consumer may take that occurrence immediately).
const REPLACE_MODEL_QUEUE_ENTRY_SCRIPT: &str = r#"#!lua
local removed = redis.call('LREM', KEYS[1], 0, ARGV[1])
redis.call('LPUSH', KEYS[1], ARGV[1])
return removed
"#;

const PUSH_RESULT_NOTIFICATION_SCRIPT: &str = r#"#!lua
redis.call('DEL', KEYS[1])
redis.call('LPUSH', KEYS[1], ARGV[1])
redis.call('EXPIRE', KEYS[1], ARGV[2])
return 1
"#;

/// Node Gateway Redis 管理器
#[derive(Clone)]
pub struct NodeGatewayRedis {
    redis: Arc<RedisRuntimeStore>,
    poll_pool: Pool,
    result_pool: Pool,
    command_timeout: Duration,
}

impl NodeGatewayRedis {
    /// Share only short commands with the application. Construct two new
    /// pools (not clones of the command pool) for task claims and result waits.
    /// Both pools use the same configured Redis endpoint/database as producers.
    pub fn new(redis: Arc<RedisRuntimeStore>, config: &RedisConfig) -> anyhow::Result<Self> {
        let make_pool = |size| {
            RedisRuntimeStore::create_pool_with_role(
                &config.url,
                size,
                Duration::from_secs(config.connect_timeout_secs),
                Duration::from_millis(config.pool_wait_timeout_ms),
                Duration::from_millis(config.command_timeout_ms),
                keycompute_runtime::redis_roles::RedisConnectionRole::CriticalState,
            )
        };
        Ok(Self {
            redis,
            poll_pool: make_pool(config.node_poll_pool_size as usize)?,
            result_pool: make_pool(config.node_result_pool_size as usize)?,
            command_timeout: Duration::from_millis(config.command_timeout_ms),
        })
    }

    /// A caller's blocking budget includes pool acquisition; pool saturation
    /// therefore cannot silently extend a Node poll by another full BRPOP.
    async fn blocking_pop(
        &self,
        pool: &Pool,
        key: &str,
        blocking_timeout: Duration,
    ) -> anyhow::Result<Option<(String, String)>> {
        anyhow::ensure!(
            !blocking_timeout.is_zero(),
            "Redis blocking timeout must be positive"
        );
        let deadline = Instant::now()
            .checked_add(blocking_timeout)
            .ok_or_else(|| anyhow::anyhow!("Redis blocking timeout is out of range"))?;
        let connection = timeout_at(deadline, pool.get()).await??;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let response_deadline = deadline
            .checked_add(BLOCKING_RESPONSE_GRACE)
            .ok_or_else(|| anyhow::anyhow!("Redis blocking response timeout is out of range"))?;
        let response_timeout = response_deadline.saturating_duration_since(Instant::now());
        let mut connection = BlockingConnection::new(connection);
        connection
            .connection()
            .set_response_timeout(response_timeout);
        let result = timeout_at(
            response_deadline,
            connection.connection().brpop(key, remaining.as_secs_f64()),
        )
        .await?;
        if result.is_ok() {
            connection
                .connection()
                .set_response_timeout(self.command_timeout);
            connection.recycle();
        }
        Ok(result?)
    }

    fn model_queue_key(model: &str) -> String {
        format!("queue:node:model:{model}")
    }

    /// 推送任务到模型队列
    pub async fn push_to_model_queue(
        &self,
        model: &str,
        task_id: Uuid,
    ) -> Result<(), anyhow::Error> {
        let queue_key = Self::model_queue_key(model);
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
        self.pop_from_model_queue_with_timeout(model, Duration::from_secs(timeout_secs))
            .await
    }

    /// Claim with an exact remaining Node poll budget (including pool admission).
    pub async fn pop_from_model_queue_with_timeout(
        &self,
        model: &str,
        timeout: Duration,
    ) -> anyhow::Result<Option<Uuid>> {
        let queue_key = Self::model_queue_key(model);
        let result = self
            .blocking_pop(&self.poll_pool, &queue_key, timeout)
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
        let _: i64 = deadpool_redis::redis::cmd("EVAL")
            .arg(PUSH_RESULT_NOTIFICATION_SCRIPT)
            .arg(1)
            .arg(&result_key)
            .arg(status)
            .arg(RESULT_NOTIFICATION_TTL_SECS)
            .query_async(&mut conn)
            .await?;
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

        let result = self
            .blocking_pop(
                &self.result_pool,
                &result_key,
                Duration::from_secs(timeout_secs),
            )
            .await?;

        match result {
            Some((_, status)) => Ok(Some(status)),
            None => Ok(None),
        }
    }

    /// 补推 queued 任务到模型队列
    pub async fn repush_queued_task(
        &self,
        model: &str,
        task_id: Uuid,
    ) -> Result<(), anyhow::Error> {
        let queue_key = Self::model_queue_key(model);
        let mut conn = self.redis.pool().get().await?;
        let removed: i64 = deadpool_redis::redis::cmd("EVAL")
            .arg(REPLACE_MODEL_QUEUE_ENTRY_SCRIPT)
            .arg(1)
            .arg(&queue_key)
            .arg(task_id.to_string())
            .query_async(&mut conn)
            .await?;
        tracing::debug!(
            task_id = %task_id,
            queue = %queue_key,
            removed_duplicates = removed,
            "Republished task after removing stale queue entries"
        );
        Ok(())
    }

    /// Remove every queued copy of a task after it reaches a terminal state.
    pub async fn remove_from_model_queue(
        &self,
        model: &str,
        task_id: Uuid,
    ) -> Result<u64, anyhow::Error> {
        let queue_key = Self::model_queue_key(model);
        let mut conn = self.redis.pool().get().await?;
        let removed: u64 = conn.lrem(&queue_key, 0, task_id.to_string()).await?;
        if removed > 0 {
            tracing::debug!(
                task_id = %task_id,
                queue = %queue_key,
                removed,
                "Removed terminal task from model queue"
            );
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests;
