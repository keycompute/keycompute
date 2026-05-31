//! OpenAI Provider Adapter
//!
//! OpenAI API 的 Provider 适配器实现，作为参考实现和 OpenAI-compatible Provider 的基类。

pub mod adapter;
pub mod protocol;
pub mod stream;

pub use adapter::{
    OPENAI_CHAT_ENDPOINT, OPENAI_IMAGE_EDIT_ENDPOINT, OPENAI_IMAGE_GEN_ENDPOINT,
    OPENAI_IMAGE_VARIATION_ENDPOINT, OPENAI_RESPONSES_ENDPOINT, OpenAIProvider,
};
pub use protocol::{
    ImageData, ImageEditRequest, ImageGenerationRequest, ImageGenerationResponse,
    ImageVariationRequest, OpenAIContent, OpenAIContentPart, OpenAIImageUrl, OpenAIMessage,
    OpenAIRequest, OpenAIResponse, OpenAIStreamResponse, ResponsesInput, ResponsesInputPart,
    ResponsesOutputContent, ResponsesOutputItem, ResponsesRequest, ResponsesResponse,
    ResponsesTool, ResponsesUsage, StreamOptions, convert_message_content,
};
