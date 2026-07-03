use dioxus::prelude::*;

use crate::i18n::{I18n, Lang};
use ui::layout::app_shell::UiState;

/// 获取国际化实例
/// 优先级：UiState > Signal<String>(App 上下文) > 默认中文
pub fn use_i18n() -> I18n {
    // 1) 优先从 UiState 获取（在 AppLayout/AppShell 内部）
    if let Some(ui_state) = try_use_context::<UiState>() {
        let lang_str = (ui_state.lang)();
        return I18n::new(match lang_str.as_str() {
            "en" => Lang::En,
            _ => Lang::Zh,
        });
    }

    // 2) 回退到 App() 根组件提供的 Signal<String>（首页登录/注册弹窗等场景）
    if let Some(lang_signal) = try_use_context::<Signal<String>>() {
        let lang_str = lang_signal();
        return I18n::new(match lang_str.as_str() {
            "en" => Lang::En,
            _ => Lang::Zh,
        });
    }

    // 3) 终极回退
    I18n::new(Lang::Zh)
}
