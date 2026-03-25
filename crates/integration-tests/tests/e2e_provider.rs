//! Provider 适配器端到端测试
//!
//! 验证各 Provider 适配器的协议转换和流处理

use integration_tests::common::VerificationChain;
use keycompute_claude::ClaudeProvider;
use keycompute_ollama::OllamaProvider;
use keycompute_openai::OpenAIProvider;
use keycompute_provider_trait::{ProviderAdapter, StreamEvent, UpstreamRequest};

/// 测试 Provider trait 基础功能
#[test]
fn test_provider_trait_basics() {
    let mut chain = VerificationChain::new();

    // 1. 测试 UpstreamRequest 构建器
    let request = UpstreamRequest::new(
        "https://api.openai.com/v1/chat/completions",
        "sk-test-key",
        "gpt-4o",
    )
    .with_message("system", "You are a helpful assistant")
    .with_message("user", "Hello")
    .with_stream(true)
    .with_max_tokens(1000)
    .with_temperature(0.7);

    chain.add_step(
        "keycompute-provider-trait",
        "UpstreamRequest::builder",
        format!("Model: {}", request.model),
        request.model == "gpt-4o",
    );
    chain.add_step(
        "keycompute-provider-trait",
        "UpstreamRequest::messages",
        format!("Message count: {}", request.messages.len()),
        request.messages.len() == 2,
    );
    chain.add_step(
        "keycompute-provider-trait",
        "UpstreamRequest::stream",
        format!("Stream enabled: {}", request.stream),
        request.stream,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试 StreamEvent 类型
#[test]
fn test_provider_stream_events() {
    let mut chain = VerificationChain::new();

    // 1. Delta 事件
    let delta = StreamEvent::Delta {
        content: "Hello".to_string(),
        finish_reason: None,
    };
    chain.add_step(
        "keycompute-provider-trait",
        "StreamEvent::Delta",
        "Delta event created",
        matches!(delta, StreamEvent::Delta { .. }),
    );

    // 2. Usage 事件
    let usage = StreamEvent::Usage {
        input_tokens: 100,
        output_tokens: 50,
    };
    chain.add_step(
        "keycompute-provider-trait",
        "StreamEvent::Usage",
        "Usage event created",
        matches!(usage, StreamEvent::Usage { .. }),
    );

    // 3. Done 事件
    let done = StreamEvent::Done;
    chain.add_step(
        "keycompute-provider-trait",
        "StreamEvent::Done",
        "Done event created",
        matches!(done, StreamEvent::Done),
    );

    // 4. Error 事件
    let error = StreamEvent::Error {
        message: "Test error".to_string(),
    };
    chain.add_step(
        "keycompute-provider-trait",
        "StreamEvent::Error",
        "Error event created",
        matches!(error, StreamEvent::Error { .. }),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试 OpenAI Provider
#[test]
fn test_provider_openai() {
    let mut chain = VerificationChain::new();

    // 1. 创建 OpenAI Provider
    let provider = OpenAIProvider::new();
    chain.add_step(
        "keycompute-openai",
        "OpenAIProvider::new",
        "OpenAI provider created",
        true,
    );

    // 2. 检查名称
    let name = provider.name();
    chain.add_step(
        "keycompute-openai",
        "OpenAIProvider::name",
        format!("Provider name: {}", name),
        name == "openai",
    );

    // 3. 检查支持的模型
    let models = provider.supported_models();
    chain.add_step(
        "keycompute-openai",
        "OpenAIProvider::supported_models",
        format!("Supported models: {:?}", models),
        !models.is_empty(),
    );

    // 4. 检查模型支持判断
    let supports_gpt4 = provider.supports_model("gpt-4o");
    let supports_unknown = provider.supports_model("unknown-model");

    chain.add_step(
        "keycompute-openai",
        "OpenAIProvider::supports_model_gpt4",
        format!("Supports gpt-4o: {}", supports_gpt4),
        supports_gpt4,
    );
    chain.add_step(
        "keycompute-openai",
        "OpenAIProvider::supports_model_unknown",
        format!("Supports unknown: {}", supports_unknown),
        !supports_unknown,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试 UpstreamMessage 辅助函数
#[test]
fn test_provider_upstream_message() {
    use keycompute_provider_trait::UpstreamMessage;

    let mut chain = VerificationChain::new();

    // 1. 创建系统消息
    let sys = UpstreamMessage::system("You are helpful");
    chain.add_step(
        "keycompute-provider-trait",
        "UpstreamMessage::system",
        format!("Role: {}", sys.role),
        sys.role == "system",
    );

    // 2. 创建用户消息
    let user = UpstreamMessage::user("Hello");
    chain.add_step(
        "keycompute-provider-trait",
        "UpstreamMessage::user",
        format!("Role: {}", user.role),
        user.role == "user",
    );

    // 3. 创建助手消息
    let assistant = UpstreamMessage::assistant("Hi there");
    chain.add_step(
        "keycompute-provider-trait",
        "UpstreamMessage::assistant",
        format!("Role: {}", assistant.role),
        assistant.role == "assistant",
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试 Claude Provider
#[test]
fn test_provider_claude() {
    let mut chain = VerificationChain::new();

    // 1. 创建 Claude Provider
    let provider = ClaudeProvider::new();
    chain.add_step(
        "keycompute-claude",
        "ClaudeProvider::new",
        "Claude provider created",
        true,
    );

    // 2. 检查名称
    let name = provider.name();
    chain.add_step(
        "keycompute-claude",
        "ClaudeProvider::name",
        format!("Provider name: {}", name),
        name == "claude",
    );

    // 3. 检查支持的模型
    let models = provider.supported_models();
    chain.add_step(
        "keycompute-claude",
        "ClaudeProvider::supported_models",
        format!("Supported models count: {}", models.len()),
        !models.is_empty(),
    );

    // 4. 检查 Claude 3.5 Sonnet 支持
    let supports_sonnet = provider.supports_model("claude-3-5-sonnet-20241022");
    chain.add_step(
        "keycompute-claude",
        "ClaudeProvider::supports_model_sonnet",
        format!("Supports claude-3-5-sonnet-20241022: {}", supports_sonnet),
        supports_sonnet,
    );

    // 5. 检查 Claude 3 Opus 支持
    let supports_opus = provider.supports_model("claude-3-opus-20240229");
    chain.add_step(
        "keycompute-claude",
        "ClaudeProvider::supports_model_opus",
        format!("Supports claude-3-opus-20240229: {}", supports_opus),
        supports_opus,
    );

    // 6. 检查不支持的模型
    let supports_unknown = provider.supports_model("gpt-4o");
    chain.add_step(
        "keycompute-claude",
        "ClaudeProvider::supports_model_unknown",
        format!("Supports gpt-4o: {}", supports_unknown),
        !supports_unknown,
    );

    // 7. 检查短名称支持（兼容形式）
    let supports_short_name = provider.supports_model("claude-3-5-sonnet");
    chain.add_step(
        "keycompute-claude",
        "ClaudeProvider::supports_model_short_name",
        format!("Supports claude-3-5-sonnet: {}", supports_short_name),
        supports_short_name,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试 Ollama Provider
#[test]
fn test_provider_ollama() {
    let mut chain = VerificationChain::new();

    // 1. 创建 Ollama Provider
    let provider = OllamaProvider::new();
    chain.add_step(
        "keycompute-ollama",
        "OllamaProvider::new",
        "Ollama provider created",
        true,
    );

    // 2. 检查名称
    let name = provider.name();
    chain.add_step(
        "keycompute-ollama",
        "OllamaProvider::name",
        format!("Provider name: {}", name),
        name == "ollama",
    );

    // 3. 检查支持的模型
    let models = provider.supported_models();
    chain.add_step(
        "keycompute-ollama",
        "OllamaProvider::supported_models",
        format!("Supported models count: {}", models.len()),
        !models.is_empty(),
    );

    // 4. 检查 Llama 3.2 支持
    let supports_llama = provider.supports_model("llama3.2");
    chain.add_step(
        "keycompute-ollama",
        "OllamaProvider::supports_model_llama",
        format!("Supports llama3.2: {}", supports_llama),
        supports_llama,
    );

    // 5. 检查 Mistral 支持
    let supports_mistral = provider.supports_model("mistral");
    chain.add_step(
        "keycompute-ollama",
        "OllamaProvider::supports_model_mistral",
        format!("Supports mistral: {}", supports_mistral),
        supports_mistral,
    );

    // 6. 检查 Qwen 支持
    let supports_qwen = provider.supports_model("qwen2.5:7b");
    chain.add_step(
        "keycompute-ollama",
        "OllamaProvider::supports_model_qwen",
        format!("Supports qwen2.5:7b: {}", supports_qwen),
        supports_qwen,
    );

    // 7. 检查不支持的模型
    let supports_unknown = provider.supports_model("gpt-4o");
    chain.add_step(
        "keycompute-ollama",
        "OllamaProvider::supports_model_unknown",
        format!("Supports gpt-4o: {}", supports_unknown),
        !supports_unknown,
    );

    // 8. 检查默认端点
    let default_endpoint = keycompute_ollama::OLLAMA_DEFAULT_ENDPOINT;
    chain.add_step(
        "keycompute-ollama",
        "OllamaProvider::default_endpoint",
        format!("Default endpoint: {}", default_endpoint),
        default_endpoint == "http://localhost:11434/api/chat",
    );

    chain.print_report();
    assert!(chain.all_passed());
}
