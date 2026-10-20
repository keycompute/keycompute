use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

pub const SEARCH_DEBOUNCE_MS: u32 = 300;
pub const SEARCH_MAX_LENGTH: usize = 255;

pub fn normalize_search(value: &str) -> String {
    value.trim().to_owned()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DebounceCandidate {
    revision: u64,
    value: String,
}

fn next_candidate(latest_revision: u64, input: &str) -> DebounceCandidate {
    DebounceCandidate {
        revision: latest_revision.wrapping_add(1),
        value: normalize_search(input),
    }
}

fn completed_candidate(
    latest_revision: u64,
    current_value: &str,
    candidate: &DebounceCandidate,
) -> Option<String> {
    (latest_revision == candidate.revision && current_value != candidate.value)
        .then(|| candidate.value.clone())
}

fn changed_search_page(current: &(String, u32), next: String) -> Option<(String, u32)> {
    (current.0 != next).then_some((next, 1))
}

/// Debounce remote searches from the latest edit, including ABA edits such as
/// `a -> ab -> a`. The monotonically increasing revision prevents an older
/// timer for the same text from committing the latest draft too early.
pub fn use_debounced_search(input: Signal<String>) -> Signal<String> {
    let mut debounced = use_signal(|| normalize_search(&input.peek()));
    let mut revision = use_signal(|| 0u64);

    use_effect(move || {
        let candidate = next_candidate(*revision.peek(), &input());
        revision.set(candidate.revision);
        spawn(async move {
            TimeoutFuture::new(SEARCH_DEBOUNCE_MS).await;
            let completed =
                completed_candidate(*revision.peek(), debounced.peek().as_str(), &candidate);
            if let Some(next) = completed {
                debounced.set(next);
            }
        });
    });

    debounced
}

/// Keep a remote search and its page in one atomic state. When the debounced
/// search changes, the page returns to one without briefly requesting either
/// the old search's first page or the new search's previous page.
pub fn use_debounced_search_page(input: Signal<String>) -> Signal<(String, u32)> {
    let debounced = use_debounced_search(input);
    let mut query = use_signal(|| (normalize_search(&input.peek()), 1));

    use_effect(move || {
        let next = debounced();
        let changed = changed_search_page(&query.peek(), next);
        if let Some(next) = changed {
            query.set(next);
        }
    });

    query
}

#[cfg(test)]
mod tests {
    use super::{
        SEARCH_DEBOUNCE_MS, SEARCH_MAX_LENGTH, changed_search_page, completed_candidate,
        next_candidate, normalize_search,
    };

    #[test]
    fn search_policy_is_short_trimmed_and_shared() {
        assert_eq!(normalize_search("  demo tenant  "), "demo tenant");
        assert!((250..=500).contains(&SEARCH_DEBOUNCE_MS));
        assert_eq!(SEARCH_MAX_LENGTH, 255);
    }

    #[test]
    fn every_remote_typeahead_uses_the_shared_debounce() {
        for (name, source, expected_call) in [
            (
                "accounts",
                include_str!("../views/shared/accounts.rs"),
                "use_debounced_search(search)",
            ),
            (
                "model tenant picker",
                include_str!("../views/shared/model_controls.rs"),
                "use_debounced_search_page(search_input)",
            ),
            (
                "passthrough list",
                include_str!("../views/shared/passthrough_bindings.rs"),
                "use_debounced_search_page(query_input)",
            ),
            (
                "passthrough account picker",
                include_str!("../views/shared/passthrough_bindings.rs"),
                "use_debounced_search_page(account_query_input)",
            ),
            (
                "passthrough tenant picker",
                include_str!("../views/shared/passthrough_bindings.rs"),
                "use_debounced_search_page(tenant_query_input)",
            ),
            (
                "pricing",
                include_str!("../views/shared/pricing.rs"),
                "use_debounced_search(search)",
            ),
            (
                "tenants",
                include_str!("../views/shared/tenants.rs"),
                "use_debounced_search(search)",
            ),
            (
                "users",
                include_str!("../views/shared/users.rs"),
                "use_debounced_search(search)",
            ),
        ] {
            assert_eq!(
                source.matches(expected_call).count(),
                1,
                "{name} must call its shared remote-search debounce exactly once"
            );
        }
    }

    #[test]
    fn only_the_latest_candidate_commits_across_aba_edits() {
        let first_a = next_candidate(0, "a");
        let intermediate_b = next_candidate(first_a.revision, "ab");
        let latest_a = next_candidate(intermediate_b.revision, "a");

        assert_eq!(completed_candidate(latest_a.revision, "", &first_a), None);
        assert_eq!(
            completed_candidate(latest_a.revision, "", &intermediate_b),
            None
        );
        assert_eq!(
            completed_candidate(latest_a.revision, "", &latest_a),
            Some("a".to_owned())
        );
        assert_eq!(completed_candidate(latest_a.revision, "a", &latest_a), None);
    }

    #[test]
    fn changed_paginated_search_commits_first_page_atomically() {
        assert_eq!(
            changed_search_page(&("old".to_owned(), 7), "new".to_owned()),
            Some(("new".to_owned(), 1))
        );
        assert_eq!(
            changed_search_page(&("same".to_owned(), 7), "same".to_owned()),
            None
        );
    }
}
