use dioxus::prelude::*;

use crate::icons::{
    IconChevronDown, IconGlobe, IconLogOut, IconMenu, IconMoon, IconSettings, IconSun, IconUser,
};

/// 用户下拉菜单项回调
#[derive(Clone, Copy, PartialEq)]
pub enum UserMenuAction {
    /// 点击个人资料
    Profile,
    /// 点击设置
    Settings,
    /// 点击退出登录
    Logout,
}

/// 顶部栏组件
///
/// # Props
/// - `page_title`：当前页面标题
/// - `user_name`：当前用户名（头像首字母）
/// - `sidebar_collapsed`：侧边栏折叠状态（Signal，点击汉堡菜单时切换）
/// - `sidebar_mobile_open`：移动端侧边栏开关（Signal）
/// - `theme`：当前主题（Signal<String>），值为 "light" / "dark" / "system"
/// - `lang`：当前语言（Signal<String>），值为 "zh" / "en"
/// - `on_user_menu`：用户下拉菜单项点击回调
#[component]
pub fn Header(
    #[props(default)] page_title: String,
    #[props(default)] user_name: String,
    sidebar_collapsed: Signal<bool>,
    sidebar_mobile_open: Signal<bool>,
    theme: Signal<String>,
    lang: Signal<String>,
    #[props(default)] toggle_sidebar_title: String,
    #[props(default)] open_menu_title: String,
    #[props(default)] switch_to_light_theme_title: String,
    #[props(default)] switch_to_dark_theme_title: String,
    #[props(default)] switch_to_zh_title: String,
    #[props(default)] switch_to_en_title: String,
    #[props(default)] profile_label: String,
    #[props(default)] account_settings_label: String,
    #[props(default)] logout_label: String,
    #[props(default)] on_user_menu: EventHandler<UserMenuAction>,
) -> Element {
    // 头像首字母
    let avatar_char = user_name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "U".to_string());

    // 主题图标：light 显示月亮（切换到 dark），dark 显示太阳（切换到 light）
    let is_dark = theme() == "dark";
    let theme_title = if is_dark {
        switch_to_light_theme_title
    } else {
        switch_to_dark_theme_title
    };

    let lang_val = lang();
    let lang_label = if lang_val == "zh" { "EN" } else { "中" };
    let lang_title = if lang_val == "zh" {
        switch_to_en_title
    } else {
        switch_to_zh_title
    };

    let title = page_title.clone();

    // 下拉菜单展开状态
    let mut dropdown_open = use_signal(|| false);

    rsx! {
        header { class: "header",
            // 左侧
            div { class: "header-left",
                // PC 端折叠/展开按钮
                button {
                    class: "header-toggle-btn hide-mobile",
                    title: "{toggle_sidebar_title}",
                    onclick: move |_| {
                        let cur = sidebar_collapsed();
                        *sidebar_collapsed.write() = !cur;
                    },
                    IconMenu { size: 20 }
                }
                // 移动端汉堡菜单
                button {
                    class: "header-toggle-btn hide-desktop hide-tablet",
                    title: "{open_menu_title}",
                    onclick: move |_| {
                        let cur = sidebar_mobile_open();
                        *sidebar_mobile_open.write() = !cur;
                    },
                    IconMenu { size: 20 }
                }

                // 页面标题
                if !title.is_empty() {
                    h1 { class: "header-page-title", "{title}" }
                }
            }

            // 右侧工具栏
            div { class: "header-right",
                // 主题切换
                button {
                    class: "header-icon-btn",
                    title: "{theme_title}",
                    onclick: move |_| {
                        let cur = theme();
                        let next = if cur == "dark" { "light" } else { "dark" };
                        *theme.write() = next.to_string();
                        // 持久化到 localStorage
                        #[cfg(target_arch = "wasm32")]
                        {
                            let _ = write_local_storage("theme", next);
                        }
                    },
                    if is_dark {
                        IconSun { size: 18 }
                    } else {
                        IconMoon { size: 18 }
                    }
                }

                // 语言切换
                button {
                    class: "header-icon-btn header-lang-btn",
                    title: "{lang_title}",
                    onclick: move |_| {
                        let cur = lang();
                        let next = if cur == "zh" { "en" } else { "zh" };
                        *lang.write() = next.to_string();
                        #[cfg(target_arch = "wasm32")]
                        {
                            let _ = write_local_storage("lang", next);
                        }
                    },
                    IconGlobe { size: 18 }
                    span { "{lang_label}" }
                }

                // 通知功能待实现，暂隐藏铃铛按钮
                // button {
                //     class: "header-icon-btn",
                //     title: "通知",
                //     IconBell { size: 18 }
                // }

                // 用户头像
                div { class: "header-avatar",
                    title: "{user_name}",
                    "{avatar_char}"
                }

                // 用户名 + 下拉箭头（桌面端）- 带下拉菜单
                div {
                    class: "header-user-dropdown",
                    style: "position: relative;",
                    button {
                        class: "header-icon-btn hide-mobile",
                        style: "gap: 4px; width: auto; padding: 0 8px;",
                        onclick: move |_| {
                            let cur = dropdown_open();
                            *dropdown_open.write() = !cur;
                        },
                        span {
                            style: "font-size: 13px; font-weight: 500; color: var(--text-primary); max-width: 120px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                            "{user_name}"
                        }
                        IconChevronDown {
                            size: 16,
                        }
                    }

                    // 下拉菜单
                    if dropdown_open() {
                        div {
                            class: "dropdown-menu",
                            style: "position: absolute; top: 100%; right: 0; margin-top: 4px; min-width: 160px; background: var(--bg-card, white); border: 1px solid var(--border-color, #e2e8f0); border-radius: 8px; box-shadow: 0 4px 12px rgba(0,0,0,0.15); z-index: 1000; overflow: hidden;",

                            // 个人资料
                            button {
                                class: "dropdown-item",
                                style: "display: flex; align-items: center; gap: 10px; width: 100%; padding: 10px 14px; border: none; background: none; cursor: pointer; font-size: 14px; color: var(--text-primary); text-align: left; transition: background 0.15s;",
                                onmouseenter: move |e| { let _ = e; },
                                onmouseleave: move |e| { let _ = e; },
                                onclick: move |_| {
                                    *dropdown_open.write() = false;
                                    on_user_menu.call(UserMenuAction::Profile);
                                },
                                IconUser { size: 16 }
                                span { "{profile_label}" }
                            }

                            // 设置
                            button {
                                class: "dropdown-item",
                                style: "display: flex; align-items: center; gap: 10px; width: 100%; padding: 10px 14px; border: none; background: none; cursor: pointer; font-size: 14px; color: var(--text-primary); text-align: left; transition: background 0.15s;",
                                onmouseenter: move |e| { let _ = e; },
                                onmouseleave: move |e| { let _ = e; },
                                onclick: move |_| {
                                    *dropdown_open.write() = false;
                                    on_user_menu.call(UserMenuAction::Settings);
                                },
                                IconSettings { size: 16 }
                                span { "{account_settings_label}" }
                            }

                            // 分隔线
                            div {
                                style: "height: 1px; background: var(--border-color, #e2e8f0); margin: 4px 0;"
                            }

                            // 退出登录
                            button {
                                class: "dropdown-item",
                                style: "display: flex; align-items: center; gap: 10px; width: 100%; padding: 10px 14px; border: none; background: none; cursor: pointer; font-size: 14px; color: var(--danger, #dc2626); text-align: left; transition: background 0.15s;",
                                onmouseenter: move |e| { let _ = e; },
                                onmouseleave: move |e| { let _ = e; },
                                onclick: move |_| {
                                    *dropdown_open.write() = false;
                                    on_user_menu.call(UserMenuAction::Logout);
                                },
                                IconLogOut { size: 16 }
                                span { "{logout_label}" }
                            }
                        }
                    }
                }

                // 点击外部关闭下拉菜单覆盖层
                if dropdown_open() {
                    div {
                        style: "position: fixed; inset: 0; z-index: 999;",
                        onclick: move |_| {
                            *dropdown_open.write() = false;
                        },
                    }
                }
            }
        }
    }
}

// ── localStorage 写入 ────────────────────────────
#[cfg(target_arch = "wasm32")]
fn write_local_storage(key: &str, value: &str) -> Option<()> {
    web_sys::window()?
        .local_storage()
        .ok()??
        .set_item(key, value)
        .ok()
}
