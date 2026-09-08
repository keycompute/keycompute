pub(crate) fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '%' => escaped.push_str(r"\%"),
            '_' => escaped.push_str(r"\_"),
            '\\' => escaped.push_str(r"\\"),
            value => escaped.push(value),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::escape_like_pattern;

    #[test]
    fn escapes_like_metacharacters() {
        assert_eq!(escape_like_pattern(r"acme_%\corp"), r"acme\_\%\\corp");
    }
}
