use dioxus::prelude::*;

/// Toast 消息类型
#[derive(Clone, PartialEq)]
pub struct ToastMsg {
    pub kind: ToastKind,
    pub title: String,
    pub message: Option<String>,
}

/// Toast 级别
#[derive(Clone, PartialEq)]
pub enum ToastKind {
    Success,
    Error,
    Warning,
    Info,
}

impl ToastKind {
    pub fn css_class(&self) -> &'static str {
        match self {
            Self::Success => "toast toast-success",
            Self::Error => "toast toast-error",
            Self::Warning => "toast toast-warning",
            Self::Info => "toast toast-info",
        }
    }
}

/// 全局 Toast 通知组件
///
/// 通过 `toast` Signal 控制显示，值为 `None` 时不渲染任何内容。
///
/// # 示例
/// ```rust,ignore
/// let toast: Signal<Option<ToastMsg>> = use_signal(|| None);
/// rsx! {
///     Toast { toast }
/// }
/// ```
#[component]
pub fn Toast(toast: Signal<Option<ToastMsg>>) -> Element {
    if let Some(ref msg) = *toast.read() {
        let kind_class = match msg.kind {
            ToastKind::Success => "toast-success",
            ToastKind::Error => "toast-error",
            ToastKind::Info => "toast-info",
            ToastKind::Warning => "toast-warning",
        };
        rsx! {
            div {
                class: "toast {kind_class}",
                p { class: "toast-title", "{msg.title}" }
                if let Some(ref text) = msg.message {
                    p { class: "toast-message", "{text}" }
                }
            }
        }
    } else {
        rsx! {}
    }
}
