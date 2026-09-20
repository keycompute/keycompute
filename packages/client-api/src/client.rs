//! HTTP 客户端封装
//!
//! 封装 reqwest 客户端，提供统一的请求方法和认证管理

use crate::config::ClientConfig;
use crate::error::{ClientError, Result};
use crate::query_cache::{
    JSON_REPRESENTATION, QueryCache, box_cache_future, cache_key, is_allowlisted,
};
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
    session: RwLock<SessionState>,
    cooldowns: Mutex<Cooldowns>,
    query_cache: QueryCache,
}

#[derive(Default)]
struct SessionState {
    token: Option<String>,
    generation: u64,
}

impl std::fmt::Debug for ClientInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never include credentials or their derived cooldown keys in logs.
        f.debug_struct("ClientInner").finish_non_exhaustive()
    }
}

impl ApiClient {
    /// 创建新的 API 客户端
    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
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
                session: RwLock::new(SessionState::default()),
                cooldowns,
                query_cache: QueryCache::default(),
            }),
        })
    }

    /// 设置认证 Token
    pub fn set_token(&self, token: impl Into<String>) {
        let token = token.into();
        let changed = {
            let mut session = self.inner.session.write().expect("RwLock poisoned");
            if session.token.as_deref() == Some(token.as_str()) {
                false
            } else {
                session.token = Some(token);
                session.generation = session.generation.saturating_add(1);
                true
            }
        };
        if changed {
            self.inner.query_cache.invalidate_all();
        }
    }

    /// 清除认证 Token
    pub fn clear_token(&self) {
        {
            let mut session = self.inner.session.write().expect("RwLock poisoned");
            session.token = None;
            session.generation = session.generation.saturating_add(1);
        }
        self.inner.query_cache.invalidate_all();
    }

    /// 获取当前 Token
    pub fn get_token(&self) -> Option<String> {
        self.inner
            .session
            .read()
            .expect("RwLock poisoned")
            .token
            .clone()
    }

    /// 检查是否已认证
    pub fn is_authenticated(&self) -> bool {
        self.get_token().is_some()
    }

    /// Monotonic session fence used by cache keys and refresh CAS checks.
    pub fn session_generation(&self) -> u64 {
        self.inner
            .session
            .read()
            .expect("RwLock poisoned")
            .generation
    }

    /// Read the token and generation under one lock for refresh CAS logic.
    pub fn session_snapshot(&self) -> (Option<String>, u64) {
        let session = self.inner.session.read().expect("RwLock poisoned");
        (session.token.clone(), session.generation)
    }

    /// Replace a token only if the observed token and session generation are
    /// still current.  A late refresh must never overwrite a newer login.
    pub fn compare_and_set_token(
        &self,
        expected_token: &str,
        expected_generation: u64,
        token: impl Into<String>,
    ) -> bool {
        self.compare_and_set_session_token(Some(expected_token), expected_generation, token)
    }

    /// Replace a token only if the complete session snapshot is still current.
    /// `None` supports the short window where a store has a restored token but
    /// the shared HTTP client has not yet been initialized.
    pub fn compare_and_set_session_token(
        &self,
        expected_token: Option<&str>,
        expected_generation: u64,
        token: impl Into<String>,
    ) -> bool {
        {
            let mut session = self.inner.session.write().expect("RwLock poisoned");
            if session.generation != expected_generation
                || session.token.as_deref() != expected_token
            {
                return false;
            }
            session.token = Some(token.into());
            session.generation = session.generation.saturating_add(1);
        }
        self.inner.query_cache.invalidate_all();
        true
    }

    /// Clear only the session that a refresh failure observed.
    pub fn clear_token_if_current(
        &self,
        expected_token: Option<&str>,
        expected_generation: u64,
    ) -> bool {
        let cleared = {
            let mut session = self.inner.session.write().expect("RwLock poisoned");
            if session.generation != expected_generation
                || session.token.as_deref() != expected_token
            {
                false
            } else {
                session.token = None;
                session.generation = session.generation.saturating_add(1);
                true
            }
        };
        if cleared {
            self.inner.query_cache.invalidate_all();
        }
        cleared
    }

    /// Invalidate all cached console display reads, normally after a mutation
    /// or an identity/session transition.
    pub fn invalidate_console_reads(&self) {
        self.inner.query_cache.invalidate_all();
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
        if self.inner.config.console_display_cache && is_allowlisted(path) {
            return self.get_json_cached(path, token).await;
        }
        self.get_json_fresh(path, token).await
    }

    /// Fetch a network response without reading or populating the client
    /// display cache. The server endpoint still defines data freshness.
    pub async fn get_json_fresh<T: DeserializeOwned>(
        &self,
        path: &str,
        token: Option<&str>,
    ) -> Result<T> {
        let builder = self.request_with_auth(Method::GET, path, token).await?;
        self.send_and_parse(builder).await
    }

    async fn get_json_cached<T: DeserializeOwned>(
        &self,
        path: &str,
        token: Option<&str>,
    ) -> Result<T> {
        let (session_token, generation) = self.session_snapshot();
        let token = token
            .map(str::to_owned)
            .or(session_token)
            .unwrap_or_default();
        let url = self.inner.config.build_url(path);
        let Some(key) = cache_key(&url, &token, generation, JSON_REPRESENTATION) else {
            return self.get_json_fresh(path, Some(&token)).await;
        };
        let client = self.clone();
        let path = path.to_owned();
        let token_for_request = token.clone();
        let future = self
            .inner
            .query_cache
            .get_or_start(key, move |state, key, id, epoch| {
                box_cache_future(async move {
                    let started = web_time::Instant::now();
                    let mut result = client
                        .get_json_fresh::<serde_json::Value>(&path, Some(&token_for_request))
                        .await;
                    // Do not renew a server snapshot's remaining freshness at L1.
                    // Subtract the whole round trip conservatively, including retry waits.
                    if let Ok(value) = &mut result
                        && let Some(remaining) = value
                            .get("cache_max_age_ms")
                            .and_then(serde_json::Value::as_u64)
                    {
                        value["cache_max_age_ms"] =
                            serde_json::Value::from(remaining.saturating_sub(
                                started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                            ));
                    }
                    crate::query_cache::QueryCache::finish(&state, &key, id, epoch, &result)
                })
            });
        let result = future.await;
        if self.session_generation() != generation && result.is_ok() {
            return Err(ClientError::Other(
                "console read session changed before delivery".into(),
            ));
        }
        result.and_then(|value| {
            serde_json::from_value(value)
                .map_err(|error| ClientError::Serialization(error.to_string()))
        })
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
        let mutation = !matches!(*request.method(), Method::GET | Method::HEAD);
        // The guard also invalidates on timeout, early return and cancellation.
        // A dropped HTTP command may still commit on the server.
        let _mutation_fence = mutation.then(|| self.inner.query_cache.mutation_fence());
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

    /// Messages uses the same platform key, with its required protocol header.
    /// Mutating generation requests never opt into automatic retry.
    pub async fn post_messages_json<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        api_key: &str,
    ) -> Result<T> {
        let builder = self
            .request_with_api_key(Method::POST, path, api_key)
            .await?
            .header("anthropic-version", "2023-06-01")
            .json(body);
        self.api_client.send_and_parse(builder).await
    }

    /// 发送 GET 请求并解析响应（含重试）
    pub async fn get_json<T: DeserializeOwned>(&self, path: &str, api_key: &str) -> Result<T> {
        let builder = self
            .request_with_api_key(Method::GET, path, api_key)
            .await?;
        self.api_client.send_and_parse(builder).await
    }
}
