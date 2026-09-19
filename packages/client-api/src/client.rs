//! HTTP 客户端封装
//!
//! 封装 reqwest 客户端，提供统一的请求方法和认证管理

use crate::config::ClientConfig;
use crate::error::{ClientError, Result};
use crate::retry::{Cooldowns, response_metadata};
use reqwest::{Client, Method, Request, RequestBuilder, Response};
use serde::{Serialize, de::DeserializeOwned};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// HTTP 客户端
#[derive(Debug, Clone)]
pub struct ApiClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    client: Client,
    config: ClientConfig,
    auth_token: RwLock<Option<String>>,
    cooldowns: Mutex<Cooldowns>,
}

impl std::fmt::Debug for ClientInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never include credentials or their derived cooldown keys in logs.
        f.debug_struct("ClientInner").finish_non_exhaustive()
    }
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

        let cooldowns = Mutex::new(Cooldowns::with_base_url(&config.base_url));
        Ok(Self {
            inner: Arc::new(ClientInner {
                client,
                config,
                auth_token: RwLock::new(None),
                cooldowns,
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

    /// Send an idempotent POST. The header is attached before the retryable
    /// request builder is cloned, so every transport retry uses the exact same
    /// key.
    pub async fn post_json_with_idempotency_key<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        idempotency_key: &str,
        token: Option<&str>,
    ) -> Result<T> {
        let builder = self.request_with_auth(Method::POST, path, token).await?;
        if idempotency_key.trim().is_empty() {
            return Err(ClientError::Config(
                "Idempotency key must not be empty".into(),
            ));
        }
        // This explicit API is only for endpoints with server-enforced
        // idempotency. An arbitrary header on a generic POST is insufficient.
        self.send_with_policy(
            builder
                .header("Idempotency-Key", idempotency_key)
                .json(body),
            true,
        )
        .await
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

    /// Safe reads may retry transient failures. Mutations require an explicit
    /// idempotency contract. A 429 returns immediately and cools down all
    /// matching calls instead of starting one retry loop per component.
    pub(crate) async fn send_and_parse<T: DeserializeOwned>(
        &self,
        builder: RequestBuilder,
    ) -> Result<T> {
        self.send_with_policy(builder, false).await
    }

    async fn send_with_policy<T: DeserializeOwned>(
        &self,
        builder: RequestBuilder,
        idempotent_post: bool,
    ) -> Result<T> {
        let request = builder.build().map_err(ClientError::from)?;
        let budget = Duration::from_secs(self.inner.config.timeout_secs);
        let operation = self.send_attempts(request, idempotent_post);
        #[cfg(not(target_arch = "wasm32"))]
        {
            tokio::time::timeout(budget, operation)
                .await
                .map_err(|_| ClientError::Network("Request deadline exceeded".into()))?
        }
        #[cfg(target_arch = "wasm32")]
        {
            // reqwest 0.12 owns an AbortGuard until the response body is
            // consumed; dropping this future aborts the browser fetch too.
            // Neither browser nor native cancellation can undo a server-side
            // command that already committed, hence the strict retry policy.
            let timeout = gloo_timers::future::TimeoutFuture::new(
                budget.as_millis().min(u128::from(u32::MAX)) as u32,
            );
            futures::pin_mut!(operation, timeout);
            match futures::future::select(operation, timeout).await {
                futures::future::Either::Left((result, _)) => result,
                futures::future::Either::Right(_) => {
                    Err(ClientError::Network("Request deadline exceeded".into()))
                }
            }
        }
    }

    async fn send_attempts<T: DeserializeOwned>(
        &self,
        request: Request,
        idempotent_post: bool,
    ) -> Result<T> {
        // Keep only header/URL identity for cooldown lookup, never duplicate
        // a request body just to calculate its rate-limit category.
        let identity = self
            .inner
            .client
            .request(request.method().clone(), request.url().clone())
            .headers(request.headers().clone())
            .build()
            .map_err(ClientError::from)?;
        let safe = matches!(*request.method(), Method::GET | Method::HEAD)
            || (idempotent_post && request.method() == Method::POST);
        let template = request.try_clone();
        let retries = if safe && self.inner.config.retry_enabled && template.is_some() {
            self.inner.config.max_retries.min(5)
        } else {
            0
        };
        let mut next_request = Some(request);
        for attempt in 0..=retries {
            if attempt > 0 {
                let base = 500u32.saturating_mul(1 << (attempt - 1).min(3));
                let jitter = (uuid::Uuid::new_v4().as_u128() % 251) as u32;
                Self::sleep_ms(base + jitter).await;
            }
            let blocked = self
                .inner
                .cooldowns
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remaining(&identity);
            if let Some(info) = blocked {
                return Err(ClientError::RateLimited(info));
            }
            let request = next_request
                .take()
                .or_else(|| template.as_ref().and_then(Request::try_clone))
                .ok_or_else(|| ClientError::Other("Request cannot be replayed".into()))?;
            let result = match self.inner.client.execute(request).await {
                Ok(response) => self.handle_response(response, &identity).await,
                Err(error) => Err(ClientError::from(error)),
            };
            match result {
                Err(ref error)
                    if attempt < retries
                        && matches!(
                            error,
                            ClientError::Network(_) | ClientError::ServerError(_)
                        ) => {}
                result => return result,
            }
        }
        unreachable!("the final attempt always returns")
    }

    async fn sleep_ms(ms: u32) {
        #[cfg(target_arch = "wasm32")]
        gloo_timers::future::TimeoutFuture::new(ms).await;
        #[cfg(not(target_arch = "wasm32"))]
        tokio::time::sleep(Duration::from_millis(u64::from(ms))).await;
    }

    async fn handle_response<T: DeserializeOwned>(
        &self,
        response: Response,
        identity: &Request,
    ) -> Result<T> {
        let status = response.status();
        if status.is_success() {
            return response.json::<T>().await.map_err(ClientError::from);
        }
        // Publish the cooldown as soon as headers arrive, before waiting for
        // an error body, so subsequent component requests are already fenced.
        let metadata = (status.as_u16() == 429).then(|| response_metadata(response.headers()));
        if let Some(info) = &metadata {
            self.inner
                .cooldowns
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .record(identity, info.clone());
        }
        let text = response.text().await.unwrap_or_default();
        let error = ClientError::from_status(status.as_u16(), text);
        if let Some(mut info) = metadata {
            let message = error.message();
            if !message.trim().is_empty() {
                info.message = message;
            }
            return Err(ClientError::RateLimited(info));
        }
        Err(error)
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
