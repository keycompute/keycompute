//! Node Gateway 节点协议类型定义
//!
//! 本协议版本固定为 `node.v1`，所有公开 JSON 字段使用 `snake_case`。
//! 这些类型在 `node-token` 和 `node-gateway` 之间共享，`node-token` 只能复用协议类型，
//! 不依赖 `node-gateway` 或服务端内部 store。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ============================================================================
// 类型别名
// ============================================================================

/// 节点 ID
pub type NodeId = Uuid;

/// 节点会话 ID
pub type NodeSessionId = Uuid;

/// 节点任务 ID
pub type NodeTaskId = Uuid;

/// 节点租约 ID
pub type NodeLeaseId = Uuid;

// ============================================================================
// 节点能力
// ============================================================================

/// 节点模型能力
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeModelCapability {
    /// 模型名称
    pub model: String,
}

/// 节点能力声明
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCapabilities {
    /// 运行时类型（MVP 固定为 "ollama"）
    pub runtime: String,
    /// 支持的模型列表
    pub models: Vec<NodeModelCapability>,
}

// ============================================================================
// 注册协议
// ============================================================================

/// 节点注册请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRegisterRequest {
    /// 协议版本（固定为 "node.v1"）
    pub protocol_version: String,
    /// 客户端实例 ID
    pub client_instance_id: String,
    /// 节点显示名称
    pub display_name: String,
    /// 注册 token
    pub registration_token: String,
    /// 节点能力声明
    pub capabilities: NodeCapabilities,
}

/// 节点注册响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRegisterResponse {
    /// 协议版本
    pub protocol_version: String,
    /// 节点 ID
    pub node_id: NodeId,
    /// 会话 ID
    pub session_id: NodeSessionId,
    /// 会话 token（只返回一次，服务端只保存 hash）
    pub session_token: String,
    /// 心跳间隔（秒）
    pub heartbeat_interval_secs: u64,
    /// 轮询超时（秒）
    pub poll_timeout_secs: u64,
}

// ============================================================================
// 心跳协议
// ============================================================================

/// 节点心跳请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeHeartbeatRequest {
    /// 协议版本
    pub protocol_version: String,
    /// 节点 ID
    pub node_id: NodeId,
    /// 会话 ID
    pub session_id: NodeSessionId,
    /// 当前可接受模型列表
    pub accepted_models: Vec<String>,
}

/// 节点心跳响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeHeartbeatResponse {
    /// 协议版本
    pub protocol_version: String,
    /// 是否接受（session 与请求体身份校验通过）
    pub accepted: bool,
    /// 节点状态（online/offline/excluded）
    pub node_status: String,
    /// 服务端失败计数
    pub server_failure_count: u32,
    /// 失败阈值
    pub failure_threshold: u32,
}

// ============================================================================
// 轮询协议
// ============================================================================

/// 节点轮询请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodePollRequest {
    /// 协议版本
    pub protocol_version: String,
    /// 节点 ID
    pub node_id: NodeId,
    /// 会话 ID
    pub session_id: NodeSessionId,
}

/// 节点轮询响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodePollResponse {
    /// 协议版本
    pub protocol_version: String,
    /// 任务信封（如果有任务）
    pub task: Option<NodeTaskEnvelope>,
    /// 重试间隔（毫秒）
    pub retry_after_ms: Option<u64>,
}

// ============================================================================
// 任务协议
// ============================================================================

/// 节点任务信封
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeTaskEnvelope {
    /// 任务 ID
    pub task_id: NodeTaskId,
    /// 租约 ID
    pub lease_id: NodeLeaseId,
    /// 模型名称（去掉 node: 前缀后的实际模型名）
    pub model: String,
    /// 任务截止时间（Unix 毫秒时间戳）
    pub deadline_unix_ms: i64,
    /// 完成宽限期（Unix 毫秒时间戳）
    pub complete_grace_until_unix_ms: i64,
    /// 任务载荷
    pub payload: NodeTaskPayload,
}

/// 节点任务载荷
///
/// 支持三种任务类型（互斥）：Chat 完成、图片生成、图片编辑。
/// 三个字段至多设置一个；若全部为 `None` 则视为无效 payload。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeTaskPayload {
    /// 请求 ID
    pub request_id: Uuid,
    /// Chat 完成请求（可选，与图片生成/编辑互斥）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<crate::ChatCompletionRequest>,
    /// 图片生成请求（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_generation: Option<ImageGenerationRequest>,
    /// 图片编辑请求（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_edit: Option<ImageEditRequest>,
}

impl NodeTaskPayload {
    /// 是否为 Chat 任务
    pub fn is_chat(&self) -> bool {
        self.chat.is_some()
    }

    /// 是否为图片生成任务
    pub fn is_image_generation(&self) -> bool {
        self.image_generation.is_some()
    }

    /// 是否为图片编辑任务
    pub fn is_image_edit(&self) -> bool {
        self.image_edit.is_some()
    }

    /// 校验 payload 合法性：至多设置一种任务类型，且不能全部为空。
    pub fn validate(&self) -> Result<(), &'static str> {
        let count = self.chat.is_some() as u8
            + self.image_generation.is_some() as u8
            + self.image_edit.is_some() as u8;
        if count > 1 {
            return Err("NodeTaskPayload: more than one task type set");
        }
        if count == 0 {
            return Err("NodeTaskPayload: no task type set");
        }
        Ok(())
    }
}

// ============================================================================
// 提交结果协议
// ============================================================================

/// 节点任务完成请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeTaskCompleteRequest {
    /// 协议版本
    pub protocol_version: String,
    /// 节点 ID
    pub node_id: NodeId,
    /// 会话 ID
    pub session_id: NodeSessionId,
    /// 任务 ID
    pub task_id: NodeTaskId,
    /// 租约 ID
    pub lease_id: NodeLeaseId,
    /// 任务结果
    pub result: NodeTaskResult,
}

/// 节点任务结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum NodeTaskResult {
    /// 任务成功(非流式完整响应)
    Succeeded {
        /// Chat 完成响应
        response: crate::ChatCompletionResponse,
    },
    /// 图片生成/编辑任务成功
    ImageSucceeded {
        /// 图片生成响应
        image_response: ImageGenerationResponse,
    },
    /// 任务失败
    Failed {
        /// 错误码
        code: String,
        /// 错误消息
        message: String,
        /// 该失败是否由请求本身的问题引起(如模型不支持、参数非法)。
        /// 若为 true: server 不计入 node failure_count, 不 requeue,
        /// 任务直接 terminal failed, 立即返回错误给 HTTP client。
        /// 老 client 不发该字段时默认 false, 保留原有行为。
        #[serde(default)]
        is_client_error: bool,
    },
}

/// 节点任务完成响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeTaskCompleteResponse {
    /// 执行动作
    pub action: NodeTaskCompleteAction,
    /// 任务状态
    pub task_status: String,
    /// 节点状态
    pub node_status: String,
    /// 服务端失败计数
    pub server_failure_count: u32,
    /// 失败阈值
    pub failure_threshold: u32,
}

/// 节点任务完成动作
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NodeTaskCompleteAction {
    /// 任务成功完成
    Succeeded,
    /// 任务恢复为 queued（重新入队）
    Requeued,
    /// 任务失败
    Failed,
    /// 任务过期
    Expired,
}

// ============================================================================
// 图片生成/编辑类型
// ============================================================================

/// 图片生成请求
///
/// `prompt` 为文本提示词，`n` 和 `size` 为可选参数（参照 OpenAI Images API）。
/// 注意：当通过 Ollama `/api/generate` 执行时，`n` 和 `size` 参数会被忽略
///（Ollama generate API 不支持多图/尺寸控制），仅发出 warning 日志。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerationRequest {
    /// 生成提示词
    pub prompt: String,
    /// 生成图片数量（可选，默认 1），如 `2`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    /// 图片尺寸（可选），如 `"1024x1024"`、`"512x512"`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
}

/// 图片编辑请求
///
/// 注意：`image` 和 `mask` 字段使用 base64 编码字符串传输，
/// 而非 `Vec<u8>`（后者经 JSON 序列化后会膨胀约 6 倍体积）。
/// 执行时由 node executor 解码为原始字节。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageEditRequest {
    /// 编辑提示词
    pub prompt: String,
    /// 原始图片（base64 编码，不含 data URI 前缀）
    pub image: String,
    /// 遮罩图片（可选，base64 编码）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask: Option<String>,
    /// 图片数量（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    /// 图片尺寸（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
}

/// 图片数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageData {
    /// 图片 URL（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Base64 编码的图片数据（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
    /// 修改后的提示词（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revised_prompt: Option<String>,
}

/// 图片生成响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerationResponse {
    /// 创建时间戳
    pub created: i64,
    /// 图片数据列表
    pub data: Vec<ImageData>,
}
