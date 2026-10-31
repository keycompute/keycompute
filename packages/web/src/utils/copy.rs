use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use crate::stores::ui_store::UiStore;

use super::copy_to_clipboard;

/// 创建复制按钮的 `onclick` handler（信号闪烁模式）。
///
/// - **成功**：`copied` 信号置 `true`，2 秒后自动复位
/// - **失败**（非 HTTPS 安全上下文）：通过 Toast 提示用户手动右键复制
///
/// # 用法
/// ```rust,ignore
/// onclick: on_copy(
///     text_to_copy.clone(),
///     i18n.t("xxx.copy_manual_hint").to_string(),
///     ui_store,
///     copied,
/// ),
/// ```
pub fn on_copy(
    text: String,
    hint: String,
    mut ui_store: UiStore,
    mut copied: Signal<bool>,
) -> EventHandler<MouseEvent> {
    EventHandler::new(move |_: MouseEvent| {
        if copy_to_clipboard(&text) {
            copied.set(true);
            let mut c = copied.clone();
            spawn(async move {
                TimeoutFuture::new(2000).await;
                c.set(false);
            });
        } else {
            ui_store.show_info(hint.as_str());
        }
    })
}

/// 创建带键控状态的复制按钮 handler。
///
/// 与 `on_copy` 不同，这个版本把当前复制内容的键和代次写入信号。延迟
/// 复位时只有信号仍对应同一个键和同一代复制操作才会清空状态，避免快速
/// 打开另一条详情或连续复制同一条详情后被旧定时器误清除。
pub fn on_copy_keyed(
    text: String,
    hint: String,
    mut ui_store: UiStore,
    mut copied_key: Signal<Option<String>>,
    mut copy_generation: Signal<u64>,
    key: String,
) -> EventHandler<MouseEvent> {
    EventHandler::new(move |_: MouseEvent| {
        if copy_to_clipboard(&text) {
            let generation = copy_generation().wrapping_add(1);
            copy_generation.set(generation);
            copied_key.set(Some(key.clone()));
            let mut state = copied_key.clone();
            let current_generation = copy_generation.clone();
            let expected = key.clone();
            spawn(async move {
                TimeoutFuture::new(2000).await;
                if copy_state_is_current(
                    state().as_deref(),
                    current_generation(),
                    expected.as_str(),
                    generation,
                ) {
                    state.set(None);
                }
            });
        } else {
            ui_store.show_info(hint.as_str());
        }
    })
}

fn copy_state_is_current(
    current_key: Option<&str>,
    current_generation: u64,
    expected_key: &str,
    expected_generation: u64,
) -> bool {
    current_key == Some(expected_key) && current_generation == expected_generation
}

#[cfg(test)]
mod tests {
    use super::copy_state_is_current;

    #[test]
    fn stale_copy_timer_cannot_clear_a_newer_same_key_copy() {
        assert!(!copy_state_is_current(Some("user-1"), 2, "user-1", 1));
        assert!(copy_state_is_current(Some("user-1"), 2, "user-1", 2));
        assert!(!copy_state_is_current(Some("user-2"), 2, "user-1", 2));
    }
}

/// 创建复制按钮的 `onclick` handler（Toast 提示模式）。
///
/// 适用于按钮本身展示复制内容的场景（如邀请链接），复制成功时通过
/// Success Toast 反馈，而非改变按钮文本。
///
/// - **成功**：通过 Success Toast 提示用户
/// - **失败**（非 HTTPS 安全上下文）：通过 Info Toast 提示手动右键复制
///
/// # 用法
/// ```rust,ignore
/// onclick: on_copy_toast(
///     text_to_copy.clone(),
///     i18n.t("common.copied"),
///     i18n.t("common.copy_manual_hint"),
///     ui_store,
/// ),
/// ```
#[allow(dead_code)]
pub fn on_copy_toast(
    text: String,
    success_msg: &'static str,
    hint: &'static str,
    mut ui_store: UiStore,
) -> EventHandler<MouseEvent> {
    EventHandler::new(move |_: MouseEvent| {
        if copy_to_clipboard(&text) {
            ui_store.show_success(success_msg);
        } else {
            ui_store.show_info(hint);
        }
    })
}
