pub mod copy;
pub mod display;
pub mod password;
pub(crate) mod resource;
pub mod time;

pub use copy::on_copy;

/// Move keyboard focus to an element rendered by the current page.
pub fn focus_element(element_id: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;

        let Some(element) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id(element_id))
            .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            return;
        };
        let _ = element.focus();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = element_id;
    }
}

/// Restore keyboard focus to the control that opened a dialog after the
/// dialog has been removed from the DOM.
pub fn restore_focus_after_dialog(element_id: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        let element_id = element_id.to_string();
        let timeout = gloo_timers::callback::Timeout::new(0, move || {
            focus_element(&element_id);
        });
        timeout.forget();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = element_id;
    }
}

/// 复制文本到剪贴板（WASM 环境）
/// 返回 `true` 表示复制成功，`false` 表示不可用（非 HTTPS 上下文等）。
pub fn copy_to_clipboard(text: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let window = match web_sys::window() {
            Some(w) => w,
            None => return false,
        };
        let clipboard = window.navigator().clipboard();
        // navigator.clipboard 在非 HTTPS 下为 null/undefined
        let clipboard_ref: &wasm_bindgen::JsValue = clipboard.as_ref();
        if clipboard_ref.is_null() || clipboard_ref.is_undefined() {
            return false;
        }
        // write_text 返回 Promise，fire-and-forget 即可
        let _ = clipboard.write_text(text);
        true
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = text;
        false
    }
}

/// 截断字符串金额到两位小数（直接截断，不四舍五入）
pub fn format_money_str(value: &str) -> String {
    if let Some(dot_pos) = value.find('.') {
        let end = (dot_pos + 3).min(value.len());
        let mut result = value[..end].to_string();
        // 补齐到两位小数
        if result.len() == dot_pos + 1 {
            result.push_str("00");
        } else if result.len() == dot_pos + 2 {
            result.push('0');
        }
        result
    } else {
        format!("{value}.00")
    }
}

/// CNY 支付金额的两位小数展示格式。
pub fn format_cny_str(value: &str) -> String {
    format!("¥{}", format_money_str(value))
}

/// 展示高精度金额：至少保留两位小数，同时保留所有有效小数位。
///
/// 节点小费、分销收益和模型计费金额使用 `DECIMAL(20,10)`，不能套用支付金额的
/// 两位截断规则。这里仅移除两位小数之后无意义的尾零，不改变后端返回的金额。
pub fn format_precise_money_str(value: &str) -> String {
    let Some((integer, fraction)) = value.split_once('.') else {
        return format!("{value}.00");
    };

    let mut fraction = fraction.trim_end_matches('0').to_string();
    while fraction.len() < 2 {
        fraction.push('0');
    }

    format!("{integer}.{fraction}")
}

/// CNY 高精度金额展示格式。
pub fn format_precise_cny_str(value: &str) -> String {
    format!("¥{}", format_precise_money_str(value))
}

/// Format an integer-like value with thousands separators for scan-friendly
/// counters. Non-integer input is preserved unchanged.
pub fn format_count(value: impl ToString) -> String {
    let raw = value.to_string();
    let (sign, digits) = raw
        .strip_prefix('-')
        .map_or(("", raw.as_str()), |digits| ("-", digits));
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return raw;
    }
    let mut output = String::with_capacity(raw.len() + raw.len() / 3);
    output.push_str(sign);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            output.push(',');
        }
        output.push(character);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        format_cny_str, format_count, format_money_str, format_precise_cny_str,
        format_precise_money_str,
    };

    #[test]
    fn money_display_uses_two_decimal_places_consistently() {
        assert_eq!(format_money_str("12"), "12.00");
        assert_eq!(format_money_str("12.3"), "12.30");
        assert_eq!(format_money_str("12.345"), "12.34");
        assert_eq!(format_cny_str("5.2"), "¥5.20");
    }

    #[test]
    fn precise_money_display_preserves_sub_cent_values_and_trims_only_trailing_zeros() {
        assert_eq!(format_precise_money_str("12"), "12.00");
        assert_eq!(format_precise_money_str("12.3"), "12.30");
        assert_eq!(format_precise_money_str("12.3400000000"), "12.34");
        assert_eq!(format_precise_money_str("0.0090000000"), "0.009");
        assert_eq!(format_precise_money_str("1.999"), "1.999");
        assert_eq!(format_precise_cny_str("0.0000000001"), "¥0.0000000001");
    }

    #[test]
    fn counters_use_grouping_without_changing_non_integer_values() {
        assert_eq!(format_count(1234567), "1,234,567");
        assert_eq!(format_count(-1200), "-1,200");
        assert_eq!(format_count("unknown"), "unknown");
    }
}
