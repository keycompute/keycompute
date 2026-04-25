//! KeyCompute 全局共享类型定义
//!
//! 本 crate 包含所有后端 crate 共享的核心类型，无任何业务逻辑，
//! 仅用于类型定义和数据结构。

pub mod error;
pub mod execution_plan;
pub mod pricing;
pub mod request;
pub mod response;
pub mod usage;
pub mod user;

// 重新导出最常用的类型
pub use error::{ErrorCategory, KeyComputeError, Result};
pub use execution_plan::{ExecutionPlan, ExecutionTarget, SensitiveString};
pub use pricing::PricingSnapshot;
pub use request::{ChatCompletionRequest, Message, MessageRole, RequestContext};
pub use response::{
    ChatCompletionChunk, ChatCompletionResponse, Choice, ErrorResponse, MessageDelta, ModelInfo,
    ModelListResponse, Usage,
};
pub use usage::{UsageAccumulator, UsageRecord};
pub use user::{AssignableUserRole, UserRole};
