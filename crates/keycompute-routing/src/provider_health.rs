//! Provider 健康状态管理
//!
//! 管理 Provider 的健康状态、延迟、成功率等指标。

use dashmap::DashMap;
use std::time::{Duration, Instant};

/// Provider 健康状态
#[derive(Debug, Clone)]
pub struct ProviderHealth {
    /// Provider 名称
    pub name: String,
    /// 是否健康
    pub healthy: bool,
    /// 平均延迟（毫秒）
    pub avg_latency_ms: u64,
    /// 成功率（百分比，0-100）
    pub success_rate: f64,
    /// 总请求数
    pub total_requests: u64,
    /// 成功请求数
    pub success_requests: u64,
    /// 失败请求数
    pub failed_requests: u64,
    /// 最后更新时间
    pub last_updated: Instant,
    /// 最后成功时间
    pub last_success_at: Option<Instant>,
    /// 最后失败时间
    pub last_failure_at: Option<Instant>,
}

impl ProviderHealth {
    /// 创建新的健康状态
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            healthy: true,
            avg_latency_ms: 0,
            success_rate: 100.0,
            total_requests: 0,
            success_requests: 0,
            failed_requests: 0,
            last_updated: Instant::now(),
            last_success_at: None,
            last_failure_at: None,
        }
    }

    /// 记录成功请求
    pub fn record_success(&mut self, latency_ms: u64) {
        self.total_requests += 1;
        self.success_requests += 1;
        self.last_success_at = Some(Instant::now());

        // 更新平均延迟（指数加权移动平均 EWMA）
        // alpha = 0.3 表示新样本占 30%，历史占 70%
        // 这样对突发延迟更敏感，同时保持一定平滑性
        const ALPHA: f64 = 0.3;
        if self.avg_latency_ms == 0 {
            self.avg_latency_ms = latency_ms;
        } else {
            let current = self.avg_latency_ms as f64;
            let new = latency_ms as f64;
            self.avg_latency_ms = (ALPHA * new + (1.0 - ALPHA) * current) as u64;
        }

        self.update_success_rate();
        self.last_updated = Instant::now();
    }

    /// 记录失败请求
    pub fn record_failure(&mut self) {
        self.total_requests += 1;
        self.failed_requests += 1;
        self.last_failure_at = Some(Instant::now());

        self.update_success_rate();
        self.last_updated = Instant::now();
    }

    /// 更新成功率
    fn update_success_rate(&mut self) {
        if self.total_requests > 0 {
            self.success_rate = (self.success_requests as f64 / self.total_requests as f64) * 100.0;
        }

        // 如果成功率低于阈值，标记为不健康
        if self.success_rate < 50.0 && self.total_requests >= 10 {
            self.healthy = false;
        } else if self.success_rate >= 80.0 {
            self.healthy = true;
        }
    }

    /// 获取健康评分（0-100）
    pub fn health_score(&self) -> u64 {
        if !self.healthy {
            return 0;
        }

        // 基于成功率和延迟计算评分
        let latency_score = if self.avg_latency_ms < 100 {
            100
        } else if self.avg_latency_ms < 500 {
            80
        } else if self.avg_latency_ms < 1000 {
            60
        } else {
            40
        };

        ((self.success_rate as u64 * 60 + latency_score as u64 * 40) / 100).min(100)
    }
}

/// Provider 健康状态存储
#[derive(Debug)]
pub struct ProviderHealthStore {
    health_map: DashMap<String, ProviderHealth>,
}

impl Default for ProviderHealthStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderHealthStore {
    /// 创建新的健康状态存储
    pub fn new() -> Self {
        Self {
            health_map: DashMap::new(),
        }
    }

    /// 记录成功请求
    pub fn record_success(&self, provider: impl AsRef<str>, latency_ms: u64) {
        let provider = provider.as_ref();

        self.health_map
            .entry(provider.to_string())
            .and_modify(|health| health.record_success(latency_ms))
            .or_insert_with(|| {
                let mut health = ProviderHealth::new(provider);
                health.record_success(latency_ms);
                health
            });

        tracing::debug!(
            provider = %provider,
            latency_ms = latency_ms,
            "Provider request succeeded"
        );
    }

    /// 记录失败请求
    pub fn record_failure(&self, provider: impl AsRef<str>) {
        let provider = provider.as_ref();

        self.health_map
            .entry(provider.to_string())
            .and_modify(|health| health.record_failure())
            .or_insert_with(|| {
                let mut health = ProviderHealth::new(provider);
                health.record_failure();
                health
            });

        tracing::warn!(provider = %provider, "Provider request failed");
    }

    /// 获取 Provider 健康状态（Routing 只读）
    pub fn get_health(&self, provider: &str) -> Option<ProviderHealth> {
        self.health_map.get(provider).map(|h| h.clone())
    }

    /// 检查 Provider 是否健康
    pub fn is_healthy(&self, provider: &str) -> bool {
        self.health_map
            .get(provider)
            .map(|h| h.healthy)
            .unwrap_or(true) // 默认认为健康
    }

    /// 获取所有 Provider 健康状态
    pub fn all_health(&self) -> Vec<ProviderHealth> {
        self.health_map
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// 获取健康 Provider 列表
    pub fn healthy_providers(&self, providers: &[String]) -> Vec<String> {
        providers
            .iter()
            .filter(|p| self.is_healthy(p))
            .cloned()
            .collect()
    }

    /// 获取 Provider 评分（用于路由排序）
    pub fn get_score(&self, provider: &str) -> u64 {
        self.health_map
            .get(provider)
            .map(|h| h.health_score())
            .unwrap_or(50) // 默认中等评分
    }

    /// 更新 Provider 健康状态（手动设置）
    pub fn update_health(&self, provider: impl Into<String>, health: ProviderHealth) {
        let provider = provider.into();
        self.health_map.insert(provider, health);
    }

    /// 重置 Provider 统计
    pub fn reset_stats(&self, provider: &str) {
        self.health_map.remove(provider);
    }

    /// 清理长时间未更新的 Provider（可由后台任务调用）
    pub fn cleanup_stale(&self, max_age: Duration) {
        let now = Instant::now();
        let before = self.health_map.len();

        self.health_map
            .retain(|_, health| now.duration_since(health.last_updated) < max_age);

        let after = self.health_map.len();
        if before != after {
            tracing::debug!(
                removed = before - after,
                "Stale provider health entries cleaned up"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_health_new() {
        let health = ProviderHealth::new("openai");
        assert_eq!(health.name, "openai");
        assert!(health.healthy);
        assert_eq!(health.success_rate, 100.0);
    }

    #[test]
    fn test_record_success() {
        let mut health = ProviderHealth::new("openai");

        health.record_success(100);
        assert_eq!(health.total_requests, 1);
        assert_eq!(health.success_requests, 1);
        assert_eq!(health.avg_latency_ms, 100);

        // 使用 EWMA 算法 (alpha=0.3): 0.3 * 200 + 0.7 * 100 = 60 + 70 = 130
        health.record_success(200);
        assert_eq!(health.total_requests, 2);
        assert_eq!(health.avg_latency_ms, 130);

        // 验证 EWMA 对突发延迟更敏感
        // 第三次请求：0.3 * 300 + 0.7 * 130 = 90 + 91 = 181
        health.record_success(300);
        assert_eq!(health.total_requests, 3);
        assert_eq!(health.avg_latency_ms, 181);
    }

    #[test]
    fn test_record_failure() {
        let mut health = ProviderHealth::new("openai");

        // 10 次失败
        for _ in 0..10 {
            health.record_failure();
        }

        assert_eq!(health.total_requests, 10);
        assert_eq!(health.failed_requests, 10);
        assert!(!health.healthy); // 成功率低于 50%
    }

    #[test]
    fn test_health_score() {
        let mut health = ProviderHealth::new("openai");
        assert_eq!(health.health_score(), 100);

        // 多次失败降低评分
        for _ in 0..5 {
            health.record_failure();
        }

        assert!(health.health_score() < 100);
    }

    #[test]
    fn test_provider_health_store() {
        let store = ProviderHealthStore::new();

        store.record_success("openai", 100);
        store.record_success("openai", 200);
        store.record_failure("anthropic");

        assert!(store.is_healthy("openai"));
        // 健康评分基于成功率和延迟
        // 100% 成功率 + 低延迟(<100ms) = 100 分
        let score = store.get_score("openai");
        assert!(
            (90..=100).contains(&score),
            "Expected score around 100, got {}",
            score
        );

        // 不存在的 Provider 默认健康，评分中等
        assert!(store.is_healthy("unknown"));
        assert_eq!(store.get_score("unknown"), 50);
    }

    #[test]
    fn test_healthy_providers() {
        let store = ProviderHealthStore::new();

        // 让 anthropic 多次失败变得不健康
        for _ in 0..10 {
            store.record_failure("anthropic");
        }

        store.record_success("openai", 100);

        let providers = vec!["openai".to_string(), "anthropic".to_string()];
        let healthy = store.healthy_providers(&providers);

        assert_eq!(healthy.len(), 1);
        assert_eq!(healthy[0], "openai");
    }
}
