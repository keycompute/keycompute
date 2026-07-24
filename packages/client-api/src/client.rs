//! HTTP 客户端封装
//!
//! 封装 reqwest 客户端，提供统一的请求方法和认证管理

use crate::config::ClientConfig;
use crate::error::{ClientError, Result};
use reqwest::{Client, Method, RequestBuilder, Response};
use serde::{Serialize, de::DeserializeOwned};
use std::sync::{Arc, RwLock};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

/// HTTP 客户端
#[derive(Debug, Clone)]
pub struct ApiClient {
    inner: Arc<ClientInner>,
}

#[derive(Debug)]
struct ClientInner {
    client: Client,
    config: ClientConfig,
    auth_token: RwLock<Option<String>>,
}

impl ApiClient {
    /// 创建新的 API 客户端
    pub fn new(config: ClientConfig) -> Result<Self> {
        config.validate()?;

        let client = {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let mut builder =
                    Client::builder().timeout(Duration::from_secs(config.timeout_secs));
                // 绕过系统代理，避免连接本地服务（如测试 Mock 服务器）时被代理拦截
                if config.no_proxy {
                    builder = builder.no_proxy();
                }
                builder.build().map_err(|e| {
                    ClientError::Config(format!("Failed to create HTTP client: {}", e))
                })?
            }
            #[cfg(target_arch = "wasm32")]
            {
                Client::builder().build().map_err(|e| {
                    ClientError::Config(format!("Failed to create HTTP client: {}", e))
                })?
            }
        };

        Ok(Self {
            inner: Arc::new(ClientInner {
                client,
                config,
                auth_token: RwLock::new(None),
            }),
        })
    }

    /// 设置认证 Token
    pub fn set_token(&self, token: impl Into<String>) {
        let mut guard = self.inner.auth_token.write().expect("RwLock poisoned");
        *guard = Some(token.into());
    }

    /// 清除认证 Token
    pub fn clear_token(&self) {
        let mut guard = self.inner.auth_token.write().expect("RwLock poisoned");
        *guard = None;
    }

    /// 获取当前 Token
    pub fn get_token(&self) -> Option<String> {
        let guard = self.inner.auth_token.read().expect("RwLock poisoned");
        guard.clone()
    }

    /// 检查是否已认证
    pub fn is_authenticated(&self) -> bool {
        self.get_token().is_some()
    }

    /// 发送请求（带认证）
    pub async fn request_with_auth(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
    ) -> Result<RequestBuilder> {
        let url = self.inner.config.build_url(path);
        let mut builder = self.inner.client.request(method, &url);

        // 优先使用传入的 token，否则使用内部存储的 token
        if let Some(t) = token {
            builder = builder.header("Authorization", format!("Bearer {}", t));
        } else if let Some(ref t) = self.get_token() {
            builder = builder.header("Authorization", format!("Bearer {}", t));
        }

        Ok(builder)
    }

    /// 发送 GET 请求并解析响应
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        token: Option<&str>,
    ) -> Result<T> {
        let builder = self.request_with_auth(Method::GET, path, token).await?;
        self.send_and_parse(builder).await
    }

    /// 发送 POST 请求并解析响应
    pub async fn post_json<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        token: Option<&str>,
    ) -> Result<T> {
        let builder = self.request_with_auth(Method::POST, path, token).await?;
        self.send_and_parse(builder.json(body)).await
    }

    /// 发送 PUT 请求并解析响应
    pub async fn put_json<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        token: Option<&str>,
    ) -> Result<T> {
        let builder = self.request_with_auth(Method::PUT, path, token).await?;
        self.send_and_parse(builder.json(body)).await
    }

    /// 发送 DELETE 请求并解析响应
    pub async fn delete_json<T: DeserializeOwned>(
        &self,
        path: &str,
        token: Option<&str>,
    ) -> Result<T> {
        let builder = self.request_with_auth(Method::DELETE, path, token).await?;
        self.send_and_parse(builder).await
    }

    /// 发送请求并解析 JSON 响应（含重试逻辑）
    ///
    /// 重试条件：网络/连接错误、服务器 5xx、限流 429
    /// 退避策略：指数退避，初始延迟 500ms，最大 4s（第 i 次 = 500ms × 2^i）
    /// 每次重试使用 `try_clone` 克隆 builder，无需外部依赖
    pub(crate) async fn send_and_parse<T: DeserializeOwned>(
        &self,
        builder: RequestBuilder,
    ) -> Result<T> {
        let max_retries = if self.inner.config.retry_enabled {
            self.inner.config.max_retries
        } else {
            0
        };

        // 带 streaming body 的 builder 无法克隆，直接发送不重试
        if max_retries > 0 && builder.try_clone().is_none() {
            let response = builder.send().await.map_err(ClientError::from)?;
            return self.handle_response(response).await;
        }

        let mut last_err: Option<ClientError> = None;
        for attempt in 0..=max_retries {
            // 在第 2 次及以后的尝试前，加入指数退避延迟
            if attempt > 0 {
                // 延迟时间：500ms, 1000ms, 2000ms, 最大 4000ms
                let delay_ms = (500u64 * (1u64 << (attempt - 1).min(3))) as u32;
                Self::sleep_ms(delay_ms).await;
            }

            // 每次使用克隆的 builder，保留原始供后续重试
            let req = match builder.try_clone() {
                Some(cloned) => cloned,
                None => break,
            };

            match req.send().await.map_err(ClientError::from) {
                Ok(response) => match self.handle_response::<T>(response).await {
                    Ok(result) => return Ok(result),
                    Err(e) if self.should_retry(&e) && attempt < max_retries => {
                        last_err = Some(e);
                        continue;
                    }
                    Err(e) => return Err(e),
                },
                Err(e) if self.should_retry(&e) && attempt < max_retries => {
                    last_err = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(last_err.unwrap_or(ClientError::Other(
            "Request failed after retries".to_string(),
        )))
    }

    /// 跨平台异步等待：WASM 使用 gloo_timers，native 使用 tokio
    async fn sleep_ms(ms: u32) {
        #[cfg(target_arch = "wasm32")]
        {
            gloo_timers::future::TimeoutFuture::new(ms).await;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            tokio::time::sleep(Duration::from_millis(ms as u64)).await;
        }
    }

    /// 判断错误是否值得重试
    fn should_retry(&self, err: &ClientError) -> bool {
        matches!(
            err,
            ClientError::Network(_) | ClientError::ServerError(_) | ClientError::RateLimited(_)
        )
    }

    /// 处理响应
    async fn handle_response<T: DeserializeOwned>(&self, response: Response) -> Result<T> {
        let status = response.status();

        if status.is_success() {
            response.json::<T>().await.map_err(ClientError::from)
        } else {
            let text = response.text().await.unwrap_or_default();
            Err(ClientError::from_status(status.as_u16(), text))
        }
    }

    /// 获取配置
    pub fn config(&self) -> &ClientConfig {
        &self.inner.config
    }
}

/// 用于 OpenAI 兼容 API 的客户端（使用 API Key 而非 Bearer Token）
///
/// 内部复用 `ApiClient` 的 `send_and_parse` 重试逻辑。
#[derive(Debug, Clone)]
pub struct OpenAiClient {
    /// 内部复用标准 ApiClient，仅认证头格式不同
    api_client: ApiClient,
}

impl OpenAiClient {
    /// 创建新的 OpenAI 客户端
    pub fn new(config: ClientConfig) -> Result<Self> {
        let api_client = ApiClient::new(config)?;
        Ok(Self { api_client })
    }

    /// 发送请求（使用 API Key 认证）
    pub async fn request_with_api_key(
        &self,
        method: Method,
        path: &str,
        api_key: &str,
    ) -> Result<RequestBuilder> {
        let url = self.api_client.inner.config.build_url(path);
        let builder = self
            .api_client
            .inner
            .client
            .request(method, &url)
            .header("Authorization", format!("Bearer {}", api_key));
        Ok(builder)
    }

    /// 发送 POST 请求并解析响应（含重试）
    pub async fn post_json<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        api_key: &str,
    ) -> Result<T> {
        let builder = self
            .request_with_api_key(Method::POST, path, api_key)
            .await?;
        self.api_client.send_and_parse(builder.json(body)).await
    }

    /// 发送 GET 请求并解析响应（含重试）
    pub async fn get_json<T: DeserializeOwned>(&self, path: &str, api_key: &str) -> Result<T> {
        let builder = self
            .request_with_api_key(Method::GET, path, api_key)
            .await?;
        self.api_client.send_and_parse(builder).await
    }
}
