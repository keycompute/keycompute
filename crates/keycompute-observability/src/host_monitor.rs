use std::sync::Arc;
use std::time::Duration;
use sysinfo::{Networks, System};
use tokio::time::interval;

/// 主机监控器
///
/// 采集 CPU、内存、磁盘、网络等系统指标
pub struct HostMonitor {
    system: Arc<tokio::sync::Mutex<System>>,
    networks: Arc<tokio::sync::Mutex<Networks>>,
}

impl Default for HostMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl HostMonitor {
    pub fn new() -> Self {
        Self {
            system: Arc::new(tokio::sync::Mutex::new(System::new_all())),
            networks: Arc::new(tokio::sync::Mutex::new(Networks::new_with_refreshed_list())),
        }
    }

    /// 启动后台监控任务
    ///
    /// 定期刷新系统信息并记录指标
    pub fn start_monitoring(self: Arc<Self>, interval_secs: u64) {
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(interval_secs));

            loop {
                ticker.tick().await;

                if let Err(e) = self.collect_metrics().await {
                    tracing::error!(error = %e, "Failed to collect host metrics");
                }
            }
        });
    }

    /// 采集一次系统指标
    async fn collect_metrics(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut system = self.system.lock().await;
        let mut networks = self.networks.lock().await;

        // 刷新系统信息
        system.refresh_all();
        networks.refresh(true);

        // CPU 使用率
        let cpu_usage = system.global_cpu_usage();
        tracing::info!(cpu_usage = %cpu_usage, "Host CPU usage");

        // 内存信息
        let total_memory = system.total_memory();
        let used_memory = system.used_memory();
        let memory_usage_percent = (used_memory as f32 / total_memory as f32) * 100.0;

        tracing::info!(
            total_memory_mb = %total_memory,
            used_memory_mb = %used_memory,
            memory_usage_percent = %memory_usage_percent,
            "Host memory usage"
        );

        // 网络流量
        let mut total_received = 0;
        let mut total_transmitted = 0;

        for network in networks.values() {
            total_received += network.total_received();
            total_transmitted += network.total_transmitted();
        }

        tracing::info!(
            network_received_bytes = %total_received,
            network_transmitted_bytes = %total_transmitted,
            "Host network traffic"
        );

        // 进程信息（可选）
        let process_count = system.processes().len();
        tracing::info!(process_count = %process_count, "Host process count");

        Ok(())
    }

    /// 获取当前系统快照
    pub async fn get_snapshot(&self) -> SystemSnapshot {
        let mut system = self.system.lock().await;
        system.refresh_all();

        SystemSnapshot {
            cpu_usage: system.global_cpu_usage(),
            total_memory: system.total_memory(),
            used_memory: system.used_memory(),
            process_count: system.processes().len(),
            timestamp: chrono::Utc::now(),
        }
    }
}

/// 系统快照
#[derive(Debug, Clone)]
pub struct SystemSnapshot {
    pub cpu_usage: f32,
    pub total_memory: u64,
    pub used_memory: u64,
    pub process_count: usize,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl SystemSnapshot {
    /// 获取内存使用率百分比
    pub fn memory_usage_percent(&self) -> f32 {
        if self.total_memory == 0 {
            return 0.0;
        }
        (self.used_memory as f32 / self.total_memory as f32) * 100.0
    }
}

/// 简化的主机指标采集函数
///
/// 用于一次性采集当前系统状态
pub async fn collect_host_metrics() -> HostMetrics {
    let mut system = System::new_all();
    system.refresh_all();

    HostMetrics {
        cpu_percent: system.global_cpu_usage(),
        memory_total: system.total_memory(),
        memory_used: system.used_memory(),
        load_avg: get_load_average(),
    }
}

/// 主机指标
#[derive(Debug, Clone, serde::Serialize)]
pub struct HostMetrics {
    pub cpu_percent: f32,
    pub memory_total: u64,
    pub memory_used: u64,
    pub load_avg: [f64; 3],
}

#[cfg(target_os = "linux")]
fn get_load_average() -> [f64; 3] {
    let load = sysinfo::System::load_average();
    [load.one, load.five, load.fifteen]
}

#[cfg(not(target_os = "linux"))]
fn get_load_average() -> [f64; 3] {
    [0.0, 0.0, 0.0]
}

/// 健康检查响应
#[derive(Debug, Clone, serde::Serialize)]
pub struct HealthStatus {
    pub status: String,
    pub version: String,
    pub uptime_seconds: u64,
    pub host_metrics: HostMetrics,
}

impl HealthStatus {
    pub fn new(version: impl Into<String>, uptime_seconds: u64) -> Self {
        Self {
            status: "healthy".to_string(),
            version: version.into(),
            uptime_seconds,
            host_metrics: HostMetrics {
                cpu_percent: 0.0,
                memory_total: 0,
                memory_used: 0,
                load_avg: [0.0, 0.0, 0.0],
            },
        }
    }

    pub async fn with_metrics(mut self) -> Self {
        self.host_metrics = collect_host_metrics().await;
        self
    }
}
