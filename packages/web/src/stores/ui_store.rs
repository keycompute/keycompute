use dioxus::prelude::*;

// ToastMsg/ToastKind 已迁移到 ui 包，re-export 保持外部兼容
pub use ui::{ToastKind, ToastMsg};

/// UI 全局状态（侧边栏、Toast 通知等页面级状态）
#[derive(Clone, Copy)]
pub struct UiStore {
    /// 全局 Toast 消息
    pub toast: Signal<Option<ToastMsg>>,
}

impl UiStore {
    /// 创建新的 UiStore。
    /// 注意：Signal 必须在组件顶层创建后传入
    pub fn new(toast: Signal<Option<ToastMsg>>) -> Self {
        Self { toast }
    }

    pub fn show_info(&mut self, title: impl Into<String>) {
        let mut toast = self.toast;
        *toast.write() = Some(ToastMsg {
            kind: ToastKind::Info,
            title: title.into(),
            message: None,
        });
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(4_000).await;
            *toast.write() = None;
        });
    }

    pub fn show_success(&mut self, title: impl Into<String>) {
        let mut toast = self.toast;
        *toast.write() = Some(ToastMsg {
            kind: ToastKind::Success,
            title: title.into(),
            message: None,
        });
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(3_000).await;
            *toast.write() = None;
        });
    }

    pub fn show_error(&mut self, title: impl Into<String>) {
        let mut toast = self.toast;
        *toast.write() = Some(ToastMsg {
            kind: ToastKind::Error,
            title: title.into(),
            message: None,
        });
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(5_000).await;
            *toast.write() = None;
        });
    }

    #[allow(dead_code)]
    pub fn show_error_msg(&mut self, title: impl Into<String>, msg: impl Into<String>) {
        let mut toast = self.toast;
        *toast.write() = Some(ToastMsg {
            kind: ToastKind::Error,
            title: title.into(),
            message: Some(msg.into()),
        });
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(5_000).await;
            *toast.write() = None;
        });
    }

    #[allow(dead_code)]
    pub fn clear_toast(&mut self) {
        *self.toast.write() = None;
    }
}
