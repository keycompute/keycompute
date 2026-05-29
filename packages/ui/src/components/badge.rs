use dioxus::prelude::*;

/// 徽章变体
#[derive(Clone, PartialEq, Default)]
pub enum BadgeVariant {
    #[default]
    Neutral,
    Success,
    Warning,
    Error,
    Info,
}

/// 状态徽章组件
///
/// # 示例
/// ```rust,ignore
/// Badge { variant: BadgeVariant::Success, "活跃" }
/// Badge { variant: BadgeVariant::Error, "已禁用" }
/// ```
#[component]
pub fn Badge(
    /// 徽章变体（默认 Neutral）
    #[props(default)]
    variant: BadgeVariant,
    /// 额外 CSS 类名
    #[props(default)]
    class: String,
    /// 徽章内容
    children: Element,
) -> Element {
    let variant_class = match variant {
        BadgeVariant::Neutral => "badge-neutral",
        BadgeVariant::Success => "badge-success",
        BadgeVariant::Warning => "badge-warning",
        BadgeVariant::Error => "badge-error",
        BadgeVariant::Info => "badge-info",
    };

    let full_class = format!("badge {variant_class} {}", class.trim());

    rsx! {
        span { class: "{full_class}",
            {children}
        }
    }
}
