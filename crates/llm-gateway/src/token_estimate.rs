//! Bounded CPU work for provisional usage. Never run an unbounded BPE merge
//! over attacker-controlled multi-megabyte text on a Tokio worker. Provider
//! final usage remains authoritative; fallback byte counts are deliberately
//! conservative estimates, NOT exact model tokens or output generation limits.
const MAX_EXACT_BYTES: usize = 8192;

pub(crate) fn estimate_tokens(content: &str) -> u32 {
    if content.is_empty() {
        return 0;
    }
    if content.len() > MAX_EXACT_BYTES {
        // Byte-level tokenization cannot require more nonempty tokens than
        // UTF-8 bytes. This bounds work without spawning uncancellable CPU jobs.
        return u32::try_from(content.len()).unwrap_or(u32::MAX);
    }
    let count = tiktoken_rs::o200k_base_singleton()
        .encode_with_special_tokens(content)
        .len();
    u32::try_from(count).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn small_estimates_remain_compatible() {
        for text in [
            "",
            "Hello",
            "你好世界",
            "normal tool output",
            "<|endoftext|>",
        ] {
            assert_eq!(
                estimate_tokens(text),
                tiktoken_rs::o200k_base_singleton()
                    .encode_with_special_tokens(text)
                    .len() as u32
            );
        }
    }
    #[test]
    fn large_unbroken_and_unicode_payloads_have_bounded_estimation() {
        for text in [
            "x".repeat(2 * 1024 * 1024),
            "你".repeat(100_000),
            "x".repeat(MAX_EXACT_BYTES + 1),
        ] {
            assert_eq!(estimate_tokens(&text) as usize, text.len());
        }
    }
}
