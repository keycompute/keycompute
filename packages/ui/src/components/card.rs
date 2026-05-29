use dioxus::prelude::*;

/// 内容卡片组件
///
/// # 插槽
/// - `header`   — 卡片标题区（含右侧操作区）
/// - `children` — 卡片主体内容（自动包裹 .card-body）
/// - `footer`   — 卡片底部区（可选）
///
/// # 示例
/// ```rust,ignore
/// Card {
///     title: "API Keys",
///     Card {
///         // 主体内容
///         "内容..."
///     }
/// }
/// ```
#[component]
pub fn Card(
    /// 卡片标题（为空时不渲染 header）
    #[props(default)]
    title: String,
    /// 标题右侧操作区（可选）
    #[props(default)]
    actions: Option<Element>,
    /// 卡片底部内容（可选）
    #[props(default)]
    footer: Option<Element>,
    /// 额外 CSS 类名（作用于 .card）
    #[props(default)]
    class: String,
    /// 主体内容
    children: Element,
) -> Element {
    let full_class = format!("card {}", class.trim());

    rsx! {
        div { class: "{full_class}",
            if !title.is_empty() {
                div { class: "card-header",
                    h3 { class: "card-title", "{title}" }
                    if let Some(act) = actions {
                        div { class: "card-actions",
                            {act}
                        }
                    }
                }
            }
            div { class: "card-body",
                {children}
            }
            if let Some(ftr) = footer {
                div { class: "card-footer",
                    {ftr}
                }
            }
        }
    }
}

/// 统计数字卡片（用于仪表盘）
#[component]
pub fn StatCard(
    /// 指标标题
    title: String,
    /// 数字值
    value: String,
    /// 描述/单位
    #[props(default)]
    description: String,
    /// 图标内容（可选）
    #[props(default)]
    icon: Option<Element>,
    /// 变化趋势（正数 "↑5.3%"，负数 "↓1.2%"）
    #[props(default)]
    trend: String,
    /// 趋势是否为正（true→绿色，false→红色）
    #[props(default = true)]
    trend_positive: bool,
) -> Element {
    let trend_class = if trend_positive {
        "stat-trend positive"
    } else {
        "stat-trend negative"
    };

    rsx! {
        div { class: "stat-card card",
            div { class: "stat-card-body card-body",
                div { class: "stat-card-header",
                    div { class: "stat-card-info",
                        p { class: "stat-label", "{title}" }
                        p { class: "stat-value", "{value}" }
                        if !description.is_empty() {
                            p { class: "stat-description", "{description}" }
                        }
                    }
                    if let Some(ic) = icon {
                        div { class: "stat-icon",
                            {ic}
                        }
                    }
                }
                if !trend.is_empty() {
                    span { class: "{trend_class}", "{trend}" }
                }
            }
        }
    }
}
