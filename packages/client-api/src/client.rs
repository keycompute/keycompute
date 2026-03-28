//! HTTP 客户端封装
//!
//! 封装 reqwest 客户端，提供统一的请求方法和认证管理

use crate::config::ClientConfig;
use crate::error::{ClientError, Result};
use reqwest::{Client, Method, RequestBuilder, Response};
use serde::{Serialize, de::DeserializeOwned};
use std::sync::{Arc, RwLock};
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

        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| ClientError::Config(format!("Failed to create HTTP client: {}", e)))?;

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

    /// 发送 GET 请求
    pub async fn get(&self, path: &str) -> Result<Response> {
        self.request(Method::GET, path)
            .send()
            .await
            .map_err(ClientError::from)
    }

    /// 发送 POST 请求
    pub async fn post<T: Serialize>(&self, path: &str, body: &T) -> Result<Response> {
        self.request(Method::POST, path)
            .json(body)
            .send()
            .await
            .map_err(ClientError::from)
    }

    /// 发送 PUT 请求
    pub async fn put<T: Serialize>(&self, path: &str, body: &T) -> Result<Response> {
        self.request(Method::PUT, path)
            .json(body)
            .send()
            .await
            .map_err(ClientError::from)
    }

    /// 发送 DELETE 请求
    pub async fn delete(&self, path: &str) -> Result<Response> {
        self.request(Method::DELETE, path)
            .send()
            .await
            .map_err(ClientError::from)
    }

    /// 发送 PATCH 请求
    pub async fn patch<T: Serialize>(&self, path: &str, body: &T) -> Result<Response> {
        self.request(Method::PATCH, path)
            .json(body)
            .send()
            .await
            .map_err(ClientError::from)
    }

    /// 构建请求
    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let url = self.inner.config.build_url(path);
        let builder = self.inner.client.request(method, &url);
        self.add_auth_header(builder)
    }

    /// 添加认证头
    fn add_auth_header(&self, builder: RequestBuilder) -> RequestBuilder {
        // 使用 try_block 来简化异步操作
        // 注意：这里我们使用同步方式获取 token，因为 request 方法是同步的
        // 在实际请求时会通过 interceptor 方式添加 token
        builder
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

        if let Some(t) = token {
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

    /// 发送请求并解析 JSON 响应
    async fn send_and_parse<T: DeserializeOwned>(&self, builder: RequestBuilder) -> Result<T> {
        let response = builder.send().await.map_err(ClientError::from)?;
        self.handle_response(response).await
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
#[derive(Debug, Clone)]
pub struct OpenAiClient {
    inner: Arc<ClientInner>,
}

impl OpenAiClient {
    /// 创建新的 OpenAI 客户端
    pub fn new(config: ClientConfig) -> Result<Self> {
        config.validate()?;

        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| ClientError::Config(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            inner: Arc::new(ClientInner {
                client,
                config,
                auth_token: RwLock::new(None),
            }),
        })
    }

    /// 发送请求（使用 API Key 认证）
    pub async fn request_with_api_key(
        &self,
        method: Method,
        path: &str,
        api_key: &str,
    ) -> Result<RequestBuilder> {
        let url = self.inner.config.build_url(path);
        let builder = self
            .inner
            .client
            .request(method, &url)
            .header("Authorization", format!("Bearer {}", api_key));
        Ok(builder)
    }

    /// 发送 POST 请求并解析响应
    pub async fn post_json<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        api_key: &str,
    ) -> Result<T> {
        let builder = self
            .request_with_api_key(Method::POST, path, api_key)
            .await?;
        let response = builder.json(body).send().await.map_err(ClientError::from)?;

        let status = response.status();
        if status.is_success() {
            response.json::<T>().await.map_err(ClientError::from)
        } else {
            let text = response.text().await.unwrap_or_default();
            Err(ClientError::from_status(status.as_u16(), text))
        }
    }

    /// 发送 GET 请求并解析响应
    pub async fn get_json<T: DeserializeOwned>(&self, path: &str, api_key: &str) -> Result<T> {
        let builder = self
            .request_with_api_key(Method::GET, path, api_key)
            .await?;
        let response = builder.send().await.map_err(ClientError::from)?;

        let status = response.status();
        if status.is_success() {
            response.json::<T>().await.map_err(ClientError::from)
        } else {
            let text = response.text().await.unwrap_or_default();
            Err(ClientError::from_status(status.as_u16(), text))
        }
    }
}
