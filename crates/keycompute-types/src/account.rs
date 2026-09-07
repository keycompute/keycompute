//! 上游账号能力类型。

use serde::{Deserialize, Serialize};

/// 一个上游账号能够处理的原生 API 表面。
///
/// 协议类型（OpenAI / Anthropic）描述认证和基础线格式；能力进一步区分
/// 同一 OpenAI 兼容协议下的 Chat Completions 与 Responses，避免把请求
/// 路由到只实现了其中一个端点的账号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountApiCapability {
    ChatCompletions,
    Responses,
    Messages,
}

impl AccountApiCapability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "chat_completions" => Some(Self::ChatCompletions),
            "responses" => Some(Self::Responses),
            "messages" => Some(Self::Messages),
            _ => None,
        }
    }
}

impl std::fmt::Display for AccountApiCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_strings_are_stable() {
        for capability in [
            AccountApiCapability::ChatCompletions,
            AccountApiCapability::Responses,
            AccountApiCapability::Messages,
        ] {
            assert_eq!(
                AccountApiCapability::parse(capability.as_str()),
                Some(capability)
            );
        }
        assert_eq!(AccountApiCapability::parse("chat"), None);
    }
}
