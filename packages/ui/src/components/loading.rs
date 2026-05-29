use dioxus::prelude::*;

/// 旋转加载指示器
///
/// # 示例
/// ```rust,ignore
/// LoadingSpinner {}                  // 默认 24px，primary 色
/// LoadingSpinner { size: 16 }        // 小尺寸
/// ```
#[component]
pub fn LoadingSpinner(
    /// 尺寸（像素，默认 24）
    #[props(default = 24_u32)]
    size: u32,
    /// 额外 CSS 类名
    #[props(default)]
    class: String,
) -> Element {
    let style = format!("width: {size}px; height: {size}px;");
    let full_class = format!("spinner {}", class.trim());

    rsx! {
        span {
            class: "{full_class}",
            style: "{style}",
            role: "status",
            "aria-label": "加载中",
        }
    }
}

/// 全屏/区域加载蒙层
#[component]
pub fn LoadingOverlay(
    /// 是否显示
    #[props(default = true)]
    visible: bool,
    /// 提示文字
    #[props(default = "加载中...".to_string())]
    text: String,
) -> Element {
    if !visible {
        return rsx! {};
    }

    rsx! {
        div { class: "loading-overlay",
            div { class: "loading-overlay-content",
                LoadingSpinner { size: 32 }
                span { class: "loading-text", "{text}" }
            }
        }
    }
}

/// 骨架屏动画块
///
/// # 示例
/// ```rust,ignore
/// Skeleton { width: "100%", height: "20px" }
/// Skeleton { width: "60%", height: "16px" }
/// ```
#[component]
pub fn Skeleton(
    /// 宽度（CSS 值，默认 "100%"）
    #[props(default = "100%".to_string())]
    width: String,
    /// 高度（CSS 值，默认 "16px"）
    #[props(default = "16px".to_string())]
    height: String,
    /// 圆角（CSS 值，默认 "var(--radius-md)"）
    #[props(default = "var(--radius-md)".to_string())]
    border_radius: String,
    /// 额外 CSS 类名
    #[props(default)]
    class: String,
) -> Element {
    let style = format!("width: {width}; height: {height}; border-radius: {border_radius};");
    let full_class = format!("skeleton {}", class.trim());

    rsx! {
        span {
            class: "{full_class}",
            style: "{style}",
        }
    }
}

/// 卡片骨架屏（模拟卡片列表的加载状态）
#[component]
pub fn CardSkeleton(
    /// 展示几行文字骨架
    #[props(default = 3_u32)]
    lines: u32,
) -> Element {
    rsx! {
        div { class: "card skeleton-card",
            div { class: "card-body",
                Skeleton { width: "40%", height: "20px" }
                div { style: "margin-top: var(--space-md); display: flex; flex-direction: column; gap: var(--space-sm);",
                    for _ in 0..lines {
                        Skeleton { width: "100%", height: "14px" }
                    }
                }
            }
        }
    }
}
