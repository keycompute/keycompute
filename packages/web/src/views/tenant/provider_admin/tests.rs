use super::{capability_mode, caps, models};
#[test]
fn model_input_is_deduplicated_without_rewriting_ids() {
    assert_eq!(models(" a, b,a "), ["a", "b"]);
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
