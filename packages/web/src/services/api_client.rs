#[cfg(not(target_arch = "wasm32"))]
use std::sync::LazyLock;

use client_api::api::auth::RefreshTokenRequest;
use client_api::error::{ClientError, Result};
use client_api::{ApiClient, AuthApi, ClientConfig};
use dioxus::prelude::ReadableExt;
use futures::FutureExt;
#[cfg(not(target_arch = "wasm32"))]
use futures::future::BoxFuture;
#[cfg(target_arch = "wasm32")]
use futures::future::LocalBoxFuture;

use crate::stores::auth_store::{AuthState, AuthStore};

/// 全局单例 API 客户端
/// ApiClient 内部持有 Arc，Clone 只是增加引用计数，开销极低
fn build_client() -> ApiClient {
    let base_url = option_env!("API_BASE_URL").unwrap_or("").to_string();
    let config = ClientConfig::new(base_url).with_console_display_cache(true);
    ApiClient::new(config).expect("Failed to create API client")
}

#[cfg(not(target_arch = "wasm32"))]
static CLIENT: LazyLock<ApiClient> = LazyLock::new(build_client);

#[cfg(target_arch = "wasm32")]
thread_local! {
    static CLIENT: std::cell::OnceCell<ApiClient> = const { std::cell::OnceCell::new() };
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct RefreshKey {
    login: uuid::Uuid,
    revision: u64,
    token: String,
}

struct RefreshHandle {
    future: futures::future::Shared<RefreshFuture>,
}

#[cfg(target_arch = "wasm32")]
type RefreshFuture = LocalBoxFuture<'static, Result<String>>;
#[cfg(not(target_arch = "wasm32"))]
type RefreshFuture = BoxFuture<'static, Result<String>>;

/// Weak storage cannot keep abandoned refresh HTTP requests alive. Keeping
/// the handle in every caller permits cancellation of one waiter, not others.
#[derive(Default)]
struct RefreshCoordinator {
    flights:
        std::sync::Mutex<std::collections::HashMap<RefreshKey, std::sync::Weak<RefreshHandle>>>,
}

impl RefreshCoordinator {
    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
    fn run(&self, key: RefreshKey, future: RefreshFuture) -> RefreshFuture {
        let mut flights = self.flights.lock().unwrap_or_else(|e| e.into_inner());
        flights.retain(|_, handle| handle.strong_count() != 0);
        let handle = if let Some(handle) = flights.get(&key).and_then(std::sync::Weak::upgrade) {
            handle
        } else {
            if flights.len() >= 16 {
                return box_refresh_future(async {
                    Err(ClientError::ServiceUnavailable(
                        "登录刷新繁忙，请稍后重试".into(),
                    ))
                });
            }
            let handle = std::sync::Arc::new(RefreshHandle {
                future: future.shared(),
            });
            flights.insert(key, std::sync::Arc::downgrade(&handle));
            handle
        };
        drop(flights);
        box_refresh_future(async move { handle.future.clone().await })
    }
}

#[cfg(not(target_arch = "wasm32"))]
static REFRESH: LazyLock<RefreshCoordinator> = LazyLock::new(RefreshCoordinator::default);
#[cfg(target_arch = "wasm32")]
thread_local! {
    static REFRESH: RefreshCoordinator = RefreshCoordinator::default();
}

#[cfg(target_arch = "wasm32")]
fn box_refresh_future<F>(future: F) -> RefreshFuture
where
    F: std::future::Future<Output = Result<String>> + 'static,
{
    future.boxed_local()
}
#[cfg(not(target_arch = "wasm32"))]
fn box_refresh_future<F>(future: F) -> RefreshFuture
where
    F: std::future::Future<Output = Result<String>> + Send + 'static,
{
    future.boxed()
}

fn refresh_singleflight(observed: AuthState) -> RefreshFuture {
    let token = observed.access_token.unwrap_or_default();
    let key = RefreshKey {
        login: observed.session_id,
        revision: observed.token_revision,
        token: token.clone(),
    };
    let client = get_client();
    let future = box_refresh_future(async move {
        let req = RefreshTokenRequest::new(token);
        let response = AuthApi::new(&client).refresh_token(&req).await?;
        if response.access_token.is_empty() {
            return Err(ClientError::InvalidResponse("刷新令牌响应为空".into()));
        }
        Ok(response.access_token)
    });
    #[cfg(not(target_arch = "wasm32"))]
    {
        REFRESH.run(key, future)
    }
    #[cfg(target_arch = "wasm32")]
    {
        REFRESH.with(|coordinator| coordinator.run(key, future))
    }
}

/// 获取全局 API 客户端实例（廉价克隆，仅增加 Arc 引用计数）
pub fn get_client() -> ApiClient {
    #[cfg(not(target_arch = "wasm32"))]
    {
        CLIENT.clone()
    }
    #[cfg(target_arch = "wasm32")]
    {
        CLIENT.with(|client| client.get_or_init(build_client).clone())
    }
}

/// 归一化配置的 API 基址到根路径（去掉已知 API 家族后缀）。
fn normalize_api_root(configured: &str) -> String {
    let mut root = configured.trim_end_matches('/');
    loop {
        // 按最长后缀优先裁剪；用 strip_suffix 而非 trim_end_matches，
        // 避免重复匹配同一后缀时误吞 `api/v1/v1` 这类前缀
        let stripped = root
            .strip_suffix("/api/v1")
            .or_else(|| root.strip_suffix("/auth"))
            .or_else(|| root.strip_suffix("/nt/v1"))
            .or_else(|| root.strip_suffix("/pt/v1"))
            .or_else(|| root.strip_suffix("/v1"));
        match stripped {
            Some(next) => root = next.trim_end_matches('/'),
            None => return root.to_string(),
        }
    }
}

/// 获取对外展示用的 API 根路径（不含 /v1 后缀）
///
/// - 如果配置了绝对 `API_BASE_URL`，优先使用配置值并归一化到根路径
/// - 如果当前是同域反代部署（`API_BASE_URL=""`），在浏览器中读取当前站点 origin
///
/// Anthropic SDK 会在 base_url 后自行追加 `/v1/messages`，快速示例需用根路径；
/// OpenAI 兼容端点需要 `/v1` 后缀（见 `public_openai_api_base_url`）。
pub fn public_api_root_url() -> String {
    let client = get_client();
    let configured = client.config().base_url.trim_end_matches('/');

    if configured.is_empty() {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(origin) =
                web_sys::window().and_then(|window| window.location().origin().ok())
            {
                return origin.trim_end_matches('/').to_string();
            }
        }

        return "http://localhost:8080".to_string();
    }

    normalize_api_root(configured)
}

/// 为根路径追加 OpenAI 兼容的 `/v1` 后缀：幂等地处理尾斜杠与已含 `/v1` 的情况。
fn append_v1(root: &str) -> String {
    let root = root.trim_end_matches('/');
    if root.ends_with("/v1") {
        root.to_string()
    } else {
        format!("{root}/v1")
    }
}

/// 获取对外展示用的 OpenAI 兼容 API 基址（以 `/v1` 结尾）
///
/// - 如果配置了绝对 `API_BASE_URL`，优先使用配置值并归一化到 `/v1`
/// - 如果当前是同域反代部署（`API_BASE_URL=""`），在浏览器中读取当前站点 origin
pub fn public_openai_api_base_url() -> String {
    append_v1(&public_api_root_url())
}

/// Refresh only inside the same login session, then replay a rejected request
/// once. Neither a late 401 nor a successful stale response may cross a login.
pub async fn with_auto_refresh<F, Fut, T>(auth_store: AuthStore, f: F) -> Result<T>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    with_auto_refresh_using(auth_store, get_client(), f, refresh_singleflight).await
}

fn session_changed() -> ClientError {
    ClientError::Other("登录状态已变更，请重新操作".into())
}

/// Shared by the real UI and deterministic Dioxus race tests. The injected
/// refresh transport does not alter the session/command replay decision.
async fn with_auto_refresh_using<F, Fut, R, T>(
    mut auth_store: AuthStore,
    client: ApiClient,
    f: F,
    refresh: R,
) -> Result<T>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
    R: Fn(AuthState) -> RefreshFuture,
{
    // Always read the Signal: a populated HTTP-client token is not a substitute
    // for observing reactive UI login state.
    let observed = (auth_store.state)();
    let token = observed
        .access_token
        .clone()
        .filter(|_| observed.is_authenticated)
        .ok_or_else(|| ClientError::Unauthorized("请先登录".into()))?;
    let result = f(token).await;
    if !auth_store.same_session(&observed) {
        return Err(session_changed());
    }
    if !matches!(result, Err(ClientError::Unauthorized(_))) {
        return result;
    }

    let next_token = if !auth_store.matches(&observed) {
        // A concurrent refresh, not a new login (checked above), already won.
        auth_store
            .state
            .peek()
            .access_token
            .clone()
            .ok_or_else(session_changed)?
    } else {
        let refreshed = refresh(observed.clone()).await;
        if !auth_store.same_session(&observed) {
            return Err(session_changed());
        }
        if !auth_store.matches(&observed) {
            auth_store
                .state
                .peek()
                .access_token
                .clone()
                .ok_or_else(session_changed)?
        } else {
            match refreshed {
                Ok(token) if !token.is_empty() => {
                    if !auth_store.refresh_if_current(&observed, token.clone()) {
                        return Err(session_changed());
                    }
                    client.set_token(token.clone());
                    token
                }
                Ok(_) => return Err(ClientError::InvalidResponse("刷新令牌响应为空".into())),
                Err(ClientError::Unauthorized(_)) => {
                    // Logout is another exact CAS. A same-session refresh
                    // that wins between the check above and this branch must
                    // be allowed to supply the replay token instead.
                    if auth_store.logout_if_current(&observed) {
                        client.clear_token();
                        return Err(ClientError::Unauthorized("登录已过期，请重新登录".into()));
                    }
                    if !auth_store.same_session(&observed) {
                        return Err(session_changed());
                    }
                    auth_store
                        .state
                        .peek()
                        .access_token
                        .clone()
                        .ok_or_else(session_changed)?
                }
                Err(error) => return Err(error),
            }
        }
    };
    let replay = f(next_token).await;
    if !auth_store.same_session(&observed) {
        return Err(session_changed());
    }
    replay
}

/// 将 ClientError 转为跟随当前界面语言的安全提示文本。
#[allow(dead_code)]
pub fn localize_error(i18n: crate::i18n::I18n, err: &client_api::error::ClientError) -> String {
    use client_api::error::ClientError;
    let key = match err {
        ClientError::Unauthorized(_) => "error.unauthorized",
        ClientError::TenantSelectionRequired(_) => "error.tenant_selection_required",
        ClientError::Forbidden(_) => "error.forbidden",
        ClientError::NotFound(_) => "error.not_found",
        ClientError::RateLimited(_) => "error.rate_limited",
        ClientError::Verification(_) => "error.verification",
        ClientError::Network(_) => "error.network",
        ClientError::ServerError(_) => "error.server",
        ClientError::ServiceUnavailable(_) => "error.service_unavailable",
        ClientError::Serialization(_) | ClientError::InvalidResponse(_) => "error.invalid_response",
        ClientError::Config(_) => "error.config",
        ClientError::Http(msg) => {
            if msg.contains("400") {
                "error.bad_request"
            } else if msg.contains("409") {
                "error.conflict"
            } else {
                "error.request_failed"
            }
        }
        ClientError::Other(_) => "error.request_failed",
    };
    i18n.t(key).to_string()
}

/// 优先使用后端返回的业务消息；如消息过于底层，再回退到本地友好文案。
pub fn user_error_message(i18n: crate::i18n::I18n, err: &client_api::error::ClientError) -> String {
    let message = err.message();
    if message.trim().is_empty() {
        return localize_error(i18n, err);
    }

    match err {
        ClientError::TenantSelectionRequired(_) => localize_error(i18n, err),
        ClientError::Network(_)
        | ClientError::Serialization(_)
        | ClientError::InvalidResponse(_)
        | ClientError::Config(_)
        | ClientError::Http(_)
        | ClientError::ServerError(_)
        | ClientError::ServiceUnavailable(_) => localize_error(i18n, err),
        _ => message,
    }
}

#[cfg(test)]
mod tests {
    use super::{append_v1, normalize_api_root, user_error_message};
    use crate::i18n::{I18n, Lang};
    use client_api::ClientError;

    #[test]
    fn transport_and_deserialization_details_are_not_rendered_to_users() {
        let error = ClientError::Http(
            "HTTP 400: Failed to deserialize query string: missing field `tenant_id`".into(),
        );
        let zh = user_error_message(I18n::new(Lang::Zh), &error);
        let en = user_error_message(I18n::new(Lang::En), &error);
        assert_eq!(zh, "请求参数错误，请检查输入");
        assert_eq!(en, "The request is invalid. Check the input and try again.");
        for message in [zh, en] {
            assert!(!message.contains("tenant_id"));
            assert!(!message.contains("deserialize"));
        }
    }

    #[test]
    fn normalize_api_root_strips_known_suffixes() {
        assert_eq!(
            normalize_api_root("http://gw.example.com/v1"),
            "http://gw.example.com"
        );
        assert_eq!(
            normalize_api_root("http://gw.example.com/api/v1"),
            "http://gw.example.com"
        );
        assert_eq!(
            normalize_api_root("http://gw.example.com/auth"),
            "http://gw.example.com"
        );
        assert_eq!(
            normalize_api_root("http://gw.example.com/"),
            "http://gw.example.com"
        );
        assert_eq!(
            normalize_api_root("http://gw.example.com"),
            "http://gw.example.com"
        );
        assert_eq!(
            normalize_api_root("http://localhost:8080/v1"),
            "http://localhost:8080"
        );
        // 畸形双后缀：不得吞掉 `/api` 前缀
        assert_eq!(
            normalize_api_root("http://gw.example.com/v1/v1"),
            "http://gw.example.com"
        );
        assert_eq!(
            normalize_api_root("http://gw.example.com/api/v1/v1"),
            "http://gw.example.com"
        );
        assert_eq!(
            normalize_api_root("https://gw.example.com/console/nt/v1"),
            "https://gw.example.com/console"
        );
        assert_eq!(
            normalize_api_root("https://gw.example.com/console/pt/v1"),
            "https://gw.example.com/console"
        );
    }

    /// 空串输入保持空串返回（调用方保证不会传入空串，此处锁定防御性行为）
    #[test]
    fn normalize_api_root_handles_empty_input() {
        assert_eq!(normalize_api_root(""), "");
    }

    #[test]
    fn append_v1_is_idempotent_and_handles_trailing_slash() {
        assert_eq!(
            append_v1("http://gw.example.com"),
            "http://gw.example.com/v1"
        );
        assert_eq!(
            append_v1("http://gw.example.com/"),
            "http://gw.example.com/v1"
        );
        assert_eq!(
            append_v1("http://gw.example.com/v1"),
            "http://gw.example.com/v1"
        );
        assert_eq!(
            append_v1("http://localhost:8080"),
            "http://localhost:8080/v1"
        );
    }

    /// normalize 到根路径后追加 /v1，等价于旧版 public_openai_api_base_url 的行为
    #[test]
    fn normalized_root_plus_v1_matches_previous_openai_base() {
        assert_eq!(
            append_v1(&normalize_api_root("http://gw.example.com/v1")),
            "http://gw.example.com/v1"
        );
        assert_eq!(
            append_v1(&normalize_api_root("http://gw.example.com/api/v1")),
            "http://gw.example.com/v1"
        );
        assert_eq!(
            append_v1(&normalize_api_root("http://gw.example.com")),
            "http://gw.example.com/v1"
        );
        assert_eq!(
            append_v1(&normalize_api_root("http://gw.example.com/auth")),
            "http://gw.example.com/v1"
        );
        assert_eq!(
            append_v1(&normalize_api_root("http://localhost:8080/v1")),
            "http://localhost:8080/v1"
        );
        assert_eq!(
            append_v1(&normalize_api_root("https://gw.example.com/console/nt/v1")),
            "https://gw.example.com/console/v1"
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "api_client_session_tests.rs"]
mod session_tests;
