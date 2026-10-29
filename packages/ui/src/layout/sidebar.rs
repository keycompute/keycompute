use dioxus::prelude::*;
use std::collections::BTreeSet;

use crate::icons::{
    IconActivity, IconBarChart, IconBuilding, IconChevronLeft, IconChevronRight, IconClipboardList,
    IconGauge, IconHome, IconKey, IconReceipt, IconServer, IconSettings, IconShare, IconTag,
    IconUser, IconUsers, IconWallet, IconX,
};

/// 单条导航项
#[derive(Clone, PartialEq)]
pub struct NavItem {
    /// 显示标签
    pub label: String,
    /// 路由路径（用于跳转和高亮判断）
    pub path: String,
    /// 图标名称，对应 IconXxx 组件
    pub icon: NavIcon,
}

impl NavItem {
    pub fn new(label: impl Into<String>, path: impl Into<String>, icon: NavIcon) -> Self {
        Self {
            label: label.into(),
            path: path.into(),
            icon,
        }
    }
}

/// 导航分组
#[derive(Clone, PartialEq)]
pub struct NavSection {
    /// Stable identifier used for interaction state; must not be localized.
    pub id: String,
    /// 分组标题（显示在侧边栏）
    pub title: Option<String>,
    pub items: Vec<NavItem>,
}

/// 图标枚举，对应 icons.rs 中的组件
#[derive(Clone, PartialEq)]
pub enum NavIcon {
    Home,
    Key,
    Wallet,
    Users,
    Settings,
    BarChart,
    Receipt,
    Share,
    User,
    Server,
    Tag,
    Building,
    Activity,
    Gauge,
    ClipboardList,
}

fn section_is_collapsed(section: &NavSection, collapsed: &BTreeSet<String>) -> bool {
    section.title.is_some() && collapsed.contains(&section.id)
}

fn section_items_visible(
    section: &NavSection,
    collapsed: &BTreeSet<String>,
    sidebar_collapsed: bool,
) -> bool {
    sidebar_collapsed || !section_is_collapsed(section, collapsed)
}

/// 侧边导航栏
///
/// # Props
/// - `sections`：导航分组列表
/// - `collapsed`：是否折叠状态（Signal）
/// - `mobile_open`：移动端是否打开（Signal）
/// - `current_path`：当前活跃路径
/// - `site_logo_src`：站点 Logo 地址，为空时回退为站点名称首字母
#[component]
pub fn Sidebar(
    #[props(default)] sections: Vec<NavSection>,
    collapsed: Signal<bool>,
    mut mobile_open: Signal<bool>,
    #[props(default)] current_path: String,
    #[props(default)] expand_sidebar_title: String,
    #[props(default)] collapse_sidebar_title: String,
    #[props(default)] expand_label: String,
    #[props(default)] collapse_label: String,
    #[props(default)] close_menu_title: String,
    #[props(default = "Main navigation".to_string())] navigation_label: String,
    #[props(default = "AI platform".to_string())] product_label: String,
    #[props(default = "KeyCompute".to_string())] site_name: String,
    #[props(default)] site_logo_src: String,
) -> Element {
    let is_collapsed = collapsed();
    let is_mobile_open = mobile_open();
    let site_initial = site_name
        .chars()
        .find(|character| !character.is_whitespace())
        .unwrap_or('K')
        .to_uppercase()
        .collect::<String>();

    let sidebar_class = {
        let mut cls = "sidebar".to_string();
        if is_collapsed {
            cls.push_str(" collapsed");
        }
        if is_mobile_open {
            cls.push_str(" mobile-open");
        }
        cls
    };
    // Several routes intentionally share a path prefix (for example
    // `/tenant/finance` and `/tenant/finance/controls`). Pick the most
    // specific matching item once for the whole navigation so only one entry
    // can be active at a time while parent-only console routes still work.
    let active_path = active_nav_path(&sections, &current_path).map(str::to_owned);
    let initially_collapsed = sections
        .iter()
        .filter_map(|section| {
            section.title.as_ref()?;
            (!section
                .items
                .iter()
                .any(|item| active_path.as_deref() == Some(item.path.as_str())))
            .then(|| section.id.clone())
        })
        .collect::<BTreeSet<_>>();
    let mut collapsed_sections = use_signal(move || initially_collapsed);
    let active_section = sections.iter().find_map(|section| {
        section.title.as_ref()?;
        section
            .items
            .iter()
            .any(|item| active_path.as_deref() == Some(item.path.as_str()))
            .then(|| section.id.clone())
    });
    use_effect(use_reactive!(|active_section| {
        if let Some(title) = active_section.as_ref()
            && collapsed_sections.peek().contains(title)
        {
            collapsed_sections.write().remove(title);
        }
    }));

    // 折叠/展开图标
    let toggle_icon = if is_collapsed {
        rsx! { IconChevronRight { size: 16 } }
    } else {
        rsx! { IconChevronLeft { size: 16 } }
    };

    rsx! {
        nav { id: "app-sidebar", class: "{sidebar_class}", aria_label: "{navigation_label}",
            // Logo 区域
            div { class: "sidebar-logo",
                if site_logo_src.trim().is_empty() {
                    div { class: "sidebar-logo-icon sidebar-logo-fallback", "{site_initial}" }
                } else {
                    img {
                        class: "sidebar-logo-icon",
                        src: "{site_logo_src}",
                        alt: "{site_name}",
                    }
                }
                div { class: "sidebar-logo-copy",
                    span { class: "sidebar-logo-text", "{site_name}" }
                    span { class: "sidebar-logo-kicker", "{product_label}" }
                }
                button {
                    class: "sidebar-mobile-close",
                    r#type: "button",
                    title: "{close_menu_title}",
                    aria_label: "{close_menu_title}",
                    onclick: move |_| mobile_open.set(false),
                    IconX { size: 20 }
                }
            }

            // 导航分组
            div { class: "sidebar-nav",
                for section in sections.iter() {
                    div { class: "sidebar-section",
                        if let Some(title) = &section.title {
                            {
                                let section_id = section.id.clone();
                                let is_open = !section_is_collapsed(section, &collapsed_sections());
                                rsx! {
                                    button {
                                        class: "sidebar-section-title sidebar-section-toggle",
                                        r#type: "button",
                                        aria_expanded: is_open,
                                        onclick: move |_| {
                                            let mut state = collapsed_sections.write();
                                            if !state.remove(&section_id) {
                                                state.insert(section_id.clone());
                                            }
                                        },
                                        span { "{title}" }
                                        span { class: "sidebar-section-indicator", aria_hidden: "true",
                                            if is_open { "−" } else { "+" }
                                        }
                                    }
                                }
                            }
                        }
                        // Compact mode hides the section toggles, so every item
                        // must remain reachable by its icon. Per-section
                        // disclosure only applies while the full sidebar is
                        // visible.
                        if section_items_visible(section, &collapsed_sections(), is_collapsed) {
                            for item in section.items.iter() {
                                SidebarNavItem {
                                    item: item.clone(),
                                    mobile_open,
                                    active: active_path.as_deref() == Some(item.path.as_str()),
                                }
                            }
                        }
                    }
                }
            }

            // 底部折叠按钮
            div { class: "sidebar-footer",
                button {
                    class: "sidebar-item",
                    title: if is_collapsed { expand_sidebar_title.clone() } else { collapse_sidebar_title.clone() },
                    onclick: move |_| {
                        let cur = collapsed();
                        *collapsed.write() = !cur;
                    },
                    span { class: "sidebar-item-icon", {toggle_icon} }
                    span { class: "sidebar-item-label",
                        if is_collapsed { {expand_label.clone()} } else { {collapse_label.clone()} }
                    }
                }
            }
        }
    }
}

/// 单条导航项组件（内部组件）
#[component]
fn SidebarNavItem(item: NavItem, mut mobile_open: Signal<bool>, active: bool) -> Element {
    let item_class = if active {
        "sidebar-item active"
    } else {
        "sidebar-item"
    };

    let icon_el = match item.icon {
        NavIcon::Home => rsx! { IconHome { size: 20 } },
        NavIcon::Key => rsx! { IconKey { size: 20 } },
        NavIcon::Wallet => rsx! { IconWallet { size: 20 } },
        NavIcon::Users => rsx! { IconUsers { size: 20 } },
        NavIcon::Settings => rsx! { IconSettings { size: 20 } },
        NavIcon::BarChart => rsx! { IconBarChart { size: 20 } },
        NavIcon::Receipt => rsx! { IconReceipt { size: 20 } },
        NavIcon::Share => rsx! { IconShare { size: 20 } },
        NavIcon::User => rsx! { IconUser { size: 20 } },
        NavIcon::Server => rsx! { IconServer { size: 20 } },
        NavIcon::Tag => rsx! { IconTag { size: 20 } },
        NavIcon::Building => rsx! { IconBuilding { size: 20 } },
        NavIcon::Activity => rsx! { IconActivity { size: 20 } },
        NavIcon::Gauge => rsx! { IconGauge { size: 20 } },
        NavIcon::ClipboardList => rsx! { IconClipboardList { size: 20 } },
    };

    let title_attr = item.label.clone();
    let label = item.label.clone();
    let path = item.path.clone();
    let nav = use_navigator();

    rsx! {
        button {
            class: "{item_class}",
            title: "{title_attr}",
            onclick: move |_| {
                mobile_open.set(false);
                nav.push(path.as_str());
            },
            span { class: "sidebar-item-icon", {icon_el} }
            span { class: "sidebar-item-label",
                "{label}"
            }
        }
    }
}

fn active_nav_path<'a>(sections: &'a [NavSection], current_path: &str) -> Option<&'a str> {
    sections
        .iter()
        .flat_map(|section| section.items.iter())
        .filter(|item| nav_item_is_active(current_path, &item.path))
        .max_by_key(|item| item.path.len())
        .map(|item| item.path.as_str())
}

fn nav_item_is_active(current_path: &str, item_path: &str) -> bool {
    // Navigation entries represent a page, while the current route may also
    // contain a query string (for example, a selected tab). Compare only the
    // path so query-driven sub-state does not make the parent entry appear
    // inactive.
    let current_path = current_path.split('?').next().unwrap_or(current_path);
    let item_path = item_path.split('?').next().unwrap_or(item_path);

    current_path == item_path
        || (item_path != "/"
            && current_path
                .strip_prefix(item_path)
                .is_some_and(|suffix| suffix.starts_with('/')))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        NavIcon, NavItem, NavSection, active_nav_path, nav_item_is_active, section_is_collapsed,
        section_items_visible,
    };

    #[test]
    fn nested_console_routes_keep_their_parent_navigation_active() {
        assert!(nav_item_is_active(
            "/admin/monitoring/diagnostics",
            "/admin/monitoring"
        ));
        assert!(nav_item_is_active("/admin/monitoring", "/admin/monitoring"));
        assert!(!nav_item_is_active(
            "/admin/monitoring-archive",
            "/admin/monitoring"
        ));
        assert!(!nav_item_is_active("/dashboard", "/"));
        assert!(nav_item_is_active(
            "/node/earnings?tab=withdrawals",
            "/node/earnings?"
        ));
        assert!(nav_item_is_active(
            "/platform/operations?tab=capacity",
            "/platform/operations?"
        ));
    }

    #[test]
    fn the_longest_matching_route_is_the_only_active_item() {
        let sections = vec![NavSection {
            id: "test".to_string(),
            title: None,
            items: vec![
                NavItem::new("Finance", "/tenant/finance", NavIcon::Wallet),
                NavItem::new(
                    "Financial controls",
                    "/tenant/finance/controls",
                    NavIcon::Wallet,
                ),
            ],
        }];

        assert_eq!(
            active_nav_path(&sections, "/tenant/finance/controls"),
            Some("/tenant/finance/controls")
        );
        assert_eq!(
            active_nav_path(&sections, "/tenant/finance"),
            Some("/tenant/finance")
        );
    }

    #[test]
    fn translated_section_titles_keep_the_same_collapse_state() {
        let mut section = NavSection {
            id: "billing".to_string(),
            title: Some("Billing".to_string()),
            items: vec![],
        };
        let collapsed = BTreeSet::from(["billing".to_string()]);

        assert!(section_is_collapsed(&section, &collapsed));
        section.title = Some("账单".to_string());
        assert!(section_is_collapsed(&section, &collapsed));
        assert!(section_items_visible(&section, &collapsed, true));
        assert!(!section_items_visible(&section, &collapsed, false));
    }
}
