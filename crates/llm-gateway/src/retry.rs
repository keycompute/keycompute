//! 重试策略
//!
//! 定义重试逻辑和退避策略。

use std::time::Duration;

/// 重试策略
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// 最大重试次数
    pub max_retries: u32,
    /// 初始退避时间（毫秒）
    pub initial_backoff_ms: u64,
    /// 最大退避时间（毫秒）
    pub max_backoff_ms: u64,
    /// 退避倍数
    pub backoff_multiplier: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff_ms: 100,
            max_backoff_ms: 10000,
            backoff_multiplier: 2.0,
        }
    }
}

impl RetryPolicy {
    /// 创建新的重试策略
    pub fn new(max_retries: u32) -> Self {
        Self {
            max_retries,
            ..Default::default()
        }
    }

    /// 计算第 n 次重试的退避时间
    pub fn backoff_duration(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::from_millis(0);
        }

        let backoff = (self.initial_backoff_ms as f64
            * self.backoff_multiplier.powi((attempt - 1) as i32))
            as u64;

        Duration::from_millis(backoff.min(self.max_backoff_ms))
    }

    /// 是否应该重试
    pub fn should_retry(&self, attempt: u32, error: &keycompute_types::KeyComputeError) -> bool {
        if attempt >= self.max_retries {
            return false;
        }

        // 某些错误不应该重试
        match error {
            keycompute_types::KeyComputeError::AuthError(_) => false,
            keycompute_types::KeyComputeError::RateLimitExceeded => false,
            _ => true,
        }
    }
}

/// 重试状态
#[derive(Debug)]
pub struct RetryState {
    /// 当前尝试次数
    pub attempt: u32,
    /// 策略
    pub policy: RetryPolicy,
}

impl RetryState {
    /// 创建新的重试状态
    pub fn new(policy: RetryPolicy) -> Self {
        Self { attempt: 0, policy }
    }

    /// 获取下一次退避时间
    pub fn next_backoff(&mut self) -> Duration {
        self.attempt += 1;
        self.policy.backoff_duration(self.attempt)
    }

    /// 是否应该继续重试
    pub fn should_retry(&self, error: &keycompute_types::KeyComputeError) -> bool {
        self.policy.should_retry(self.attempt, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retry_policy_default() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries, 3);
        assert_eq!(policy.initial_backoff_ms, 100);
    }

    #[test]
    fn test_backoff_duration() {
        let policy = RetryPolicy::default();
        
        assert_eq!(policy.backoff_duration(0), Duration::from_millis(0));
        assert_eq!(policy.backoff_duration(1), Duration::from_millis(100));
        assert_eq!(policy.backoff_duration(2), Duration::from_millis(200));
        assert_eq!(policy.backoff_duration(3), Duration::from_millis(400));
    }

    #[test]
    fn test_retry_state() {
        let policy = RetryPolicy::default();
        let mut state = RetryState::new(policy);

        assert_eq!(state.attempt, 0);
        
        let backoff = state.next_backoff();
        assert_eq!(backoff, Duration::from_millis(100));
        assert_eq!(state.attempt, 1);
    }
}
