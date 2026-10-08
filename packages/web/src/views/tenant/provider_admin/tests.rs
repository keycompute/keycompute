use super::{capability_mode, caps, models, parse_account_limits};
#[test]
fn model_input_is_deduplicated_without_rewriting_ids() {
    assert_eq!(models(" a, b,a "), ["a", "b"]);
}

#[test]
fn provider_pool_controls_use_the_shared_checkbox_layout() {
    let source = include_str!("mod.rs");
    assert!(source.matches("label{class:\"checkbox-field\"").count() >= 2);
}
#[test]
fn capability_mode_round_trips_supported_protocol_sets() {
    assert_eq!(caps("anthropic", "both"), ["messages"]);
    assert_eq!(caps("openai", "chat_completions"), ["chat_completions"]);
    assert_eq!(caps("openai", "responses"), ["responses"]);
    assert_eq!(caps("openai", "both"), ["chat_completions", "responses"]);
    assert_eq!(
        capability_mode("openai", &["responses".into()]),
        "responses"
    );
    assert_eq!(
        capability_mode("openai", &["chat_completions".into()]),
        "chat_completions"
    );
    assert_eq!(
        capability_mode("openai", &["chat_completions".into(), "responses".into()]),
        "both"
    );
}

#[test]
fn provider_limits_reject_invalid_or_out_of_range_values() {
    assert_eq!(
        parse_account_limits("60", "100000", "0"),
        Some((60, 100000, 0))
    );
    assert_eq!(parse_account_limits("0", "100000", "0"), None);
    assert_eq!(parse_account_limits("60", "-1", "0"), None);
    assert_eq!(parse_account_limits("60", "100000", "11"), None);
    assert_eq!(parse_account_limits("rpm", "100000", "0"), None);
}
