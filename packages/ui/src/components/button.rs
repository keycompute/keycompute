use dioxus::prelude::*;

/// 按钮变体
#[derive(Clone, PartialEq, Default)]
pub enum ButtonVariant {
    #[default]
    Primary,
    Secondary,
    Danger,
    Ghost,
}

/// 按钮尺寸
#[derive(Clone, PartialEq, Default)]
pub enum ButtonSize {
    Small,
    #[default]
    Medium,
    Large,
}

/// 通用按钮组件
///
/// # 示例
/// ```rust
/// Button {
///     variant: ButtonVariant::Primary,
///     onclick: move |_| { /* ... */ },
///     "提交"
/// }
/// ```
#[component]
pub fn Button(
    /// 按钮变体（默认 Primary）
    #[props(default)]
    variant: ButtonVariant,
    /// 尺寸（默认 Medium）
    #[props(default)]
    size: ButtonSize,
    /// 是否禁用
    #[props(default = false)]
    disabled: bool,
    /// 是否处于加载状态（显示 spinner，禁止点击）
    #[props(default = false)]
    loading: bool,
    /// 额外 CSS 类名
    #[props(default)]
    class: String,
    /// 点击事件回调
    #[props(default)]
    onclick: EventHandler<MouseEvent>,
    /// 按钮类型属性（submit / button / reset）
    #[props(default = "button".to_string())]
    r#type: String,
    /// 按钮内容
    children: Element,
) -> Element {
    let variant_class = match variant {
        ButtonVariant::Primary => "btn-primary",
        ButtonVariant::Secondary => "btn-secondary",
        ButtonVariant::Danger => "btn-danger",
        ButtonVariant::Ghost => "btn-ghost",
    };

    let size_class = match size {
        ButtonSize::Small => "btn-sm",
        ButtonSize::Medium => "",
        ButtonSize::Large => "btn-lg",
    };

    let loading_class = if loading { " loading" } else { "" };

    let full_class = format!(
        "btn {variant_class} {size_class}{loading_class} {class}",
        class = class.trim()
    );
    let full_class = full_class.trim().to_string();

    let is_disabled = disabled || loading;
    let button_type = r#type;

    rsx! {
        button {
            class: "{full_class}",
            r#type: "{button_type}",
            disabled: is_disabled,
            onclick: move |e| {
                if !is_disabled {
                    onclick.call(e);
                }
            },
            if loading {
                span { class: "btn-spinner", }
            }
            {children}
        }
    }
}
