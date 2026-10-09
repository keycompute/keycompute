use dioxus::core::spawn_forever;
use dioxus::prelude::*;

// ToastMsg/ToastKind 已迁移到 ui 包，re-export 保持外部兼容
pub use ui::{ToastKind, ToastMsg};

/// UI 全局状态（侧边栏、Toast 通知等页面级状态）
#[derive(Clone, Copy)]
pub struct UiStore {
    /// 全局 Toast 消息
    pub toast: Signal<Option<ToastMsg>>,
    /// Monotonic ticket used to prevent an older expiry task from clearing a newer toast.
    toast_generation: Signal<u64>,
}

impl UiStore {
    /// 创建新的 UiStore。
    /// 注意：Signal 必须在组件顶层创建后传入
    pub fn new(toast: Signal<Option<ToastMsg>>, toast_generation: Signal<u64>) -> Self {
        Self {
            toast,
            toast_generation,
        }
    }

    pub fn show_info(&mut self, title: impl Into<String>) {
        self.show(ToastKind::Info, title.into(), None, 4_000);
    }

    pub fn show_success(&mut self, title: impl Into<String>) {
        self.show(ToastKind::Success, title.into(), None, 3_000);
    }

    pub fn show_error(&mut self, title: impl Into<String>) {
        self.show(ToastKind::Error, title.into(), None, 5_000);
    }

    #[allow(dead_code)]
    pub fn show_error_msg(&mut self, title: impl Into<String>, msg: impl Into<String>) {
        self.show(ToastKind::Error, title.into(), Some(msg.into()), 5_000);
    }

    #[allow(dead_code)]
    pub fn clear_toast(&mut self) {
        *self.toast.write() = None;
    }

    fn show(&mut self, kind: ToastKind, title: String, message: Option<String>, timeout_ms: u32) {
        let generation = {
            let mut current = self.toast_generation.write();
            *current = current.wrapping_add(1);
            *current
        };
        let mut toast = self.toast;
        *toast.write() = Some(ToastMsg {
            kind,
            title,
            message,
        });

        let current_generation = self.toast_generation;
        // A toast is global state. Keep its expiry independent of the component
        // that triggered it (for example, a modal that closes after success).
        // The generation check also prevents an older timer from clearing a
        // newer notification shown before the old timer fired.
        spawn_forever(async move {
            gloo_timers::future::TimeoutFuture::new(timeout_ms).await;
            if *current_generation.peek() == generation {
                *toast.write() = None;
            }
        });
    }
}
