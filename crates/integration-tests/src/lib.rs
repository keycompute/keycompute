//! KeyCompute 端到端集成测试
//!
//! 验证完整数据链路经过每个 crate 的关键业务点：
//!
//! 数据流验证点：
//! 1. API Layer (keycompute-server): 请求接入、认证提取、SSE 输出
//! 2. Auth (keycompute-auth): API Key 验证、用户/租户解析
//! 3. Rate Limit (keycompute-ratelimit): 限流检查
//! 4. Pricing (keycompute-pricing): 价格快照生成
//! 5. Routing (keycompute-routing): 执行计划生成
//! 6. Runtime (keycompute-runtime): 账号状态管理
//! 7. Gateway (llm-gateway): 流执行、Token 累积
//! 8. Billing (keycompute-billing): 用量计算、账单生成
//! 9. Distribution (keycompute-distribution): 分账计算
//! 10. Observability (keycompute-observability): 指标采集

pub mod common;
pub mod db;
pub mod mocks;
