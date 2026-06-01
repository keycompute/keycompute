//! Ollama Provider Adapter 实现
//!
//! 实现 ProviderAdapter trait，提供 Ollama 本地模型的调用能力
//!
//! Ollama 支持两种 API 格式：
//! 1. 原生格式: POST /api/chat (本实现使用此格式)
//! 2. OpenAI 兼容格式: POST /v1/chat/completions
//!
//! 使用统一 HTTP 传输层：
//! - 通过 HttpTransport 发送请求
//! - 支持连接池复用和代理出口

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use futures::StreamExt;
use keycompute_provider_trait::{
    ByteStream, HttpTransport, ProviderAdapter, StreamBox, StreamEvent, UpstreamRequest,
};
use keycompute_types::{ContentPart, KeyComputeError, MessageContent, Result};
use serde_json;
use tokio::sync::Semaphore;

use crate::protocol::{OllamaMessage, OllamaOptions, OllamaRequest, OllamaResponse};
use crate::stream::parse_ollama_stream;
use keycompute_openai::protocol::{
    OpenAIRequest, OpenAIResponse, StreamOptions, convert_message_content,
};
use keycompute_openai::stream::parse_openai_stream;

/// Ollama 默认 API 端点
pub const OLLAMA_DEFAULT_ENDPOINT: &str = "https://ollama.com/api/chat";

/// Ollama 支持的模型列表（基于官方 Ollama Cloud 模型）
pub const OLLAMA_MODELS: &[&str] = &[
    // Llama 3.2
    "llama3.2",
    "llama3.2:latest",
    "llama3.2:1b",
    "llama3.2:3b",
    // Mistral
    "mistral",
    "mistral:latest",
    "mistral:7b",
    // Gemma2
    "gemma2",
    "gemma2:latest",
    "gemma2:2b",
    "gemma2:9b",
    "gemma2:27b",
    // GPT-OSS
    "gpt-oss",
    "gpt-oss:latest",
    "gpt-oss:20b",
    "gpt-oss:120b",
    "gpt-oss:20b-cloud",
    "gpt-oss:120b-cloud",
    // Qwen 2.5
    "qwen2.5",
    "qwen2.5:latest",
    "qwen2.5:0.5b",
    "qwen2.5:1.5b",
    "qwen2.5:3b",
    "qwen2.5:7b",
    "qwen2.5:14b",
    "qwen2.5:32b",
    "qwen2.5:72b",
    // Qwen 3.5
    "qwen3.5",
    "qwen3.5:latest",
    "qwen3.5:cloud",
    "qwen3.5:0.8b",
    "qwen3.5:2b",
    "qwen3.5:4b",
    "qwen3.5:9b",
    "qwen3.5:27b",
    "qwen3.5:35b",
    "qwen3.5:122b",
    "qwen3.5:397b-cloud",
    // Qwen3-VL
    "qwen3-vl",
    "qwen3-vl:latest",
    "qwen3-vl:2b",
    "qwen3-vl:4b",
    "qwen3-vl:8b",
    "qwen3-vl:30b",
    "qwen3-vl:32b",
    "qwen3-vl:235b",
    "qwen3-vl:235b-cloud",
    "qwen3-vl:235b-instruct-cloud",
    // Qwen3-Coder
    "qwen3-coder",
    "qwen3-coder:latest",
    "qwen3-coder:30b",
    "qwen3-coder:480b",
    "qwen3-coder:480b-cloud",
    // Qwen3-Next
    "qwen3-next",
    "qwen3-next:latest",
    "qwen3-next:80b",
    "qwen3-next:80b-cloud",
    // Qwen3-Coder-Next
    "qwen3-coder-next",
    "qwen3-coder-next:latest",
    "qwen3-coder-next:cloud",
    "qwen3-coder-next:q4_K_M",
    "qwen3-coder-next:q8_0",
    // MiniMax
    "minimax-m2:cloud",
    "minimax-m2.1:cloud",
    "minimax-m2.5:cloud",
    "minimax-m2.7:cloud",
    // DeepSeek V3
    "deepseek-v3.1",
    "deepseek-v3.1:latest",
    "deepseek-v3.1:671b",
    "deepseek-v3.1:671b-cloud",
    "deepseek-v3.2:cloud",
    // GLM
    "glm-4.6:cloud",
    "glm-4.7:cloud",
    "glm-5:cloud",
    // Kimi K2
    "kimi-k2:1t-cloud",
    "kimi-k2-thinking:cloud",
    "kimi-k2.5:cloud",
    // Gemma3
    "gemma3",
    "gemma3:latest",
    "gemma3:270m",
    "gemma3:1b",
    "gemma3:4b",
    "gemma3:12b",
    "gemma3:27b",
    "gemma3:4b-cloud",
    "gemma3:12b-cloud",
    "gemma3:27b-cloud",
    // Gemma4
    "gemma4",
    "gemma4:latest",
    "gemma4:e2b",
    "gemma4:e4b",
    "gemma4:26b",
    "gemma4:31b",
    "gemma4:31b-cloud",
    // Mistral Large 3
    "mistral-large-3:675b-cloud",
    // Devstral
    "devstral-small-2",
    "devstral-small-2:latest",
    "devstral-small-2:24b",
    "devstral-small-2:24b-cloud",
    "devstral-2",
    "devstral-2:latest",
    "devstral-2:123b",
    "devstral-2:123b-cloud",
    // Ministral 3
    "ministral-3",
    "ministral-3:latest",
    "ministral-3:3b",
    "ministral-3:8b",
    "ministral-3:14b",
    "ministral-3:3b-cloud",
    "ministral-3:8b-cloud",
    "ministral-3:14b-cloud",
    // NVIDIA Nemotron
    "nemotron-3-super",
    "nemotron-3-super:latest",
    "nemotron-3-super:120b",
    "nemotron-3-super:cloud",
    "nemotron-3-nano",
    "nemotron-3-nano:latest",
    "nemotron-3-nano:4b",
    "nemotron-3-nano:30b",
    // Cogito 2.1
    "cogito-2.1",
    "cogito-2.1:latest",
    "cogito-2.1:671b",
    "cogito-2.1:671b-cloud",
    // Gemini 3 Flash
    "gemini-3-flash-preview",
    "gemini-3-flash-preview:latest",
    "gemini-3-flash-preview:cloud",
    // RNJ-1
    "rnj-1",
    "rnj-1:latest",
    "rnj-1:8b",
    "rnj-1:8b-cloud",
];

/// Ollama Provider 适配器
#[derive(Debug, Clone)]
pub struct OllamaProvider;

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl OllamaProvider {
    /// 创建新的 Ollama Provider
    pub fn new() -> Self {
        Self
    }

    /// 构建 Ollama 原生格式请求体
    ///
    /// `images_map` 是消息索引 → 已解析 base64 图片列表的映射（由 `resolve_request_images` 生成）。
    fn build_request_body(
        &self,
        request: &UpstreamRequest,
        images_map: &HashMap<usize, Vec<String>>,
    ) -> OllamaRequest {
        let mut system_content = None;
        let mut messages = Vec::new();

        for (i, msg) in request.messages.iter().enumerate() {
            if msg.role == "system" {
                system_content = Some(msg.content.to_string());
            } else {
                let text = msg.content.extract_text();
                let images = images_map.get(&i).cloned();
                messages.push(OllamaMessage {
                    role: msg.role.clone(),
                    content: text,
                    images,
                });
            }
        }

        let options = if request.temperature.is_some()
            || request.top_p.is_some()
            || request.max_tokens.is_some()
        {
            let mut opts = OllamaOptions::new();
            if let Some(temp) = request.temperature {
                opts = opts.temperature(temp);
            }
            if let Some(top_p) = request.top_p {
                opts = opts.top_p(top_p);
            }
            if let Some(max_tokens) = request.max_tokens {
                opts = opts.num_predict(max_tokens as i32);
            }
            Some(opts)
        } else {
            None
        };

        OllamaRequest {
            model: request.model.clone(),
            messages,
            stream: Some(request.stream),
            format: None,
            options,
            system: system_content,
        }
    }

    /// 提取 data URI 中的纯 base64 图片数据
    ///
    /// Ollama 原生 API 的 images 字段只接受纯 base64（无 data URI 前缀）。
    /// HTTP URL 在此处跳过，由 `resolve_images_async` 异步下载处理。
    fn extract_data_uri_images(content: &MessageContent) -> Option<Vec<String>> {
        match content {
            MessageContent::Parts(parts) => {
                let images: Vec<String> = parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::ImageUrl { image_url } => {
                            // 只处理 base64 data URI: data:image/xxx;base64,<data> → 纯 base64
                            // 必须验证中间段包含 "base64"，拒绝非 base64 编码的 data URI
                            if let Some(rest) = image_url.url.strip_prefix("data:image/") {
                                rest.split_once(',').and_then(|(header, data)| {
                                    if header.contains("base64") {
                                        Some(data.to_string())
                                    } else {
                                        None
                                    }
                                })
                            } else {
                                // HTTP URL 在此跳过，由 resolve_images_async 处理
                                None
                            }
                        }
                        _ => None,
                    })
                    .collect();
                if images.is_empty() {
                    None
                } else {
                    Some(images)
                }
            }
            _ => None,
        }
    }

    /// 判断消息内容是否包含需要异步下载的 HTTP URL 图片
    ///
    /// 仅 `Parts` 变体中的 `ImageUrl` 且 URL 不以 `data:` 开头才需要 HTTP 下载。
    /// 此方法用于 Semaphore 之前的快速路径判断，避免纯文本消息不必要地获取并发许可。
    fn needs_image_download(content: &MessageContent) -> bool {
        match content {
            MessageContent::Parts(parts) => parts.iter().any(|p| {
                matches!(p, ContentPart::ImageUrl { image_url } if !image_url.url.starts_with("data:"))
            }),
            _ => false,
        }
    }

    /// 判断 IP 地址是否为私有/内网地址
    ///
    /// 覆盖所有 RFC 1918 / RFC 6598 / RFC 3927 / RFC 4291 私有地址范围：
    /// - IPv4: 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
    /// - IPv4: 127.0.0.0/8 (loopback), 169.254.0.0/16 (link-local, 含云元数据端点)
    /// - IPv4: 0.0.0.0 (unspecified, 精确匹配)
    /// - IPv6: ::1 (loopback), fc00::/7 (ULA), fe80::/10 (link-local)
    fn is_private_ip(ip: &IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => {
                let octets = v4.octets();
                v4.is_loopback()                          // 127.0.0.0/8
                    || v4.is_unspecified()                // 0.0.0.0
                    || octets[0] == 10                    // 10.0.0.0/8
                    || (octets[0] == 172 && octets[1] >= 16 && octets[1] <= 31) // 172.16.0.0/12
                    || (octets[0] == 192 && octets[1] == 168) // 192.168.0.0/16
                    || (octets[0] == 169 && octets[1] == 254) // 169.254.0.0/16 (link-local, AWS IMDS)
            }
            IpAddr::V6(v6) => {
                let octets = v6.octets();
                v6.is_loopback()                          // ::1
                    || (octets[0] & 0xfe) == 0xfc         // fc00::/7 (ULA)
                    || octets[0] == 0xfe && octets[1] & 0xc0 == 0x80 // fe80::/10 (link-local)
            }
        }
    }

    /// 提取 URL 中的 host 和端口
    ///
    /// 支持以下格式：
    /// - `example.com:8080` → ("example.com", 8080)
    /// - `example.com` → ("example.com", 80/443)
    /// - `[::1]:8080` → ("::1", 8080)   (IPv6 方括号)
    /// - `[::1]` → ("::1", 80/443)      (IPv6 方括号，无显式端口)
    fn extract_host_port(url: &str) -> Result<(&str, u16)> {
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .ok_or_else(|| KeyComputeError::ProviderError(format!("Invalid image URL: {}", url)))?;

        let host_port = rest.split('/').next().unwrap_or("");
        if host_port.is_empty() {
            return Err(KeyComputeError::ProviderError(format!(
                "Invalid image URL (missing host): {}",
                url
            )));
        }

        // 纵深防御：拒绝明显非法的 host（空格、控制字符、@、以 / 开头）
        if host_port.contains(' ')
            || host_port.contains('@')
            || host_port.starts_with('/')
            || host_port.chars().any(|c| c.is_ascii_control())
        {
            return Err(KeyComputeError::ProviderError(format!(
                "Invalid image host: {}",
                url
            )));
        }

        // IPv6 方括号格式: [::1] 或 [::1]:8080
        if let Some(rest) = host_port.strip_prefix('[') {
            let (host, port_str) = rest.split_once(']').ok_or_else(|| {
                KeyComputeError::ProviderError(format!("Invalid IPv6 URL (missing ']'): {}", url))
            })?;
            if host.is_empty() {
                return Err(KeyComputeError::ProviderError(format!(
                    "Invalid IPv6 URL (empty host): {}",
                    url
                )));
            }
            let port = if let Some(p) = port_str.strip_prefix(':') {
                p.parse::<u16>().map_err(|_| {
                    KeyComputeError::ProviderError(format!("Invalid port in image URL: {}", url))
                })?
            } else if url.starts_with("https://") {
                443
            } else {
                80
            };
            return Ok((host, port));
        }

        if let Some((host, port_str)) = host_port.split_once(':') {
            let port: u16 = port_str.parse().map_err(|_| {
                KeyComputeError::ProviderError(format!("Invalid port in image URL: {}", url))
            })?;
            Ok((host, port))
        } else {
            let default_port = if url.starts_with("https://") { 443 } else { 80 };
            Ok((host_port, default_port))
        }
    }

    /// 验证图片 URL 是否安全（防 SSRF 攻击）
    ///
    /// 多层防御：
    /// 1. 协议检查：仅允许 http/https
    /// 2. 字符串前缀匹配：快速拦截内网/私有地址字面量（localhost、127.、10.、172.16-31、192.168. 等）
    /// 3. DNS 解析检查（`validate_dns_no_private`）：解析域名后验证实际 IP 非私有地址
    ///
    /// 已知限制（TOCTOU 窗口）：
    /// `validate_dns_no_private` 预检与 reqwest 实际 DNS 解析之间存在极短的竞态窗口。
    /// 攻击者可利用超低 TTL DNS 记录实施重绑定攻击（预检时返回公网 IP，请求时切换到内网 IP）。
    /// 以下措施将此窗口的利用难度提升至极高水平：
    /// - `HttpTransport::get_binary` 实现方禁止 HTTP 重定向（trait 安全要求），阻断间接 SSRF
    /// - DNS 10s + HTTP 30s 独立超时，防止慢速攻击和网络不通导致的无限阻塞
    /// - 下载后校验 Content-Type 为 `image/*`，防止返回非图片内容
    /// - 20 MB 下载大小限制防止内存耗尽
    ///
    /// 注意：不再使用 IP 固定连接（会导致 TLS SNI 与 hostname 不匹配），
    /// 改为依赖 DNS 预检 + 多层防御。
    fn validate_image_url(url: &str) -> Result<()> {
        // 仅允许 http/https
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(KeyComputeError::ProviderError(format!(
                "Unsupported image URL protocol: {}",
                url
            )));
        }

        // 提取 host（快速路径：字符串前缀匹配）
        let (host, _port) = Self::extract_host_port(url)?;

        // 判断 host 是否为 IP 字面量（IPv4 或 IPv6）
        let is_ip_literal = host.parse::<IpAddr>().is_ok();

        // 始终拦截 localhost（知名主机名，必然指向回环地址）
        if host == "localhost" {
            return Err(KeyComputeError::ProviderError(format!(
                "Internal/private IP address blocked for image download: {}",
                host
            )));
        }

        // IPv4/IPv6 私有/回环地址前缀拦截 — 仅对 IP 字面量生效，避免误拦截合法域名
        // （如 127.example.com、10x-engineering.io、fcdn.example.com 等）
        if is_ip_literal {
            // IPv6 前缀
            // 注意：extract_host_port 返回的 host 已剥离 IPv6 方括号，因此前缀也是去括号格式
            let ipv6_blocked = ["::1", "::ffff:", "fc", "fd", "fe80:"];
            for prefix in &ipv6_blocked {
                if host.starts_with(prefix) {
                    return Err(KeyComputeError::ProviderError(format!(
                        "Internal/private IP address blocked for image download: {}",
                        host
                    )));
                }
            }

            // IPv4 前缀
            let ipv4_blocked = ["127.", "10.", "0.0.0.0", "169.254."];
            for prefix in &ipv4_blocked {
                if host.starts_with(prefix) {
                    return Err(KeyComputeError::ProviderError(format!(
                        "Internal/private IP address blocked for image download: {}",
                        host
                    )));
                }
            }

            if host.starts_with("172.")
                && let Some(second) = host.split('.').nth(1)
                && let Ok(n) = second.parse::<u32>()
                && (16..=31).contains(&n)
            {
                return Err(KeyComputeError::ProviderError(format!(
                    "Private network address blocked for image download: {}",
                    host
                )));
            }

            if host.starts_with("192.168.") {
                return Err(KeyComputeError::ProviderError(format!(
                    "Private network address blocked for image download: {}",
                    host
                )));
            }
        }

        Ok(())
    }

    /// 解析 DNS 并验证实际 IP 非私有地址（防 DNS 重绑定攻击）
    ///
    /// URL 字符串前缀匹配是快速路径，但无法防御 DNS 重绑定：
    /// 攻击者注册低 TTL 域名，验证时解析到公网 IP，请求时解析到内网 IP。
    /// 此方法在实际 HTTP 请求前解析 DNS 并检查所有解析出的 IP。
    ///
    /// # 重要：调用方必须包裹超时
    ///
    /// 本方法内部使用 `tokio::net::lookup_host` 进行 DNS 解析，
    /// 该调用**没有内置超时**——网络不通时可能永久挂起。
    /// **所有调用方必须用 `tokio::time::timeout` 包裹本方法**，
    /// 否则将重现已修复的 504 Gateway Timeout 问题。
    /// 当前唯一调用方 `download_image_to_base64` 使用 10s DNS 超时。
    ///
    /// 返回解析出的 IP 地址列表。
    /// 注意：调用方仅需校验 `Ok`（所有 IP 均为公网地址），
    /// 不再使用返回值进行 IP 固定连接（会导致 HTTPS 的 TLS SNI 不匹配问题）。
    async fn validate_dns_no_private(url: &str) -> Result<Vec<IpAddr>> {
        let (host, _port) = Self::extract_host_port(url)?;

        // 如果 host 已经是 IP 字面量，直接检查是否为私有/内网地址
        if let Ok(ip) = host.parse::<IpAddr>() {
            if Self::is_private_ip(&ip) {
                return Err(KeyComputeError::ProviderError(format!(
                    "Private IP address blocked for image download: {}",
                    host
                )));
            }
            return Ok(vec![ip]);
        }

        // 解析 DNS（使用系统默认解析器）
        let addr_str = format!("{}:{}", host, _port);
        let addrs = tokio::net::lookup_host(&addr_str).await.map_err(|e| {
            KeyComputeError::ProviderError(format!(
                "Failed to resolve image host '{}': {}",
                host, e
            ))
        })?;

        let ips: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
        for ip in &ips {
            if Self::is_private_ip(ip) {
                return Err(KeyComputeError::ProviderError(format!(
                    "Image host '{}' resolved to private IP {} — blocked for security",
                    host, ip
                )));
            }
        }

        Ok(ips)
    }

    /// 通过 HTTP 下载图片并转换为 base64
    ///
    /// 使用 `HttpTransport` 统一发送请求（而非直接使用 reqwest），
    /// 确保代理配置、Mock 测试等与系统其他部分一致。
    ///
    /// DNS 重绑定防护：
    /// 1. `validate_dns_no_private` 解析 DNS 并验证所有 IP 非私有地址（预检）
    /// 2. 使用原始 URL 发起请求，reqwest 自行解析 DNS 并设置 TLS SNI
    /// 3. DNS 重绑定的 TOCTOU 窗口仅毫秒级（两次 DNS 解析之间），
    ///    配合禁止重定向 + Content-Type 校验 + 大小限制，实际风险极低
    ///
    /// 安全措施（多层防御）：
    /// - URL 前缀匹配快速拦截内网地址
    /// - DNS 预检解析验证实际 IP 非私有地址（10s 独立超时）
    /// - 禁止 HTTP 重定向（transport 层保证）
    /// - 限制最大下载大小为 20MB，防止内存耗尽攻击
    /// - DNS 解析 + HTTP 下载各有独立超时，防止慢速攻击和网络不通导致的无限阻塞
    async fn download_image_to_base64(transport: &dyn HttpTransport, url: &str) -> Result<String> {
        const MAX_IMAGE_SIZE: u64 = 20 * 1024 * 1024; // 20 MB
        const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

        // 防 SSRF: 验证 URL 安全性（阻止内网地址）—— 同步操作，无需 timeout
        Self::validate_image_url(url)?;

        // DNS 解析超时（10s）：防止容器网络不通时 tokio::net::lookup_host 永久挂起
        const DNS_TIMEOUT: Duration = Duration::from_secs(10);
        let _resolved_ips = tokio::time::timeout(DNS_TIMEOUT, Self::validate_dns_no_private(url))
            .await
            .map_err(|_| {
                KeyComputeError::ProviderError(format!(
                    "DNS resolution timed out ({}s) for image host: {}",
                    DNS_TIMEOUT.as_secs(),
                    url
                ))
            })??;

        // HTTP GET 超时（30s）：防止慢速下载阻塞
        let get_response =
            tokio::time::timeout(DOWNLOAD_TIMEOUT, transport.get_binary(url, vec![]))
                .await
                .map_err(|_| {
                    KeyComputeError::ProviderError(format!(
                        "Image download timed out ({}s): {}",
                        DOWNLOAD_TIMEOUT.as_secs(),
                        url
                    ))
                })??;

        // 使用原始 URL 直接发起请求，确保 TLS SNI 正确匹配 hostname。

        // 校验 Content-Type 为 image/*，防止下载到非图片内容
        if let Some(ref ct) = get_response.content_type {
            let ct_lower = ct.to_lowercase();
            if !ct_lower.starts_with("image/") {
                return Err(KeyComputeError::ProviderError(format!(
                    "Invalid Content-Type for image download: '{}' (expected image/*): {}",
                    ct, url
                )));
            }
        }
        // 注意：未返回 Content-Type 时不阻断（某些 CDN/图床不设置此头），
        // 安全依赖其余多层防御（SSRF 防护 + 大小限制 + DNS 重绑定防护）

        let bytes = get_response.body;

        // 检查大小限制
        if bytes.len() as u64 > MAX_IMAGE_SIZE {
            return Err(KeyComputeError::ProviderError(format!(
                "Image exceeds size limit ({} bytes, max {}): {}",
                bytes.len(),
                MAX_IMAGE_SIZE,
                url
            )));
        }

        Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
    }

    /// 异步解析消息中的图片（data URI + HTTP URL 下载）
    ///
    /// 返回 Ollama 原生 API 可用的纯 base64 图片列表。
    async fn resolve_message_images(
        transport: &dyn HttpTransport,
        content: &MessageContent,
    ) -> Result<Option<Vec<String>>> {
        // 先提取 data URI 图片
        let mut images = Self::extract_data_uri_images(content).unwrap_or_default();

        // 再下载 HTTP URL 图片
        if let MessageContent::Parts(parts) = content {
            for part in parts {
                if let ContentPart::ImageUrl { image_url } = part
                    && !image_url.url.starts_with("data:")
                {
                    let base64 = Self::download_image_to_base64(transport, &image_url.url).await?;
                    images.push(base64);
                }
            }
        }

        if images.is_empty() {
            Ok(None)
        } else {
            Ok(Some(images))
        }
    }

    /// 预解析请求中所有消息的图片（并行下载以降低延迟）
    ///
    /// 返回消息索引 → 图片列表的映射。
    /// 多个消息的图片 HTTP 下载会并行执行，但通过 Semaphore 限制最大并发数为 8，
    /// 防止极端场景下（大量消息 + 大量图片 URL）文件描述符或内存耗尽。
    ///
    /// 纯文本消息（不含 HTTP URL 图片）直接跳过 Semaphore 获取，避免不必要的并发限制开销。
    async fn resolve_request_images(
        transport: &dyn HttpTransport,
        request: &UpstreamRequest,
    ) -> Result<HashMap<usize, Vec<String>>> {
        /// 最大并发下载数
        const MAX_CONCURRENT_DOWNLOADS: usize = 8;

        let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_DOWNLOADS));

        // 分离处理：纯文本消息同步处理，避免不必要的 clone 和 future 开销
        let mut images_map = HashMap::new();
        let mut download_futures = Vec::new();

        for (i, msg) in request.messages.iter().enumerate() {
            if !Self::needs_image_download(&msg.content) {
                // 快速路径：纯文本或仅含 data URI，同步提取图片
                if let Some(images) = Self::extract_data_uri_images(&msg.content) {
                    images_map.insert(i, images);
                }
            } else {
                // 慢速路径：需要 HTTP 下载，clone content 进入 async block
                let permit = semaphore.clone();
                let content = msg.content.clone();
                download_futures.push(async move {
                    let _guard = permit.acquire().await.map_err(|e| {
                        KeyComputeError::ProviderError(format!(
                            "Semaphore closed during image download: {}",
                            e
                        ))
                    })?;
                    let images = Self::resolve_message_images(transport, &content).await?;
                    Ok::<_, KeyComputeError>((i, images))
                });
            }
        }

        let results = futures::future::join_all(download_futures).await;
        for result in results {
            let (i, images) = result?;
            if let Some(images) = images {
                images_map.insert(i, images);
            }
        }
        Ok(images_map)
    }

    /// 构建 OpenAI 兼容格式的请求体（用于 /v1/chat/completions 端点）
    ///
    /// 当 Ollama 端点使用 OpenAI 兼容 API 时，使用此方法构建请求。
    /// OpenAI 格式原生支持 image_url（含 HTTP URL），解决 Vision 图片透传问题。
    fn build_openai_request_body(&self, request: &UpstreamRequest) -> OpenAIRequest {
        let messages: Vec<keycompute_openai::protocol::OpenAIMessage> = request
            .messages
            .iter()
            .map(|m| keycompute_openai::protocol::OpenAIMessage {
                role: m.role.clone(),
                content: Some(convert_message_content(m.content.clone())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            })
            .collect();

        OpenAIRequest {
            model: request.model.clone(),
            messages,
            stream: Some(request.stream),
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            top_p: request.top_p,
            stop: None,
            stream_options: if request.stream {
                Some(StreamOptions {
                    include_usage: Some(true),
                })
            } else {
                None
            },
        }
    }

    fn get_endpoint(&self, request: &UpstreamRequest) -> String {
        if request.endpoint.is_empty() {
            OLLAMA_DEFAULT_ENDPOINT.to_string()
        } else {
            request.endpoint.clone()
        }
    }

    fn build_headers(&self, api_key: &str) -> Vec<(String, String)> {
        let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        if !api_key.is_empty() && api_key != "mock-api-key" {
            headers.push(("Authorization".to_string(), format!("Bearer {}", api_key)));
        }
        headers
    }

    async fn chat_internal(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<String> {
        let endpoint = self.get_endpoint(&request);
        // 使用路径后缀精确匹配，避免被包含性 URL 参数绕过
        // 如 https://evil.com/redirect?to=/v1/chat/completions 不应被误判
        let is_openai_endpoint = endpoint.ends_with("/v1/chat/completions")
            || endpoint.contains("/v1/chat/completions?");

        // 根据端点类型选择请求体格式
        let body_json = if is_openai_endpoint {
            let mut body = self.build_openai_request_body(&request);
            body.stream = Some(false);
            serde_json::to_string(&body).map_err(|e| {
                KeyComputeError::ProviderError(format!("Failed to serialize OpenAI request: {}", e))
            })?
        } else {
            // 原生 Ollama 格式：先异步解析图片（下载 HTTP URL → base64）
            let images_map = Self::resolve_request_images(transport, &request).await?;
            let body = self.build_request_body(&request, &images_map);
            let body = OllamaRequest {
                stream: Some(false),
                ..body
            };
            serde_json::to_string(&body).map_err(|e| {
                KeyComputeError::ProviderError(format!("Failed to serialize Ollama request: {}", e))
            })?
        };

        let headers = self.build_headers(request.upstream_api_key.expose());
        let response_text = transport.post_json(&endpoint, headers, body_json).await?;

        // 根据 endpoint 类型选择正确的响应解析器
        let text = if is_openai_endpoint {
            // OpenAI 兼容格式
            let openai_response: OpenAIResponse =
                serde_json::from_str(&response_text).map_err(|e| {
                    KeyComputeError::ProviderError(format!(
                        "Failed to parse OpenAI response: {}",
                        e
                    ))
                })?;
            openai_response.extract_text()
        } else {
            // 原生 Ollama 格式
            let ollama_response: OllamaResponse =
                serde_json::from_str(&response_text).map_err(|e| {
                    KeyComputeError::ProviderError(format!(
                        "Failed to parse Ollama response: {}",
                        e
                    ))
                })?;
            ollama_response.extract_text().to_string()
        };

        Ok(text)
    }

    async fn stream_chat_internal(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<StreamBox> {
        let endpoint = self.get_endpoint(&request);
        // 使用路径后缀精确匹配，避免被包含性 URL 参数绕过
        // 如 https://evil.com/redirect?to=/v1/chat/completions 不应被误判
        let is_openai_endpoint = endpoint.ends_with("/v1/chat/completions")
            || endpoint.contains("/v1/chat/completions?");

        // 根据端点类型选择请求体格式
        let body_json = if is_openai_endpoint {
            let body = self.build_openai_request_body(&request);
            serde_json::to_string(&body).map_err(|e| {
                KeyComputeError::ProviderError(format!("Failed to serialize OpenAI request: {}", e))
            })?
        } else {
            // 原生 Ollama 格式：先异步解析图片（下载 HTTP URL → base64）
            let images_map = Self::resolve_request_images(transport, &request).await?;
            let body = self.build_request_body(&request, &images_map);
            serde_json::to_string(&body).map_err(|e| {
                KeyComputeError::ProviderError(format!("Failed to serialize Ollama request: {}", e))
            })?
        };

        let headers = self.build_headers(request.upstream_api_key.expose());
        let byte_stream: ByteStream = transport.post_stream(&endpoint, headers, body_json).await?;

        // 根据 endpoint 类型选择正确的流解析器
        if is_openai_endpoint {
            // OpenAI 兼容格式（SSE），复用 openai provider 的流解析
            Ok(parse_openai_stream(byte_stream))
        } else {
            // 原生 Ollama 格式（NDJSON）
            Ok(parse_ollama_stream(byte_stream))
        }
    }
}

#[async_trait]
impl ProviderAdapter for OllamaProvider {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn supported_models(&self) -> Vec<&'static str> {
        OLLAMA_MODELS.to_vec()
    }

    async fn stream_chat(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<StreamBox> {
        if request.stream {
            self.stream_chat_internal(transport, request).await
        } else {
            let content = self.chat_internal(transport, request).await?;
            let event = StreamEvent::delta(content);
            let stream = futures::stream::once(async move { Ok(event) }).chain(
                futures::stream::once(async move { Ok(StreamEvent::done()) }),
            );
            Ok(Box::pin(stream))
        }
    }

    async fn chat(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<String> {
        self.chat_internal(transport, request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ollama_provider_name() {
        let provider = OllamaProvider::new();
        assert_eq!(provider.name(), "ollama");
    }

    #[test]
    fn test_ollama_supported_models() {
        let provider = OllamaProvider::new();
        let models = provider.supported_models();
        assert!(models.contains(&"llama3.2"));
        assert!(models.contains(&"mistral"));
        assert!(models.contains(&"gemma2"));
    }

    #[test]
    fn test_ollama_supports_model() {
        let provider = OllamaProvider::new();
        assert!(provider.supports_model("llama3.2"));
        assert!(provider.supports_model("mistral"));
        assert!(provider.supports_model("qwen2.5:7b"));
        assert!(!provider.supports_model("gpt-4o"));
    }

    #[test]
    fn test_default_endpoint() {
        assert_eq!(OLLAMA_DEFAULT_ENDPOINT, "https://ollama.com/api/chat");
    }

    #[test]
    fn test_build_request_body() {
        let provider = OllamaProvider::new();
        let request = UpstreamRequest::new("http://localhost:11434/api/chat", "", "llama3.2")
            .with_message("system", "You are helpful")
            .with_message("user", "Hello")
            .with_stream(true)
            .with_temperature(0.7);

        let images_map = HashMap::new();
        let body = provider.build_request_body(&request, &images_map);
        assert_eq!(body.model, "llama3.2");
        assert_eq!(body.system, Some("You are helpful".to_string()));
        assert_eq!(body.messages.len(), 1);
        assert_eq!(body.stream, Some(true));
    }

    #[test]
    fn test_get_endpoint_default() {
        let provider = OllamaProvider::new();
        let request = UpstreamRequest::new("", "", "llama3.2");
        assert_eq!(provider.get_endpoint(&request), OLLAMA_DEFAULT_ENDPOINT);
    }

    #[test]
    fn test_build_headers_no_auth() {
        let provider = OllamaProvider::new();
        let headers = provider.build_headers("");
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "Content-Type" && v == "application/json")
        );
        assert!(!headers.iter().any(|(k, _)| k == "Authorization"));
    }

    #[test]
    fn test_build_headers_with_auth() {
        let provider = OllamaProvider::new();
        let headers = provider.build_headers("sk-test-key");
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "Authorization" && v == "Bearer sk-test-key")
        );
    }
}
