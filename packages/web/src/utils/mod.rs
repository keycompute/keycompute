pub mod time;

/// 复制文本到剪贴板（WASM 环境）
pub fn copy_to_clipboard(text: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = web_sys::window().map(|w| {
            let clipboard = w.navigator().clipboard();
            clipboard.write_text(text)
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = text;
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

/// 将 f64 截断到两位小数后格式化（直接截断，不四舍五入）
pub fn format_money(value: f64) -> String {
    format_money_str(&format!("{value:.10}"))
}
