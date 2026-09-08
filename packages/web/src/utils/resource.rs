use dioxus::prelude::UseResourceState;

/// A resource result tagged with the exact inputs that produced it.
///
/// Dioxus keeps the previous resource value while a dependency-triggered
/// request is pending. Tagging the value lets list views reject that stale
/// result during the render that changes the query, before the resource state
/// has necessarily transitioned to `Pending`.
#[derive(Clone, Debug)]
pub(crate) struct KeyedResourceValue<K, V> {
    key: K,
    value: V,
}

impl<K, V> KeyedResourceValue<K, V> {
    pub(crate) fn new(key: K, value: V) -> Self {
        Self { key, value }
    }
}

/// Returns a resource value only when it is ready and belongs to the current
/// request. Callers treat `None` as loading, so stale rows and their actions are
/// never rendered for a newly selected page or search.
pub(crate) fn current_keyed_value<K, V>(
    current_key: &K,
    state: UseResourceState,
    loaded: Option<KeyedResourceValue<K, V>>,
) -> Option<V>
where
    K: PartialEq,
{
    if state != UseResourceState::Ready {
        return None;
    }

    loaded
        .filter(|loaded| &loaded.key == current_key)
        .map(|loaded| loaded.value)
}

#[cfg(test)]
mod tests {
    use super::{KeyedResourceValue, current_keyed_value};
    use dioxus::prelude::UseResourceState;

    #[test]
    fn ready_value_for_the_current_request_is_visible() {
        let key = (2u32, "current search".to_string());
        let loaded = KeyedResourceValue::new(key.clone(), vec!["row"]);

        assert_eq!(
            current_keyed_value(&key, UseResourceState::Ready, Some(loaded)),
            Some(vec!["row"])
        );
    }

    #[test]
    fn pending_resource_does_not_expose_its_previous_value() {
        let key = (2u32, "search".to_string());
        let loaded = KeyedResourceValue::new(key.clone(), vec!["old row"]);

        assert_eq!(
            current_keyed_value(&key, UseResourceState::Pending, Some(loaded)),
            None
        );
    }

    #[test]
    fn ready_value_for_a_different_search_on_the_same_page_is_stale() {
        let loaded = KeyedResourceValue::new((1u32, "old".to_string()), vec!["old row"]);
        let current = (1u32, "new".to_string());

        assert_eq!(
            current_keyed_value(&current, UseResourceState::Ready, Some(loaded)),
            None
        );
    }
}
