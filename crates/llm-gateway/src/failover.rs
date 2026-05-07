//! 故障转移
//!
//! 管理 fallback 链和故障切换逻辑。

use keycompute_types::{ExecutionTarget, KeyComputeError};

/// 故障转移管理器
#[derive(Debug)]
pub struct FailoverManager {
    /// 最大 fallback 次数
    max_fallbacks: usize,
}

impl Default for FailoverManager {
    fn default() -> Self {
        Self::new()
    }
}

impl FailoverManager {
    /// 创建新的故障转移管理器
    pub fn new() -> Self {
        Self { max_fallbacks: 3 }
    }

    /// 创建带配置的管理器
    pub fn with_max_fallbacks(max_fallbacks: usize) -> Self {
        Self { max_fallbacks }
    }

    /// 选择下一个 fallback target
    pub fn select_next<'a>(
        &self,
        targets: &'a [ExecutionTarget],
        current_index: usize,
    ) -> Option<&'a ExecutionTarget> {
        if current_index + 1 >= targets.len() {
            return None;
        }
        targets.get(current_index + 1)
    }

    /// 记录失败
    pub fn record_failure(&self, target: &ExecutionTarget, error: &KeyComputeError) {
        let (account_id, provider) = match target {
            ExecutionTarget::ProviderAccount {
                account_id,
                provider,
                ..
            } => (account_id, provider),
            ExecutionTarget::Node { model } => {
                tracing::warn!(
                    model = %model,
                    error = %error,
                    "Node fallback target failed"
                );
                return;
            }
        };

        tracing::warn!(
            account_id = %account_id,
            provider = %provider,
            error = %error,
            "Fallback target failed"
        );
    }

    /// 获取最大 fallback 次数
    pub fn max_fallbacks(&self) -> usize {
        self.max_fallbacks
    }
}

/// 执行结果
#[derive(Debug)]
pub struct ExecutionResult {
    /// 是否成功
    pub success: bool,
    /// 使用的 target 索引
    pub target_index: usize,
    /// 尝试次数
    pub attempts: u32,
    /// 总耗时
    pub duration_ms: u64,
    /// 错误信息（如果失败）
    pub error: Option<String>,
}

impl ExecutionResult {
    /// 创建成功结果
    pub fn success(target_index: usize, attempts: u32, duration_ms: u64) -> Self {
        Self {
            success: true,
            target_index,
            attempts,
            duration_ms,
            error: None,
        }
    }

    /// 创建失败结果
    pub fn failure(attempts: u32, duration_ms: u64, error: impl Into<String>) -> Self {
        Self {
            success: false,
            target_index: 0,
            attempts,
            duration_ms,
            error: Some(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn create_test_targets() -> Vec<ExecutionTarget> {
        vec![
            ExecutionTarget::new_provider(
                "openai",
                Uuid::new_v4(),
                "https://api.openai.com",
                "key1",
            ),
            ExecutionTarget::new_provider(
                "claude",
                Uuid::new_v4(),
                "https://api.anthropic.com",
                "key2",
            ),
        ]
    }

    #[test]
    fn test_failover_manager_new() {
        let manager = FailoverManager::new();
        assert_eq!(manager.max_fallbacks(), 3);
    }

    #[test]
    fn test_select_next() {
        let manager = FailoverManager::new();
        let targets = create_test_targets();

        let next = manager.select_next(&targets, 0);
        assert!(next.is_some());

        // 验证返回的是 claude provider
        if let ExecutionTarget::ProviderAccount { provider, .. } = next.unwrap() {
            assert_eq!(provider, "claude");
        } else {
            panic!("Expected ProviderAccount variant");
        }

        let none = manager.select_next(&targets, 1);
        assert!(none.is_none());
    }

    #[test]
    fn test_execution_result() {
        let success = ExecutionResult::success(0, 1, 100);
        assert!(success.success);
        assert_eq!(success.target_index, 0);

        let failure = ExecutionResult::failure(3, 500, "timeout");
        assert!(!failure.success);
        assert_eq!(failure.error, Some("timeout".to_string()));
    }
}
