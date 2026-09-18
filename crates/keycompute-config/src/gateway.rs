//! Gateway 配置

use serde::Deserialize;
use std::collections::HashMap;

/// Gateway 配置
#[derive(Debug, Deserialize, Clone)]
pub struct GatewayConfig {
    #[serde(default)]
    pub routing_capacity: RoutingCapacityConfig,
    /// Process-local generation resource budgets, independent of business RPM/TPM.
    #[serde(default)]
    pub admission: GenerationAdmissionConfig,
    /// Maximum raw monitoring query range in hours.
    #[serde(default = "default_monitoring_raw_max_hours")]
    pub monitoring_raw_max_hours: u32,
    /// 自动 Provider Account 探测间隔（秒）；0 表示禁用，避免默认产生上游费用。
    #[serde(default)]
    pub account_probe_interval_secs: u64,
    /// 自动 Provider Account 探测并发数。
    #[serde(default = "default_account_probe_concurrency")]
    pub account_probe_concurrency: usize,
    /// 最大重试次数
    pub max_retries: u32,
    /// 请求超时时间（秒）
    pub timeout_secs: u64,
    /// 是否启用 fallback
    pub enable_fallback: bool,
    /// HTTP 请求超时（秒）
    pub request_timeout_secs: u64,
    /// 流式请求超时（秒）
    pub stream_timeout_secs: u64,
    /// HTTP 代理配置（可选）
    pub proxy: Option<ProxyConfig>,
}

/// HTTP 代理配置
#[derive(Debug, Deserialize, Clone)]
pub struct ProxyConfig {
    /// Provider 级代理映射
    /// 格式: {provider_name -> proxy_url}
    pub providers: HashMap<String, String>,
    /// 账号级代理映射（可选）
    /// 格式: {"provider:account_id" -> proxy_url}
    pub accounts: Option<HashMap<String, String>>,
    /// 通配符规则（可选）
    /// 格式: {pattern -> proxy_url}
    pub patterns: Option<HashMap<String, String>>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            routing_capacity: RoutingCapacityConfig::default(),
            admission: GenerationAdmissionConfig::default(),
            monitoring_raw_max_hours: default_monitoring_raw_max_hours(),
            account_probe_interval_secs: 0,
            account_probe_concurrency: default_account_probe_concurrency(),
            max_retries: 3,
            timeout_secs: 120,
            enable_fallback: true,
            request_timeout_secs: 120,
            stream_timeout_secs: 600,
            proxy: None,
        }
    }
}

fn default_monitoring_raw_max_hours() -> u32 {
    24
}

fn default_account_probe_concurrency() -> usize {
    4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_gateway_config() {
        let config = GatewayConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.timeout_secs, 120);
        assert_eq!(config.monitoring_raw_max_hours, 24);
        assert_eq!(config.account_probe_interval_secs, 0);
        assert_eq!(config.account_probe_concurrency, 4);
        assert!(config.enable_fallback);
        assert!(config.proxy.is_none());
    }

    #[test]
    fn test_proxy_config() {
        let mut providers = HashMap::new();
        providers.insert("openai".to_string(), "http://proxy-openai:8080".to_string());
        providers.insert("claude".to_string(), "http://proxy-claude:8080".to_string());

        let mut patterns = HashMap::new();
        patterns.insert("*-cn".to_string(), "http://cn-proxy:8080".to_string());

        let mut accounts = HashMap::new();
        accounts.insert(
            "openai:550e8400-e29b-41d4-a716-446655440000".to_string(),
            "http://premium-proxy:8080".to_string(),
        );

        let proxy = ProxyConfig {
            providers,
            patterns: Some(patterns),
            accounts: Some(accounts),
        };

        assert_eq!(proxy.providers.len(), 2);
        assert!(proxy.providers.contains_key("openai"));
        assert!(proxy.patterns.is_some());
        assert!(proxy.accounts.is_some());
    }
}

/// Hard bounds per application instance. Zero queue capacity selects fail-fast.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct GenerationAdmissionConfig {
    pub global_limit: usize,
    pub tenant_limit: usize,
    pub account_limit: usize,
    pub global_queue: usize,
    pub tenant_queue: usize,
    pub account_queue: usize,
    pub queue_timeout_ms: u64,
}
impl Default for GenerationAdmissionConfig {
    fn default() -> Self {
        Self {
            global_limit: 256,
            tenant_limit: 32,
            account_limit: 32,
            global_queue: 128,
            tenant_queue: 16,
            account_queue: 16,
            queue_timeout_ms: 1_000,
        }
    }
}
impl GenerationAdmissionConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.global_limit == 0
            || self.global_limit > 65_536
            || self.tenant_limit == 0
            || self.tenant_limit > self.global_limit
            || self.account_limit == 0
            || self.account_limit > self.global_limit
            || self.global_queue > 65_536
            || self.tenant_queue > self.global_queue
            || self.account_queue > self.global_queue
            || self.queue_timeout_ms == 0
            || self.queue_timeout_ms > 60_000
        {
            Err("invalid generation admission limits or queue timeout")
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn invalid_admission_config_is_rejected_before_serving() {
        let valid = GenerationAdmissionConfig::default();
        for invalid in [
            GenerationAdmissionConfig {
                global_limit: 0,
                ..valid.clone()
            },
            GenerationAdmissionConfig {
                tenant_limit: 0,
                ..valid.clone()
            },
            GenerationAdmissionConfig {
                account_limit: 65_537,
                ..valid.clone()
            },
            GenerationAdmissionConfig {
                tenant_queue: 129,
                ..valid.clone()
            },
            GenerationAdmissionConfig {
                account_queue: 129,
                ..valid.clone()
            },
            GenerationAdmissionConfig {
                queue_timeout_ms: 0,
                ..valid.clone()
            },
            GenerationAdmissionConfig {
                queue_timeout_ms: 60_001,
                ..valid.clone()
            },
        ] {
            assert!(invalid.validate().is_err());
        }
    }
}

/// Advisory load sampling only. These values never authorize an upstream call.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct RoutingCapacityConfig {
    pub candidate_limit: usize,
    pub cache_entries: usize,
    pub ttl_ms: u64,
    pub concurrency: usize,
    pub timeout_ms: u64,
}
impl Default for RoutingCapacityConfig {
    fn default() -> Self {
        Self {
            candidate_limit: 32,
            cache_entries: 4096,
            ttl_ms: 100,
            concurrency: 16,
            timeout_ms: 1000,
        }
    }
}
impl RoutingCapacityConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(4..=256).contains(&self.candidate_limit)
            || !(self.candidate_limit..=65_536).contains(&self.cache_entries)
            || self.ttl_ms > 1000
            || !(1..=64).contains(&self.concurrency)
            || !(1..=5000).contains(&self.timeout_ms)
        {
            Err("invalid routing capacity snapshot budgets")
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod capacity_config_tests {
    use super::*;
    #[test]
    fn snapshot_settings_reject_unbounded_or_inconsistent_limits() {
        let defaults = RoutingCapacityConfig::default();
        for invalid in [
            RoutingCapacityConfig {
                candidate_limit: 0,
                ..defaults.clone()
            },
            RoutingCapacityConfig {
                cache_entries: 31,
                ..defaults.clone()
            },
            RoutingCapacityConfig {
                ttl_ms: 1001,
                ..defaults.clone()
            },
            RoutingCapacityConfig {
                concurrency: 0,
                ..defaults.clone()
            },
            RoutingCapacityConfig {
                timeout_ms: 0,
                ..defaults.clone()
            },
        ] {
            assert!(invalid.validate().is_err());
        }
        assert!(defaults.validate().is_ok());
    }
}
