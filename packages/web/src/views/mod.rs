pub mod api_keys;
pub mod auth;
pub mod billing;
pub mod dashboard;
pub mod distribution;
pub mod error;
pub mod home;
pub mod node;
pub mod payments;
pub mod shared;
pub mod tenant;
pub mod usage;
pub mod user;

pub use billing::Billing;
pub use error::NotFound;
pub use home::Home;
pub use usage::Usage;

#[cfg(test)]
mod pagination_layout_tests {
    /// 全部含分页页脚的视图源文件（路径相对于本文件所在目录）。
    ///
    /// 分页页脚必须渲染为表格面板（`table-pagination-panel`）的兄弟节点而非子节点，
    /// 以便与定价管理页的页脚结构保持一致。
    const PAGINATED_VIEW_SOURCES: &[(&str, &str)] = &[
        ("api_keys/list.rs", include_str!("api_keys/list.rs")),
        (
            "distribution/overview.rs",
            include_str!("distribution/overview.rs"),
        ),
        (
            "node/node_earnings.rs",
            include_str!("node/node_earnings.rs"),
        ),
        ("payments/overview.rs", include_str!("payments/overview.rs")),
        ("shared/accounts.rs", include_str!("shared/accounts.rs")),
        (
            "shared/distribution_records.rs",
            include_str!("shared/distribution_records.rs"),
        ),
        ("shared/monitoring.rs", include_str!("shared/monitoring.rs")),
        (
            "shared/node_gateway.rs",
            include_str!("shared/node_gateway.rs"),
        ),
        (
            "shared/payment_orders.rs",
            include_str!("shared/payment_orders.rs"),
        ),
        ("shared/pricing.rs", include_str!("shared/pricing.rs")),
        ("shared/tenants.rs", include_str!("shared/tenants.rs")),
        ("shared/users.rs", include_str!("shared/users.rs")),
        ("usage.rs", include_str!("usage.rs")),
    ];

    /// 将字符串字面量、字符字面量与注释替换为空格（保持字节长度与换行位置不变），
    /// 让后续扫描只针对代码本身。
    fn mask_non_code(source: &str) -> String {
        let bytes = source.as_bytes();
        let mut masked = bytes.to_vec();
        let n = bytes.len();
        let mut i = 0;
        while i < n {
            let b = bytes[i];
            // Raw 字符串：r"..." 或 r#"..."#
            if b == b'r' && i + 1 < n && (bytes[i + 1] == b'"' || bytes[i + 1] == b'#') {
                let mut j = i + 1;
                let mut hashes = 0;
                while j < n && bytes[j] == b'#' {
                    hashes += 1;
                    j += 1;
                }
                if j < n && bytes[j] == b'"' {
                    j += 1;
                    while j < n {
                        if bytes[j] == b'"'
                            && j + 1 + hashes <= n
                            && bytes[j + 1..j + 1 + hashes].iter().all(|&c| c == b'#')
                        {
                            j += 1 + hashes;
                            break;
                        }
                        j += 1;
                    }
                    blank_range(&mut masked, i, j.min(n));
                    i = j.min(n);
                    continue;
                }
                i += 1;
                continue;
            }
            // 普通字符串字面量
            if b == b'"' {
                let mut j = i + 1;
                while j < n {
                    if bytes[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if bytes[j] == b'"' {
                        j += 1;
                        break;
                    }
                    j += 1;
                }
                blank_range(&mut masked, i, j.min(n));
                i = j.min(n);
                continue;
            }
            // 字符字面量（含转义）与生命周期前缀
            if b == b'\'' {
                if i + 1 < n && bytes[i + 1] == b'\\' {
                    let mut j = i + 2;
                    while j < n && bytes[j] != b'\'' {
                        j += 1;
                    }
                    let end = (j + 1).min(n);
                    blank_range(&mut masked, i, end);
                    i = end;
                    continue;
                }
                if i + 2 < n && bytes[i + 1] != b'\'' && bytes[i + 2] == b'\'' {
                    blank_range(&mut masked, i, i + 3);
                    i += 3;
                    continue;
                }
                i += 1;
                continue;
            }
            // 行注释
            if b == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
                let mut j = i;
                while j < n && bytes[j] != b'\n' {
                    j += 1;
                }
                blank_range(&mut masked, i, j);
                i = j;
                continue;
            }
            // 块注释
            if b == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
                let mut j = i + 2;
                while j + 1 < n && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                    j += 1;
                }
                let end = (j + 2).min(n);
                blank_range(&mut masked, i, end);
                i = end;
                continue;
            }
            i += 1;
        }
        String::from_utf8(masked).expect("掩码后文本应保持有效的 UTF-8")
    }

    /// 将 [start, end) 区间内的非换行字节替换为空格。
    fn blank_range(masked: &mut [u8], start: usize, end: usize) {
        for slot in masked.iter_mut().take(end).skip(start) {
            if *slot != b'\n' {
                *slot = b' ';
            }
        }
    }

    /// 在掩码文本上配对花括号，返回 (开括号位置, 闭括号位置) 列表。
    fn paired_braces(masked: &str) -> Vec<(usize, usize)> {
        let mut stack = Vec::new();
        let mut pairs = Vec::new();
        for (index, byte) in masked.bytes().enumerate() {
            match byte {
                b'{' => stack.push(index),
                b'}' => {
                    if let Some(open) = stack.pop() {
                        pairs.push((open, index));
                    }
                }
                _ => {}
            }
        }
        pairs
    }

    #[test]
    fn pagination_footers_render_outside_their_table_panels() {
        for &(name, source) in PAGINATED_VIEW_SOURCES {
            let masked = mask_non_code(source);
            let pairs = paired_braces(&masked);

            // 表格面板的字节范围：class 含 `table-pagination-panel` 的行上，
            // 代码区最后一个 `{` 即面板块起点。
            let mut panel_ranges = Vec::new();
            let mut line_start = 0usize;
            for line in source.split('\n') {
                if line.contains("table-pagination-panel") {
                    let masked_line = &masked[line_start..line_start + line.len()];
                    if let Some(offset) = masked_line.rfind('{') {
                        let open = line_start + offset;
                        if let Some(&(_, close)) =
                            pairs.iter().find(|(pair_open, _)| *pair_open == open)
                        {
                            panel_ranges.push((open, close));
                        }
                    }
                }
                line_start += line.len() + 1;
            }
            assert!(
                !panel_ranges.is_empty(),
                "{name} 应至少包含一个 table-pagination-panel 面板"
            );

            // 所有分页组件（Pagination 与 CursorPagination）都必须在面板之外渲染。
            let bytes = masked.as_bytes();
            let mut pagination_count = 0usize;
            for (index, _) in masked.match_indices("Pagination {") {
                let is_cursor = index >= 6
                    && bytes[index - 6..index].starts_with(b"Cursor")
                    && (index == 6
                        || !(bytes[index - 7].is_ascii_alphanumeric() || bytes[index - 7] == b'_'));
                let prefixed = index > 0
                    && (bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_');
                if prefixed && !is_cursor {
                    continue; // 形如 BalanceReservationPagination 的定义，并非分页组件
                }
                pagination_count += 1;
                let line = source[..index].matches('\n').count() + 1;
                for &(open, close) in &panel_ranges {
                    assert!(
                        !(open < index && index < close),
                        "{name} 第 {line} 行的分页组件嵌在 table-pagination-panel 面板内部，应移至面板之外"
                    );
                }
            }
            assert!(pagination_count > 0, "{name} 应包含至少一个分页组件");
        }
    }
}
