//! 路由配置
//!
//! Axum Router 配置，挂载所有路由

use crate::{
    handlers::{chat_completions, health_check, list_models},
    middleware::{
        cors_layer, rate_limit_middleware, request_logger, trace_id_middleware,
    },
    state::AppState,
};
use axum::{
    middleware::from_fn_with_state,
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;

/// 创建路由器
pub fn create_router(state: AppState) -> Router {
    // API 路由（需要限流）
    let api_routes = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(list_models))
        // API 路由添加限流中间件
        .layer(from_fn_with_state(state.clone(), rate_limit_middleware));

    // 健康检查路由（不需要限流）
    let health_routes = Router::new().route("/health", get(health_check));

    // 合并所有路由
    Router::new()
        .merge(api_routes)
        .merge(health_routes)
        .layer(axum::middleware::from_fn(request_logger))
        .layer(axum::middleware::from_fn(trace_id_middleware))
        .layer(TraceLayer::new_for_http())
        .layer(cors_layer())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_router() {
        let state = AppState::new();
        let router = create_router(state);
        // 确保可以创建路由器
        let _ = router;
    }
}
