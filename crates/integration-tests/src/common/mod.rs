//! 测试公共工具和辅助函数

use std::time::Duration;
use tokio::time::timeout;
use uuid::Uuid;

/// 测试超时设置
pub const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 测试用的 API Key
pub const TEST_API_KEY: &str = "sk-test-integration-12345";

/// 生成唯一测试 ID
pub fn generate_test_id() -> String {
    format!("test-{}", Uuid::new_v4().simple())
}

/// 测试上下文
#[derive(Debug, Clone)]
pub struct TestContext {
    pub test_id: String,
    pub request_id: Uuid,
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub produce_ai_key_id: Uuid,
}

impl TestContext {
    pub fn new() -> Self {
        Self {
            test_id: generate_test_id(),
            request_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            produce_ai_key_id: Uuid::new_v4(),
        }
    }
}

impl Default for TestContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Explicit test database; never silently use a developer's local database.
pub fn resolve_database_url() -> String {
    std::env::var("DATABASE_URL")
        .or_else(|_| std::env::var("KC__DATABASE__URL"))
        .expect("set DATABASE_URL to an isolated integration database")
}

/// Explicit test Redis endpoint; do not load machine-specific environment files.
pub fn resolve_redis_url() -> String {
    std::env::var("REDIS_URL")
        .or_else(|_| std::env::var("KC__REDIS__URL"))
        .expect("set REDIS_URL to an isolated integration Redis")
}

/// 带超时的测试运行器
pub async fn run_test_with_timeout<F, Fut, T>(f: F) -> anyhow::Result<T>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    timeout(TEST_TIMEOUT, f())
        .await
        .map_err(|_| anyhow::anyhow!("Test timed out after {:?}", TEST_TIMEOUT))?
}

/// 验证链结构 - 用于追踪数据流经过的 crate
#[derive(Debug, Default)]
pub struct VerificationChain {
    steps: Vec<VerificationStep>,
}

#[derive(Debug, Clone)]
pub struct VerificationStep {
    pub crate_name: &'static str,
    pub operation: &'static str,
    pub data_check: String,
    pub passed: bool,
}

impl VerificationChain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_step(
        &mut self,
        crate_name: &'static str,
        operation: &'static str,
        data_check: impl Into<String>,
        passed: bool,
    ) {
        self.steps.push(VerificationStep {
            crate_name,
            operation,
            data_check: data_check.into(),
            passed,
        });
    }

    pub fn all_passed(&self) -> bool {
        self.steps.iter().all(|s| s.passed)
    }

    pub fn get_steps(&self) -> &[VerificationStep] {
        &self.steps
    }

    pub fn print_report(&self) {
        println!("\n=== 数据链路验证报告 ===");
        for (i, step) in self.steps.iter().enumerate() {
            let status = if step.passed { "✓" } else { "✗" };
            println!(
                "{}. [{}] {}::{} - {}",
                i + 1,
                status,
                step.crate_name,
                step.operation,
                step.data_check
            );
        }
        println!(
            "\n结果: {}",
            if self.all_passed() {
                "所有验证通过 ✓"
            } else {
                "存在失败的验证 ✗"
            }
        );
    }
}

/// 模拟 SSE 流解析器
pub async fn parse_sse_stream<S>(mut stream: S) -> anyhow::Result<Vec<serde_json::Value>>
where
    S: tokio_stream::Stream<Item = Result<String, reqwest::Error>> + Unpin,
{
    use tokio_stream::StreamExt;
    let mut events = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk: String = chunk?;
        for line in chunk.lines() {
            if let Some(data) = line.strip_prefix("data: ") {
                if data == "[DONE]" {
                    return Ok(events);
                }
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                    events.push(json);
                }
            }
        }
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verification_chain() {
        let mut chain = VerificationChain::new();
        chain.add_step("test-crate", "test-op", "data verified", true);
        assert!(chain.all_passed());
    }

    #[test]
    fn test_test_context() {
        let ctx = TestContext::new();
        assert!(!ctx.test_id.is_empty());
        assert_ne!(ctx.request_id, Uuid::nil());
    }

    #[test]
    fn test_resolve_database_url_prefers_explicit_env() {
        unsafe {
            std::env::set_var("DATABASE_URL", "postgres://example");
            std::env::set_var("KC__DATABASE__URL", "postgres://fallback");
        }
        assert_eq!(resolve_database_url(), "postgres://example");
        unsafe {
            std::env::remove_var("DATABASE_URL");
        }
        assert_eq!(resolve_database_url(), "postgres://fallback");
        unsafe {
            std::env::remove_var("KC__DATABASE__URL");
        }
    }

    #[test]
    fn test_resolve_redis_url_prefers_explicit_env() {
        unsafe {
            std::env::set_var("REDIS_URL", "redis://:explicit@127.0.0.1:6379");
            std::env::set_var("KC__REDIS__URL", "redis://:fallback@127.0.0.1:6379");
        }
        assert_eq!(resolve_redis_url(), "redis://:explicit@127.0.0.1:6379");
        unsafe {
            std::env::remove_var("REDIS_URL");
        }
        assert_eq!(resolve_redis_url(), "redis://:fallback@127.0.0.1:6379");
        unsafe {
            std::env::remove_var("KC__REDIS__URL");
        }
    }
}
