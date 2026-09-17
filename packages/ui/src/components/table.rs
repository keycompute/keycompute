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
    let container_class = if empty {
        "table-container table-container-empty"
    } else {
        "table-container"
    };

    rsx! {
        div { class: "{container_class}", tabindex: "0",
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
    /// 总条数（用于统一的分页摘要）
    #[props(default = 0_u64)]
    total: u64,
    /// 当前每页条数
    #[props(default = 20_u32)]
    page_size: u32,
    /// 额外 CSS 类名（作用于分页导航容器）
    #[props(default)]
    class: String,
    /// 分页导航的无障碍标签
    #[props(default)]
    aria_label: String,
    /// 是否暂时禁用所有分页控件（例如请求正在进行时）
    #[props(default = false)]
    disabled: bool,
    /// 可选的每页条数；为空时使用 10/20/50/100
    #[props(default)]
    page_size_options: Vec<u32>,
    /// 页面变更回调
    #[props(default)]
    on_page_change: EventHandler<u32>,
    /// 每页条数变更回调
    #[props(default)]
    on_page_size_change: EventHandler<u32>,
    /// 上一页按钮文案；共享库默认使用语言无关的箭头，业务层应传入本地化文案
    #[props(default = "‹".to_string())]
    previous_label: String,
    /// 下一页按钮文案
    #[props(default = "›".to_string())]
    next_label: String,
    /// 分页摘要；为空时回退到 `current / total_pages`
    #[props(default)]
    summary: String,
    /// 每页选择器文案
    #[props(default = "Per page".to_string())]
    page_size_label: String,
    /// 每页选择器的单位文案
    #[props(default = "items".to_string())]
    page_size_suffix: String,
) -> Element {
    // Keep an empty first page uncluttered, but retain the recovery control
    // when the parent is still requesting a page that no longer exists.
    if !should_render_pagination(total, current) {
        return rsx! {};
    }

    let requested_current = current;
    let (total_pages, current) = normalized_page(current, total_pages);
    let previous_page = previous_page_target(requested_current, current, total_pages);
    let next_page = next_page_target(requested_current, current, total_pages);

    let page_size = page_size.max(1);
    let options = normalized_page_size_options(page_size_options, page_size);
    // A caller-provided localized summary is based on its requested page. If
    // the response clamps that page after a deletion, avoid showing a
    // contradictory "page N of M" until the parent follows the recovery
    // action exposed by the previous button.
    let summary = if summary.is_empty() || requested_current != current {
        format!("{current} / {total_pages}")
    } else {
        summary
    };
    let aria_label = if aria_label.trim().is_empty() {
        summary.clone()
    } else {
        aria_label
    };

    let class = class.trim();
    let pagination_class = if class.is_empty() {
        "pagination pagination-footer".to_string()
    } else {
        format!("pagination pagination-footer {class}")
    };

    rsx! {
        nav {
            class: "{pagination_class}",
            aria_label: "{aria_label}",
            aria_live: "polite",
            span { class: "pagination-summary", "{summary}" }
            div { class: "pagination-actions",
                button {
                    class: "btn btn-ghost btn-sm pagination-button pagination-previous",
                    r#type: "button",
                    aria_label: "{previous_label}",
                    disabled: disabled || previous_page.is_none(),
                    onclick: move |_| {
                        if !disabled && let Some(page) = previous_page {
                            on_page_change.call(page);
                        }
                    },
                    "{previous_label}"
                }
                button {
                    class: "btn btn-ghost btn-sm pagination-button pagination-next",
                    r#type: "button",
                    aria_label: "{next_label}",
                    disabled: disabled || next_page.is_none(),
                    onclick: move |_| {
                        if !disabled && let Some(page) = next_page {
                            on_page_change.call(page);
                        }
                    },
                    "{next_label}"
                }
                label { class: "pagination-page-size",
                    span { "{page_size_label}" }
                    select {
                        aria_label: "{page_size_label}",
                        disabled,
                        value: "{page_size}",
                        onchange: move |event| {
                            if !disabled && let Ok(value) = event.value().parse::<u32>() {
                                on_page_size_change.call(value);
                            }
                        },
                        for option in options.iter() {
                            option { value: "{option}", "{option}" }
                        }
                    }
                    span { "{page_size_suffix}" }
                }
            }
        }
    }
}

/// 基于游标（而非总页数）的分页控件。
///
/// 适合 API 只返回 `next_cursor`、无法提前知道总页数的列表。调用方负责
/// 维护上一页游标历史，并通过 `has_previous` / `has_next` 告知控件可用方向。
/// 视觉结构、每页条数选择器和无障碍行为与 [`Pagination`] 保持一致。
#[component]
pub fn CursorPagination(
    /// 当前页（1 起）
    current: u32,
    /// 是否存在上一页
    has_previous: bool,
    /// 是否存在下一页
    has_next: bool,
    /// 当前每页条数
    #[props(default = 20_u32)]
    page_size: u32,
    /// 额外 CSS 类名（作用于分页导航容器）
    #[props(default)]
    class: String,
    /// 分页导航的无障碍标签
    #[props(default)]
    aria_label: String,
    /// 是否暂时禁用所有分页控件（例如请求正在进行时）
    #[props(default = false)]
    disabled: bool,
    /// 可选的每页条数；为空时使用 10/20/50/100
    #[props(default)]
    page_size_options: Vec<u32>,
    /// 上一页回调
    #[props(default)]
    on_previous: EventHandler<()>,
    /// 下一页回调
    #[props(default)]
    on_next: EventHandler<()>,
    /// 每页条数变更回调
    #[props(default)]
    on_page_size_change: EventHandler<u32>,
    /// 上一页按钮文案；业务层应传入本地化文案
    #[props(default = "‹".to_string())]
    previous_label: String,
    /// 下一页按钮文案
    #[props(default = "›".to_string())]
    next_label: String,
    /// 分页摘要；为空时仅显示当前页码，业务层应传入本地化文案
    #[props(default)]
    summary: String,
    /// 每页选择器文案
    #[props(default = "Per page".to_string())]
    page_size_label: String,
    /// 每页选择器的单位文案
    #[props(default = "items".to_string())]
    page_size_suffix: String,
) -> Element {
    let current = current.max(1);
    let page_size = page_size.max(1);
    let options = normalized_page_size_options(page_size_options, page_size);
    let summary = if summary.is_empty() {
        current.to_string()
    } else {
        summary
    };
    let aria_label = if aria_label.trim().is_empty() {
        summary.clone()
    } else {
        aria_label
    };
    let class = class.trim();
    let pagination_class = if class.is_empty() {
        "pagination pagination-footer".to_string()
    } else {
        format!("pagination pagination-footer {class}")
    };

    rsx! {
        nav {
            class: "{pagination_class}",
            aria_label: "{aria_label}",
            aria_live: "polite",
            span { class: "pagination-summary", "{summary}" }
            div { class: "pagination-actions",
                button {
                    class: "btn btn-ghost btn-sm pagination-button pagination-previous",
                    r#type: "button",
                    aria_label: "{previous_label}",
                    disabled: disabled || !has_previous,
                    onclick: move |_| {
                        if !disabled && has_previous {
                            on_previous.call(());
                        }
                    },
                    "{previous_label}"
                }
                button {
                    class: "btn btn-ghost btn-sm pagination-button pagination-next",
                    r#type: "button",
                    aria_label: "{next_label}",
                    disabled: disabled || !has_next,
                    onclick: move |_| {
                        if !disabled && has_next {
                            on_next.call(());
                        }
                    },
                    "{next_label}"
                }
                label { class: "pagination-page-size",
                    span { "{page_size_label}" }
                    select {
                        aria_label: "{page_size_label}",
                        disabled,
                        value: "{page_size}",
                        onchange: move |event| {
                            if !disabled && let Ok(value) = event.value().parse::<u32>() {
                                on_page_size_change.call(value);
                            }
                        },
                        for option in options.iter() {
                            option { value: "{option}", "{option}" }
                        }
                    }
                    span { "{page_size_suffix}" }
                }
            }
        }
    }
}

fn normalized_page(current: u32, total_pages: u32) -> (u32, u32) {
    let total_pages = total_pages.max(1);
    let current = current.clamp(1, total_pages);
    (total_pages, current)
}

fn normalized_page_size_options(mut options: Vec<u32>, page_size: u32) -> Vec<u32> {
    if options.is_empty() {
        options = vec![10, 20, 50, 100];
    }

    options.retain(|option| *option > 0);
    let page_size = page_size.max(1);
    if !options.contains(&page_size) {
        options.push(page_size);
    }
    options.sort_unstable();
    options.dedup();
    options
}

fn should_render_pagination(total: u64, current: u32) -> bool {
    total > 0 || current > 1
}

fn previous_page_target(requested_current: u32, current: u32, total_pages: u32) -> Option<u32> {
    if requested_current <= 1 {
        None
    } else if requested_current > total_pages {
        // The response has fewer pages than the parent requested. Navigate
        // directly to the displayed/clamped page so the parent can recover.
        Some(current)
    } else {
        Some(current - 1)
    }
}

fn next_page_target(requested_current: u32, current: u32, total_pages: u32) -> Option<u32> {
    (requested_current.max(1) < total_pages).then_some(current + 1)
}

#[cfg(test)]
mod tests {
    use super::{
        next_page_target, normalized_page, normalized_page_size_options, previous_page_target,
        should_render_pagination,
    };

    #[test]
    fn out_of_range_pages_are_clamped_to_an_accessible_page() {
        assert_eq!(normalized_page(2, 1), (1, 1));
        assert_eq!(normalized_page(99, 3), (3, 3));
        assert_eq!(normalized_page(0, 0), (1, 1));
    }

    #[test]
    fn out_of_range_previous_navigation_recovers_the_parent_page() {
        assert_eq!(previous_page_target(2, 1, 1), Some(1));
        assert_eq!(previous_page_target(99, 3, 3), Some(3));
        assert_eq!(previous_page_target(1, 1, 3), None);
        assert_eq!(next_page_target(1, 1, 3), Some(2));
        assert_eq!(next_page_target(3, 3, 3), None);
    }

    #[test]
    fn empty_first_page_hides_controls_but_stale_page_keeps_recovery() {
        assert!(!should_render_pagination(0, 1));
        assert!(should_render_pagination(0, 2));
        assert!(should_render_pagination(1, 1));
    }

    #[test]
    fn page_size_options_are_positive_sorted_unique_and_keep_the_current_size() {
        assert_eq!(
            normalized_page_size_options(vec![0, 50, 20, 20], 30),
            vec![20, 30, 50]
        );
        assert_eq!(
            normalized_page_size_options(Vec::new(), 0),
            vec![1, 10, 20, 50, 100]
        );
    }

    #[test]
    fn shared_pagination_has_no_hard_coded_language() {
        let source = include_str!("table.rs");
        let component_source = source.split("#[cfg(test)]").next().unwrap_or(source);
        let previous: String = ['上', '一', '页'].into_iter().collect();
        let next: String = ['下', '一', '页'].into_iter().collect();
        assert!(!component_source.contains(&format!("‹ {previous}")));
        assert!(!component_source.contains(&format!("{next} ›")));
    }
}
