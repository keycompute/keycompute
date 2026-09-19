//! KeyCompute Server
//!
//! API Gateway Layer：主 Axum 服务器，OpenAI-compatible API 入口。
//! 负责 HTTP 路由、中间件编排、SSE 输出，不含业务逻辑。

pub mod account_capacity;
pub mod admission;
pub mod console;
pub mod display_cache;
pub mod error;
pub mod extractors;
pub mod handlers;
mod lifecycle_metrics;
pub mod middleware;
pub(crate) mod model_binding;
pub mod payment_registry;
pub mod providers;
pub mod router;
pub mod shutdown;
pub mod state;

pub use error::{ApiError, Result};
pub use extractors::AuthExtractor;
pub use router::create_router;
pub use state::{AppState, AppStateConfig, init_global_crypto};

use std::net::SocketAddr;
use tracing::info;

// 从 keycompute-config 导入 ServerConfig
pub use keycompute_config::ServerConfig;

/// 运行服务器
pub async fn run(config: ServerConfig, state: AppState) -> crate::Result<()> {
    run_with_shutdown(config, state, std::future::pending()).await
}

pub async fn run_with_shutdown(
    config: ServerConfig,
    state: AppState,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> crate::Result<()> {
    if !(1..=3600).contains(&config.shutdown_timeout_secs) {
        return Err(ApiError::Config("invalid server shutdown timeout".into()));
    }
    let addr: SocketAddr = format!("{}:{}", config.bind_addr, config.port)
        .parse()
        .map_err(|error| ApiError::Config(format!("Invalid address: {error}")))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|error| ApiError::Internal(format!("Failed to bind: {error}")))?;
    info!("KeyCompute server starting on {}", addr);
    shutdown::serve_router_with_shutdown(
        listener,
        create_router(state.clone()),
        state,
        shutdown,
        std::time::Duration::from_secs(config.shutdown_timeout_secs),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_config_default() {
        let config = ServerConfig::default();
        assert_eq!(config.bind_addr, "0.0.0.0");
        assert_eq!(config.port, 3000);
    }
}
