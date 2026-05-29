use dioxus::prelude::*;

/// 提示横幅变体
#[derive(Clone, PartialEq, Default)]
pub enum AlertVariant {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

/// 内联提示横幅组件
///
/// # 示例
/// ```rust,ignore
/// Alert { variant: AlertVariant::Success, "操作成功！" }
/// Alert {
///     variant: AlertVariant::Warning,
///     title: "余额不足",
///     "当前余额低于最小充值阈值，请及时充值。"
/// }
/// ```
#[component]
pub fn Alert(
    /// 变体（默认 Info）
    #[props(default)]
    variant: AlertVariant,
    /// 标题（可选，若提供则显示在内容上方）
    #[props(default)]
    title: String,
    /// 是否可关闭（显示 ✕ 按钮）
    #[props(default = false)]
    dismissible: bool,
    /// 关闭回调
    #[props(default)]
    ondismiss: EventHandler<()>,
    /// 额外 CSS 类名
    #[props(default)]
    class: String,
    /// 内容
    children: Element,
) -> Element {
    let (variant_class, icon) = match variant {
        AlertVariant::Info => ("alert-info", "ℹ"),
        AlertVariant::Success => ("alert-success", "✓"),
        AlertVariant::Warning => ("alert-warning", "⚠"),
        AlertVariant::Error => ("alert-error", "✕"),
    };

    let full_class = format!("alert {variant_class} {}", class.trim());

    rsx! {
        div { class: "{full_class}", role: "alert",
            span { class: "alert-icon", "{icon}" }
            div { class: "alert-content",
                if !title.is_empty() {
                    p { class: "alert-title", "{title}" }
                }
                div { class: "alert-body",
                    {children}
                }
            }
            if dismissible {
                button {
                    class: "alert-close btn btn-ghost btn-sm",
                    r#type: "button",
                    "aria-label": "关闭",
                    onclick: move |_| ondismiss.call(()),
                    "✕"
                }
            }
        }
    }
}
