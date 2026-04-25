use dioxus::prelude::*;

use super::footer::Footer;
use super::header::{Header, UserMenuAction};
use super::sidebar::{NavSection, Sidebar};

const VARIABLES_CSS: Asset = asset!("/assets/styling/variables.css");
const LAYOUT_CSS: Asset = asset!("/assets/styling/layout.css");
const COMPONENTS_CSS: Asset = asset!("/assets/styling/components.css");
const DARK_CSS: Asset = asset!("/assets/styling/dark.css");
const RESPONSIVE_CSS: Asset = asset!("/assets/styling/responsive.css");

/// 全局 UI 状态，通过 Context API 向下传递
#[derive(Clone, Copy)]
pub struct UiState {
    /// 侧边栏是否折叠
    pub sidebar_collapsed: Signal<bool>,
    /// 移动端侧边栏是否打开
    pub sidebar_mobile_open: Signal<bool>,
    /// 主题：light / dark / system
    pub theme: Signal<String>,
    /// 语言：zh / en
    pub lang: Signal<String>,
}

/// 应用外壳组件，包含侧边栏 + 顶部栏 + 内容区 + 页脚
///
/// # Props
/// - `nav_sections`：侧边栏导航分组列表
/// - `page_title`：当前页面标题（显示在顶部栏）
/// - `user_name`：当前登录用户名（显示头像首字母）
/// - `children`：主内容区内容
/// - `on_user_menu`：用户下拉菜单操作回调
#[component]
pub fn AppShell(
    #[props(default)] nav_sections: Vec<NavSection>,
    #[props(default)] page_title: String,
    #[props(default)] user_name: String,
    #[props(default)] current_path: String,
    #[props(default)] toggle_sidebar_title: String,
    #[props(default)] open_menu_title: String,
    #[props(default)] switch_to_light_theme_title: String,
    #[props(default)] switch_to_dark_theme_title: String,
    #[props(default)] switch_to_zh_title: String,
    #[props(default)] switch_to_en_title: String,
    #[props(default)] profile_label: String,
    #[props(default)] account_settings_label: String,
    #[props(default)] logout_label: String,
    #[props(default)] expand_sidebar_title: String,
    #[props(default)] collapse_sidebar_title: String,
    #[props(default)] expand_label: String,
    #[props(default)] collapse_label: String,
    #[props(default)] on_user_menu: EventHandler<UserMenuAction>,
    children: Element,
) -> Element {
    let sidebar_collapsed = use_signal(|| false);
    let mut sidebar_mobile_open = use_signal(|| false);

    let theme = use_signal(|| {
        #[cfg(target_arch = "wasm32")]
        {
            read_local_storage("theme").unwrap_or_else(|| "dark".to_string())
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            "dark".to_string()
        }
    });

    let fallback_lang = use_signal(|| {
        #[cfg(target_arch = "wasm32")]
        {
            read_local_storage("lang").unwrap_or_else(|| "zh".to_string())
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            "zh".to_string()
        }
    });
    let lang = try_use_context::<Signal<String>>().unwrap_or(fallback_lang);

    use_context_provider(|| UiState {
        sidebar_collapsed,
        sidebar_mobile_open,
        theme,
        lang,
    });

    // 每次 theme 变化时将主题写入 html[data-theme]
    use_effect(move || {
        let val = theme();
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(window) = web_sys::window() {
                if let Some(doc) = window.document() {
                    if let Some(html) = doc.document_element() {
                        let _ = html.set_attribute("data-theme", &val);
                    }
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = val;
    });

    let collapsed = sidebar_collapsed();
    let mobile_open = sidebar_mobile_open();

    let main_class = if collapsed {
        "main-area sidebar-collapsed"
    } else {
        "main-area"
    };

    let overlay_class = if mobile_open {
        "sidebar-overlay visible"
    } else {
        "sidebar-overlay"
    };

    rsx! {
        document::Link { rel: "stylesheet", href: VARIABLES_CSS }
        document::Link { rel: "stylesheet", href: LAYOUT_CSS }
        document::Link { rel: "stylesheet", href: COMPONENTS_CSS }
        document::Link { rel: "stylesheet", href: DARK_CSS }
        document::Link { rel: "stylesheet", href: RESPONSIVE_CSS }

        div { class: "app-shell",
            div {
                class: "{overlay_class}",
                onclick: move |_| {
                    *sidebar_mobile_open.write() = false;
                },
            }

            Sidebar {
                sections: nav_sections.clone(),
                collapsed: sidebar_collapsed,
                mobile_open: sidebar_mobile_open,
                current_path: current_path.clone(),
                expand_sidebar_title: expand_sidebar_title.clone(),
                collapse_sidebar_title: collapse_sidebar_title.clone(),
                expand_label: expand_label.clone(),
                collapse_label: collapse_label.clone(),
            }

            div { class: "{main_class}",
                Header {
                    page_title: page_title.clone(),
                    user_name: user_name.clone(),
                    sidebar_collapsed,
                    sidebar_mobile_open,
                    theme,
                    lang,
                    toggle_sidebar_title: toggle_sidebar_title.clone(),
                    open_menu_title: open_menu_title.clone(),
                    switch_to_light_theme_title: switch_to_light_theme_title.clone(),
                    switch_to_dark_theme_title: switch_to_dark_theme_title.clone(),
                    switch_to_zh_title: switch_to_zh_title.clone(),
                    switch_to_en_title: switch_to_en_title.clone(),
                    profile_label: profile_label.clone(),
                    account_settings_label: account_settings_label.clone(),
                    logout_label: logout_label.clone(),
                    on_user_menu,
                }

                main { class: "content-area",
                    div { class: "content-inner",
                        {children}
                    }
                }

                Footer {}
            }
        }
    }
}

// ── 工具函数 ──────────────────────────────────────────────
#[cfg(target_arch = "wasm32")]
fn read_local_storage(key: &str) -> Option<String> {
    web_sys::window()?
        .local_storage()
        .ok()??
        .get_item(key)
        .ok()?
}
