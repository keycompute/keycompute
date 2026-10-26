use dioxus::prelude::*;

/// 对话框组件（受控模式）
///
/// # 示例
/// ```rust,ignore
/// let mut open = use_signal(|| false);
/// Modal {
///     open,
///     title: "确认删除",
///     onclose: move |_| *open.write() = false,
///     p { "确定要删除该项目吗？" }
/// }
/// ```
#[component]
pub fn Modal(
    /// 是否显示（受控）
    open: ReadSignal<bool>,
    /// 标题
    #[props(default)]
    title: String,
    /// 关闭回调（点击遮罩或关闭按钮触发）
    #[props(default)]
    onclose: EventHandler<()>,
    /// 关闭按钮的无障碍文案
    #[props(default = "Close".to_string())]
    close_label: String,
    /// 底部操作区（可选，若不传则不渲染 modal-footer）
    #[props(default)]
    footer: Option<Element>,
    /// 弹窗最大宽度（CSS 值，如 "640px"，默认 "480px"）
    #[props(default = "480px".to_string())]
    max_width: String,
    /// 主体内容
    children: Element,
) -> Element {
    let is_open = open();
    use_dialog_focus_return(is_open);
    if !is_open {
        return rsx! {};
    }

    rsx! {
        div {
            class: "modal-backdrop",
            tabindex: "-1",
            onkeydown: move |event| {
                if event.key() == Key::Escape {
                    onclose.call(());
                } else {
                    contain_modal_focus(&event);
                }
            },
            onclick: move |_| onclose.call(()),
            div {
                class: "modal",
                role: "dialog",
                aria_modal: "true",
                aria_label: "{title}",
                style: "max-width: {max_width}",
                // 阻止冒泡，避免点击内部关闭弹窗
                onclick: move |e| e.stop_propagation(),

                // 头部
                div { class: "modal-header",
                    h2 { class: "modal-title", "{title}" }
                    button {
                        class: "modal-close btn btn-ghost btn-sm",
                        r#type: "button",
                        autofocus: true,
                        aria_label: "{close_label}",
                        onclick: move |_| onclose.call(()),
                        "✕"
                    }
                }

                // 主体
                div { class: "modal-body",
                    {children}
                }

                // 底部（可选）
                if let Some(ftr) = footer {
                    div { class: "modal-footer",
                        {ftr}
                    }
                }
            }
        }
    }
}

/// 确认对话框（内置「确认」和「取消」按钮）
#[component]
pub fn ConfirmModal(
    /// 是否显示
    open: ReadSignal<bool>,
    /// 标题
    #[props(default = "确认操作".to_string())]
    title: String,
    /// 描述文字
    #[props(default)]
    message: String,
    /// 确认按钮文字
    #[props(default = "确认".to_string())]
    confirm_text: String,
    /// 取消按钮文字
    #[props(default = "取消".to_string())]
    cancel_text: String,
    /// 右上角关闭按钮的无障碍文案
    #[props(default = "Close".to_string())]
    close_label: String,
    /// 确认按钮是否为危险变体
    #[props(default = false)]
    danger: bool,
    /// 操作提交中；锁定所有关闭和重复确认入口。
    #[props(default = false)]
    busy: bool,
    /// 确认回调
    #[props(default)]
    onconfirm: EventHandler<()>,
    /// 取消/关闭回调
    #[props(default)]
    oncancel: EventHandler<()>,
) -> Element {
    let is_open = open();
    use_dialog_focus_return(is_open);
    if !is_open {
        return rsx! {};
    }

    let confirm_class = if danger {
        "btn btn-danger"
    } else {
        "btn btn-primary"
    };

    rsx! {
        div {
            class: "modal-backdrop",
            tabindex: "-1",
            onkeydown: move |event| {
                if event.key() == Key::Escape && !busy {
                    oncancel.call(());
                } else {
                    contain_modal_focus(&event);
                }
            },
            onclick: move |_| if !busy { oncancel.call(()) },
            div {
                class: "modal",
                role: "alertdialog",
                aria_modal: "true",
                aria_label: "{title}",
                onclick: move |e| e.stop_propagation(),

                div { class: "modal-header",
                    h2 { class: "modal-title", "{title}" }
                    button {
                        class: "modal-close btn btn-ghost btn-sm",
                        r#type: "button",
                        autofocus: true,
                        aria_label: "{close_label}",
                        disabled: busy,
                        onclick: move |_| if !busy { oncancel.call(()) },
                        "✕"
                    }
                }

                div { class: "modal-body",
                    if !message.is_empty() {
                        p { class: "text-secondary", "{message}" }
                    }
                }

                div { class: "modal-footer",
                    button {
                        class: "btn btn-ghost",
                        r#type: "button",
                        disabled: busy,
                        onclick: move |_| if !busy { oncancel.call(()) },
                        "{cancel_text}"
                    }
                    button {
                        class: "{confirm_class}",
                        r#type: "button",
                        disabled: busy,
                        onclick: move |_| if !busy { onconfirm.call(()) },
                        "{confirm_text}"
                    }
                }
            }
        }
    }
}

/// Keep keyboard focus inside the top-most open dialog. This is public so the
/// application root can cover legacy `.modal-overlay` dialogs while they are
/// migrated to the shared component.
pub fn contain_modal_focus(event: &KeyboardEvent) {
    if event.key() == Key::Tab && trap_modal_focus(event.modifiers().shift()) {
        event.prevent_default();
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Default)]
struct DialogFocusState {
    was_open: bool,
    opener: Option<web_sys::HtmlElement>,
}

/// Capture the currently focused control before the dialog DOM is mounted and
/// restore it after the dialog is removed. Interior state is intentional here:
/// updating a reactive signal during render would schedule a second render and
/// can overwrite the opener after `autofocus` moves focus into the dialog.
#[cfg(target_arch = "wasm32")]
fn use_dialog_focus_return(open: bool) {
    use std::{cell::RefCell, rc::Rc};
    use wasm_bindgen::JsCast;

    let state = use_hook(|| Rc::new(RefCell::new(DialogFocusState::default())));
    let restore = {
        let mut state = state.borrow_mut();
        let restore = if open && !state.was_open {
            state.opener = web_sys::window()
                .and_then(|window| window.document())
                .and_then(|document| document.active_element())
                .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
                .filter(|element| !matches!(element.tag_name().as_str(), "BODY" | "HTML"));
            None
        } else if !open && state.was_open {
            state.opener.take()
        } else {
            None
        };
        state.was_open = open;
        restore
    };
    if let Some(element) = restore {
        defer_focus(element);
    }

    use_drop({
        let state = state.clone();
        move || {
            let opener = {
                let mut state = state.borrow_mut();
                state.was_open = false;
                state.opener.take()
            };
            if let Some(element) = opener {
                defer_focus(element);
            }
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn use_dialog_focus_return(open: bool) {
    let _ = open;
}

#[cfg(target_arch = "wasm32")]
fn defer_focus(element: web_sys::HtmlElement) {
    let timeout = gloo_timers::callback::Timeout::new(0, move || {
        if element.is_connected() {
            let _ = element.focus();
        }
    });
    timeout.forget();
}

#[cfg(target_arch = "wasm32")]
fn trap_modal_focus(backwards: bool) -> bool {
    use wasm_bindgen::JsCast;

    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return false;
    };
    let Ok(modals) = document.query_selector_all(
        ".modal-backdrop .modal, .modal-overlay .modal, .modal-backdrop [role='dialog'], .modal-overlay [role='dialog']",
    )
    else {
        return false;
    };
    let Some(modal) = modals
        .length()
        .checked_sub(1)
        .and_then(|index| modals.item(index))
        .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
    else {
        return false;
    };
    let Ok(nodes) = modal.query_selector_all(
        "a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])",
    ) else {
        return false;
    };
    let focusable = (0..nodes.length())
        .filter_map(|index| nodes.item(index))
        .filter_map(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        .collect::<Vec<_>>();
    let (Some(first), Some(last)) = (focusable.first(), focusable.last()) else {
        return false;
    };
    let Some(active) = document.active_element() else {
        let _ = first.focus();
        return true;
    };
    let active_node: &web_sys::Node = active.unchecked_ref();
    let first_node: &web_sys::Node = first.unchecked_ref();
    let last_node: &web_sys::Node = last.unchecked_ref();
    if !modal.contains(Some(active_node)) {
        let _ = if backwards {
            last.focus()
        } else {
            first.focus()
        };
        true
    } else if backwards && active_node.is_same_node(Some(first_node)) {
        let _ = last.focus();
        true
    } else if !backwards && active_node.is_same_node(Some(last_node)) {
        let _ = first.focus();
        true
    } else {
        false
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn trap_modal_focus(_backwards: bool) -> bool {
    false
}

#[cfg(test)]
mod tests {
    #[test]
    fn dialogs_expose_the_minimum_accessibility_contract() {
        let source = include_str!("modal.rs");
        assert!(source.contains("aria_modal: \"true\""));
        assert!(source.contains("aria_label: \"{title}\""));
        assert!(source.contains("aria_label:"));
        assert!(source.contains("aria_label: \"{close_label}\""));
        assert!(source.contains("Key::Escape"));
    }
}
