use dioxus::prelude::*;
use uuid::Uuid;

/// 认证状态
#[derive(Clone, PartialEq, Default)]
pub struct AuthState {
    /// 访问令牌
    pub access_token: Option<String>,
    /// 刷新令牌
    pub refresh_token: Option<String>,
    /// 是否已登录
    pub is_authenticated: bool,
    /// Changes only at login/logout, not when the same session refreshes.
    pub session_id: Uuid,
    pub token_revision: u64,
    pub persistent: bool,
}

impl AuthState {
    pub fn logged_in(access_token: String) -> Self {
        Self {
            access_token: Some(access_token),
            refresh_token: None,
            is_authenticated: true,
            session_id: Uuid::new_v4(),
            token_revision: 0,
            persistent: false,
        }
    }

    #[allow(dead_code)]
    pub fn token(&self) -> Option<&str> {
        self.access_token.as_deref()
    }
}

/// 认证状态 Store（对外暴露的 Signal 封装）
#[derive(Clone, Copy, PartialEq)]
pub struct AuthStore {
    pub state: Signal<AuthState>,
}

impl AuthStore {
    /// 创建新的 AuthStore。
    /// 注意：Signal 必须在组件顶层创建后传入，不能在此内部调用 use_signal
    pub fn new(state: Signal<AuthState>) -> Self {
        Self { state }
    }

    /// Backwards-compatible login helper that preserves the current
    /// remember-me choice. New login flows should call
    /// `login_with_persist` when the choice is explicit.
    #[allow(dead_code)]
    pub fn login(&mut self, access_token: String) {
        let persist = self.state.peek().persistent;
        self.login_with_persist(access_token, persist);
    }

    /// 登录并明确指定是否持久化保存 token
    /// - persist=true  → 写入 localStorage（关闭浏览器后仍保留）
    /// - persist=false → 写入 sessionStorage（关闭标签页/浏览器后失效）
    pub fn login_with_persist(&mut self, access_token: String, persist: bool) {
        Self::save_to_storage(&access_token, persist);
        crate::services::api_client::get_client().set_token(access_token.clone());
        // Even an identical token on an explicit login starts a new UI session.
        crate::services::api_client::get_client().invalidate_console_reads();
        let mut next = AuthState::logged_in(access_token);
        next.persistent = persist;
        *self.state.write() = next;
    }

    pub fn logout(&mut self) {
        Self::clear_storage();
        crate::services::api_client::get_client().clear_token();
        *self.state.write() = AuthState::default();
    }

    /// Clear authentication only if the complete observed login state is still
    /// current. Late bootstrap/refresh failures must not log out a newer login
    /// or a same-session token refresh that already won the race.
    pub fn logout_if_current(&mut self, observed: &AuthState) -> bool {
        let mut current = self.state.write();
        if !Self::state_matches(&current, observed) {
            return false;
        }
        Self::clear_storage();
        crate::services::api_client::get_client().clear_token();
        *current = AuthState::default();
        true
    }

    /// Non-reactive comparison for asynchronous completion guards. Callers
    /// subscribe by reading state at the start, never while completing work.
    pub fn matches(&self, observed: &AuthState) -> bool {
        Self::state_matches(&self.state.peek(), observed)
    }

    fn state_matches(current: &AuthState, observed: &AuthState) -> bool {
        current.session_id == observed.session_id
            && current.token_revision == observed.token_revision
            && current.access_token == observed.access_token
            && current.is_authenticated == observed.is_authenticated
    }

    pub fn same_session(&self, observed: &AuthState) -> bool {
        let current = self.state.peek();
        current.is_authenticated && current.session_id == observed.session_id
    }

    /// Replace only credentials, retaining this login identity and remember-me.
    pub fn refresh_if_current(&mut self, observed: &AuthState, token: String) -> bool {
        let mut state = self.state.write();
        if !Self::state_matches(&state, observed) || !observed.is_authenticated {
            return false;
        }
        Self::save_to_storage(&token, observed.persistent);
        state.access_token = Some(token);
        state.token_revision = state.token_revision.saturating_add(1);
        true
    }

    pub fn is_authenticated(&self) -> bool {
        (self.state)().is_authenticated
    }

    pub fn token(&self) -> Option<String> {
        (self.state)().access_token.clone()
    }

    #[allow(dead_code)]
    pub fn refresh_token(&self) -> Option<String> {
        (self.state)().refresh_token.clone()
    }

    pub fn load_from_storage() -> AuthState {
        #[cfg(all(not(test), target_arch = "wasm32"))]
        {
            // 优先从 localStorage 读取（“记住我”），其次 sessionStorage
            if let Some(access_token) = read_local_storage("access_token") {
                let mut state = AuthState::logged_in(access_token);
                state.persistent = true;
                return state;
            }
            if let Some(access_token) = read_session_storage("access_token") {
                return AuthState::logged_in(access_token);
            }
        }
        #[cfg(all(not(test), not(target_arch = "wasm32")))]
        {
            if let Some(access) = read_native_storage() {
                let mut state = AuthState::logged_in(access);
                state.persistent = true;
                return state;
            }
        }
        AuthState::default()
    }

    fn save_to_storage(access_token: &str, persist: bool) {
        #[cfg(test)]
        let _ = (access_token, persist);
        #[cfg(all(not(test), target_arch = "wasm32"))]
        {
            if persist {
                let _ = write_local_storage("access_token", access_token);
                // 避免两边同时有 token 导致语义不一致
                let _ = remove_session_storage("access_token");
            } else {
                let _ = write_session_storage("access_token", access_token);
                let _ = remove_local_storage("access_token");
            }
        }
        #[cfg(all(not(test), not(target_arch = "wasm32")))]
        {
            let _ = persist; // 原生平台仅文件持久化，不区分 persist
            write_native_storage(access_token);
        }
    }

    fn clear_storage() {
        #[cfg(all(not(test), target_arch = "wasm32"))]
        {
            let _ = remove_local_storage("access_token");
            let _ = remove_local_storage("refresh_token");
            let _ = remove_session_storage("access_token");
            let _ = remove_session_storage("refresh_token");
        }
        #[cfg(all(not(test), not(target_arch = "wasm32")))]
        {
            clear_native_storage();
        }
    }
}

/// 存储键命名空间前缀：隔离不同应用/子系统的浏览器存储键，避免同源部署下键名冲突
#[cfg(target_arch = "wasm32")]
fn storage_key(key: &str) -> String {
    format!("keyc_{key}")
}

#[cfg(target_arch = "wasm32")]
fn read_local_storage(key: &str) -> Option<String> {
    web_sys::window()?
        .local_storage()
        .ok()??
        .get_item(&storage_key(key))
        .ok()?
}

#[cfg(target_arch = "wasm32")]
fn write_local_storage(key: &str, value: &str) -> Option<()> {
    web_sys::window()?
        .local_storage()
        .ok()??
        .set_item(&storage_key(key), value)
        .ok()
}

#[cfg(target_arch = "wasm32")]
fn remove_local_storage(key: &str) -> Option<()> {
    web_sys::window()?
        .local_storage()
        .ok()??
        .remove_item(&storage_key(key))
        .ok()
}

#[cfg(target_arch = "wasm32")]
fn read_session_storage(key: &str) -> Option<String> {
    web_sys::window()?
        .session_storage()
        .ok()??
        .get_item(&storage_key(key))
        .ok()?
}

#[cfg(target_arch = "wasm32")]
fn write_session_storage(key: &str, value: &str) -> Option<()> {
    web_sys::window()?
        .session_storage()
        .ok()??
        .set_item(&storage_key(key), value)
        .ok()
}

#[cfg(target_arch = "wasm32")]
fn remove_session_storage(key: &str) -> Option<()> {
    web_sys::window()?
        .session_storage()
        .ok()??
        .remove_item(&storage_key(key))
        .ok()
}

// ── 非 WASM 环境（桌面端）使用系统临时目录下的 JSON 文件持久化 Token ──

/// 获取格令存储文件路径
#[cfg(all(not(test), not(target_arch = "wasm32")))]
fn native_storage_path() -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    path.push("keycompute_auth.json");
    path
}

/// 从文件读取 access_token
#[cfg(all(not(test), not(target_arch = "wasm32")))]
fn read_native_storage() -> Option<String> {
    let path = native_storage_path();
    let data = std::fs::read_to_string(&path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&data).ok()?;
    let access = parsed["access_token"].as_str()?.to_string();
    if access.is_empty() {
        return None;
    }
    Some(access)
}

/// 将 token 写入文件
#[cfg(all(not(test), not(target_arch = "wasm32")))]
fn write_native_storage(access_token: &str) {
    let path = native_storage_path();
    let content = format!(
        r#"{{"access_token":"{}"}}
"#,
        access_token
    );
    let _ = std::fs::write(&path, content);
}

/// 删除令牌文件
#[cfg(all(not(test), not(target_arch = "wasm32")))]
fn clear_native_storage() {
    let path = native_storage_path();
    let _ = std::fs::remove_file(&path);
}
