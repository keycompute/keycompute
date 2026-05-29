use dioxus::prelude::*;

/// 数据表格容器组件
///
/// 提供 `.table-container > table.table` 的标准结构，
/// 通过 `children` 传入 `thead` 和 `tbody` 内容。
///
/// # 示例
/// ```rust,ignore
/// Table {
///     thead {
///         tr {
///             TableHead { "名称" }
///             TableHead { "状态" }
///         }
///     }
///     tbody {
///         tr {
///             TableCell { "Key-001" }
///             TableCell { Badge { variant: BadgeVariant::Success, "活跃" } }
///         }
///     }
/// }
/// ```
#[component]
pub fn Table(
    /// 额外 CSS 类名（作用于 table 元素）
    #[props(default)]
    class: String,
    /// 无数据时的提示文字（默认"暂无数据"）
    #[props(default = "暂无数据".to_string())]
    empty_text: String,
    /// 是否显示空状态（传 true 时渲染 empty_text，忽略 children）
    #[props(default = false)]
    empty: bool,
    /// 列数（空状态时 colspan 用）
    #[props(default = 1_u32)]
    col_count: u32,
    /// 表格内容（thead + tbody）
    children: Element,
) -> Element {
    let table_class = format!("table {}", class.trim());

    rsx! {
        div { class: "table-container",
            table { class: "{table_class}",
                if empty {
                    tbody {
                        tr {
                            td {
                                colspan: "{col_count}",
                                class: "table-empty",
                                div { class: "table-empty-content",
                                    span { class: "table-empty-mark", "-" }
                                    span { class: "table-empty-text", "{empty_text}" }
                                }
                            }
                        }
                    }
                } else {
                    {children}
                }
            }
        }
    }
}

/// 表头单元格（th）
#[component]
pub fn TableHead(
    /// 额外 CSS 类名
    #[props(default)]
    class: String,
    children: Element,
) -> Element {
    rsx! {
        th { class: "{class}",
            {children}
        }
    }
}

/// 数据单元格（td）
#[component]
pub fn TableCell(
    /// 额外 CSS 类名
    #[props(default)]
    class: String,
    children: Element,
) -> Element {
    rsx! {
        td { class: "{class}",
            {children}
        }
    }
}

/// 分页控件
#[component]
pub fn Pagination(
    /// 当前页（1 起）
    current: u32,
    /// 总页数
    total_pages: u32,
    /// 页面变更回调
    #[props(default)]
    on_page_change: EventHandler<u32>,
) -> Element {
    if total_pages <= 1 {
        return rsx! {};
    }

    rsx! {
        div { class: "pagination",
            button {
                class: "btn btn-ghost btn-sm",
                disabled: current <= 1,
                onclick: move |_| {
                    if current > 1 {
                        on_page_change.call(current - 1);
                    }
                },
                "‹ 上一页"
            }
            span { class: "pagination-info",
                "{current} / {total_pages}"
            }
            button {
                class: "btn btn-ghost btn-sm",
                disabled: current >= total_pages,
                onclick: move |_| {
                    if current < total_pages {
                        on_page_change.call(current + 1);
                    }
                },
                "下一页 ›"
            }
        }
    }
}
