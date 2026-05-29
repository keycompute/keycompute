use dioxus::prelude::*;

/// 文本输入框组件
///
/// # 示例
/// ```rust,ignore
/// let mut email = use_signal(String::new);
/// Input {
///     label: "邮箱",
///     input_type: "email",
///     value: email,
///     placeholder: "请输入邮箱",
/// }
/// ```
#[component]
pub fn Input(
    /// 标签文字（为空时不渲染 label）
    #[props(default)]
    label: String,
    /// input type 属性（text / email / password / number 等）
    #[props(default = "text".to_string())]
    input_type: String,
    /// 受控值 Signal（双向绑定）
    #[props(default)]
    value: Signal<String>,
    /// placeholder 提示文字
    #[props(default)]
    placeholder: String,
    /// 是否禁用
    #[props(default = false)]
    disabled: bool,
    /// 是否必填
    #[props(default = false)]
    required: bool,
    /// 错误提示文字（非空时显示红色边框 + 错误信息）
    #[props(default)]
    error: String,
    /// 辅助提示文字（正常状态下显示在输入框下方）
    #[props(default)]
    hint: String,
    /// 额外 CSS 类名（作用于 .input-wrapper）
    #[props(default)]
    class: String,
    /// oninput 回调（非受控模式使用）
    #[props(default)]
    oninput: EventHandler<FormEvent>,
) -> Element {
    let has_error = !error.is_empty();
    let input_class = if has_error {
        "input-field error"
    } else {
        "input-field"
    };
    let wrapper_class = format!("input-wrapper {}", class.trim());

    rsx! {
        div { class: "{wrapper_class}",
            if !label.is_empty() {
                label {
                    class: "input-label",
                    "{label}"
                    if required {
                        span { class: "input-required", " *" }
                    }
                }
            }
            input {
                class: "{input_class}",
                r#type: "{input_type}",
                value: "{value}",
                placeholder: "{placeholder}",
                disabled,
                required,
                oninput: move |e| {
                    *value.write() = e.value();
                    oninput.call(e);
                },
            }
            if has_error {
                span { class: "input-error-msg", "{error}" }
            } else if !hint.is_empty() {
                span { class: "input-hint", "{hint}" }
            }
        }
    }
}

/// 多行文本域组件
#[component]
pub fn Textarea(
    /// 标签文字
    #[props(default)]
    label: String,
    /// 受控值 Signal
    #[props(default)]
    value: Signal<String>,
    /// placeholder
    #[props(default)]
    placeholder: String,
    /// 行数
    #[props(default = 4)]
    rows: u32,
    /// 是否禁用
    #[props(default = false)]
    disabled: bool,
    /// 错误提示
    #[props(default)]
    error: String,
    /// 辅助提示
    #[props(default)]
    hint: String,
    /// 额外 CSS 类名
    #[props(default)]
    class: String,
) -> Element {
    let has_error = !error.is_empty();
    let textarea_class = if has_error {
        "input-field error"
    } else {
        "input-field"
    };
    let wrapper_class = format!("input-wrapper {}", class.trim());

    rsx! {
        div { class: "{wrapper_class}",
            if !label.is_empty() {
                label { class: "input-label", "{label}" }
            }
            textarea {
                class: "{textarea_class}",
                rows: "{rows}",
                placeholder: "{placeholder}",
                disabled,
                oninput: move |e| {
                    *value.write() = e.value();
                },
                "{value}"
            }
            if has_error {
                span { class: "input-error-msg", "{error}" }
            } else if !hint.is_empty() {
                span { class: "input-hint", "{hint}" }
            }
        }
    }
}
