//! Bounded, identity/session-scoped caching and single-flight for safe console
//! display reads. The generic HTTP client never caches arbitrary GET requests.

use futures::FutureExt;
#[cfg(not(target_arch = "wasm32"))]
use futures::future::BoxFuture;
#[cfg(target_arch = "wasm32")]
use futures::future::LocalBoxFuture;
use reqwest::Url;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use web_time::Instant;

const MAX_ENTRIES: usize = 64;
const MAX_PENDING: usize = 64;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;
const FRESH_FOR: Duration = Duration::from_secs(2);

#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub(crate) struct CacheKey {
    pub(crate) origin: String,
    pub(crate) credential: [u8; 32],
    pub(crate) generation: u64,
    pub(crate) path_query: String,
    /// The wire representation, rather than the Rust destination type. This
    /// lets two pages using different Rust structs share one JSON response.
    pub(crate) representation: &'static str,
}

struct Entry {
    value: Value,
    expires_at: Instant,
    last_used: u64,
    bytes: usize,
}

struct Flight {
    id: u64,
    epoch: u64,
    handle: Weak<FlightHandle>,
}

struct FlightHandle {
    future: SharedCacheFuture,
}

#[cfg(target_arch = "wasm32")]
type CacheFuture = LocalBoxFuture<'static, crate::Result<Value>>;
#[cfg(not(target_arch = "wasm32"))]
type CacheFuture = BoxFuture<'static, crate::Result<Value>>;
type SharedCacheFuture = futures::future::Shared<CacheFuture>;

#[cfg(target_arch = "wasm32")]
pub(crate) fn box_cache_future<F>(future: F) -> CacheFuture
where
    F: std::future::Future<Output = crate::Result<Value>> + 'static,
{
    future.boxed_local()
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn box_cache_future<F>(future: F) -> CacheFuture
where
    F: std::future::Future<Output = crate::Result<Value>> + Send + 'static,
{
    future.boxed()
}

/// Each waiter owns its delivery fence. Sharing this outer future would allow
/// a delayed clone to reuse a result that passed its fence before invalidation.
fn caller_future(handle: Arc<FlightHandle>, state: Weak<Mutex<State>>, epoch: u64) -> CacheFuture {
    box_cache_future(async move {
        let result = handle.future.clone().await;
        fence_delivery(&state, epoch, result)
    })
}

fn fence_delivery(
    state: &Weak<Mutex<State>>,
    epoch: u64,
    result: crate::Result<Value>,
) -> crate::Result<Value> {
    let current = state.upgrade().is_some_and(|state| {
        state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .epoch
            == epoch
    });
    if current
        || matches!(
            &result,
            Err(crate::ClientError::Unauthorized(_) | crate::ClientError::Forbidden(_))
        )
    {
        result
    } else {
        Err(crate::ClientError::Other(
            "console read invalidated before delivery".into(),
        ))
    }
}

#[derive(Default)]
pub(crate) struct State {
    entries: HashMap<CacheKey, Entry>,
    pending: HashMap<CacheKey, Flight>,
    /// Weak references make cancellation observable: when all callers drop a
    /// shared future, the flight can be reclaimed on the next cache operation.
    flights: HashMap<u64, Weak<FlightHandle>>,
    epoch: u64,
    next_flight_id: u64,
    clock: u64,
    cache_bytes: usize,
}

#[derive(Clone, Default)]
pub(crate) struct QueryCache {
    state: Arc<Mutex<State>>,
}

/// Invalidates every completion path, including dropping a cancelled command.
pub(crate) struct MutationFence(QueryCache);

impl Drop for MutationFence {
    fn drop(&mut self) {
        self.0.invalidate_all();
    }
}

impl QueryCache {
    pub(crate) fn mutation_fence(&self) -> MutationFence {
        self.invalidate_all();
        MutationFence(self.clone())
    }

    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
    pub(crate) fn get_or_start<F>(&self, key: CacheKey, request: F) -> CacheFuture
    where
        F: FnOnce(Weak<Mutex<State>>, CacheKey, u64, u64) -> CacheFuture,
    {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let expired: Vec<_> = state
            .entries
            .iter()
            .filter(|(_, entry)| entry.expires_at <= now)
            .map(|(key, _)| key.clone())
            .collect();
        for key in expired {
            if let Some(entry) = state.entries.remove(&key) {
                state.cache_bytes = state.cache_bytes.saturating_sub(entry.bytes);
            }
        }
        state.flights.retain(|_, flight| flight.upgrade().is_some());
        state
            .pending
            .retain(|_, flight| flight.handle.upgrade().is_some());

        if state.entries.contains_key(&key) {
            state.clock = state.clock.saturating_add(1);
            let last_used = state.clock;
            let entry = state.entries.get_mut(&key).expect("entry checked above");
            entry.last_used = last_used;
            let value = entry.value.clone();
            let expires_at = entry.expires_at;
            let epoch = state.epoch;
            let owner = Arc::downgrade(&self.state);
            return box_cache_future(async move {
                if Instant::now() >= expires_at {
                    return Err(crate::ClientError::Other(
                        "console read expired before delivery".into(),
                    ));
                }
                fence_delivery(&owner, epoch, Ok(value))
            });
        }

        if let Some(flight) = state.pending.get(&key)
            && let Some(handle) = flight.handle.upgrade()
        {
            return caller_future(handle, Arc::downgrade(&self.state), flight.epoch);
        }

        // Never evict a live flight merely to admit more work. The caller gets
        // a finite, explicit error and may retry after an existing flight ends.
        if state.flights.len() >= MAX_PENDING {
            return box_cache_future(futures::future::ready(Err(crate::ClientError::Other(
                "console read concurrency limit reached".into(),
            ))));
        }

        let id = state.next_flight_id;
        state.next_flight_id = state.next_flight_id.wrapping_add(1);
        let epoch = state.epoch;
        let future = request(Arc::downgrade(&self.state), key.clone(), id, epoch).shared();
        let handle = Arc::new(FlightHandle {
            future: future.clone(),
        });
        let weak_handle = Arc::downgrade(&handle);
        state.flights.insert(id, weak_handle.clone());
        state.pending.insert(
            key,
            Flight {
                id,
                epoch,
                handle: weak_handle,
            },
        );
        caller_future(handle, Arc::downgrade(&self.state), epoch)
    }

    /// Finish a flight. A result from an invalidated or replaced flight is
    /// never cached or delivered as display data. Authentication errors remain
    /// visible so callers cannot mistake an invalidation for authorization.
    pub(crate) fn finish(
        state: &Weak<Mutex<State>>,
        key: &CacheKey,
        id: u64,
        epoch: u64,
        result: &crate::Result<Value>,
    ) -> crate::Result<Value> {
        let Some(state) = state.upgrade() else {
            return result.clone();
        };
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        let valid = state.epoch == epoch
            && state
                .pending
                .get(key)
                .is_some_and(|flight| flight.id == id && flight.epoch == epoch);
        if valid {
            state.pending.remove(key);
        }
        state.flights.remove(&id);
        if !valid {
            return match result {
                Err(crate::ClientError::Unauthorized(_))
                | Err(crate::ClientError::Forbidden(_)) => result.clone(),
                _ => Err(crate::ClientError::Other(
                    "console read invalidated before completion".into(),
                )),
            };
        }

        let Ok(value) = result else {
            return result.clone();
        };
        let bytes = serde_json::to_vec(value)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX);
        let fresh_for = value
            .get("cache_max_age_ms")
            .and_then(Value::as_u64)
            .map(Duration::from_millis)
            .unwrap_or(FRESH_FOR)
            .min(FRESH_FOR);
        if bytes > MAX_RESPONSE_BYTES || fresh_for.is_zero() {
            return Ok(value.clone());
        }
        state.clock = state.clock.saturating_add(1);
        let last_used = state.clock;
        if let Some(previous) = state.entries.remove(key) {
            state.cache_bytes = state.cache_bytes.saturating_sub(previous.bytes);
        }
        while (!state.entries.contains_key(key) && state.entries.len() >= MAX_ENTRIES)
            || state.cache_bytes.saturating_add(bytes) > MAX_CACHE_BYTES
        {
            if let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            {
                if let Some(entry) = state.entries.remove(&oldest) {
                    state.cache_bytes = state.cache_bytes.saturating_sub(entry.bytes);
                }
            } else {
                break;
            }
        }
        state.entries.insert(
            key.clone(),
            Entry {
                value: value.clone(),
                expires_at: Instant::now() + fresh_for,
                last_used,
                bytes,
            },
        );
        state.cache_bytes = state.cache_bytes.saturating_add(bytes);
        Ok(value.clone())
    }

    pub(crate) fn invalidate_all(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.entries.clear();
        state.cache_bytes = 0;
        state.pending.clear();
        state.epoch = state.epoch.wrapping_add(1);
        state.flights.retain(|_, flight| flight.upgrade().is_some());
    }
}

pub(crate) fn cache_key(
    url: &str,
    token: &str,
    generation: u64,
    representation: &'static str,
) -> Option<CacheKey> {
    let url = Url::parse(url).ok()?;
    let mut pairs: Vec<_> = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    // Duplicate parameter order can be significant (first/last-value parsers).
    // Stable sorting by name preserves each duplicate-name subsequence.
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let path_query = if pairs.is_empty() {
        url.path().to_string()
    } else {
        let query = pairs
            .iter()
            .map(|(key, value)| {
                format!(
                    "{}={}",
                    urlencoding::encode(key),
                    urlencoding::encode(value)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        format!("{}?{query}", url.path())
    };
    let mut digest = Sha256::new();
    digest.update(token.as_bytes());
    Some(CacheKey {
        origin: url.origin().ascii_serialization(),
        credential: digest.finalize().into(),
        generation,
        path_query,
        representation,
    })
}

pub(crate) const JSON_REPRESENTATION: &str = "application/json";

pub(crate) fn is_allowlisted(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    matches!(
        path,
        "/api/v1/me/distribution/earnings"
            | "/api/v1/me/distribution/overview"
            | "/api/v1/dashboard/overview"
            | "/api/v1/usage/trend"
            | "/api/v1/me/distribution/referrals"
            | "/api/v1/payments/balance"
            | "/api/v1/payments/methods"
            | "/api/v1/payments/orders"
            | "/api/v1/usage"
            | "/api/v1/usage/stats"
            | "/api/v1/distribution/records"
            | "/api/v1/distribution/stats"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn key_contains_only_fingerprint_and_normalized_query() {
        let a = cache_key(
            "https://example.test/api/v1/usage?b=2&a=1",
            "secret",
            7,
            "x",
        )
        .unwrap();
        let b = cache_key(
            "https://example.test/api/v1/usage?a=1&b=2",
            "secret",
            7,
            "x",
        )
        .unwrap();
        assert_eq!(a, b);
        assert!(!format!("{a:?}").contains("secret"));
    }

    #[test]
    fn canonical_query_reencodes_delimiters_and_preserves_duplicates() {
        let encoded = cache_key("https://example.test/x?a=x%26b%3D2", "t", 0, "x").unwrap();
        let split = cache_key("https://example.test/x?a=x&b=2", "t", 0, "x").unwrap();
        assert_ne!(encoded.path_query, split.path_query);
        let duplicate = cache_key("https://example.test/x?a=1&a=2", "t", 0, "x").unwrap();
        let reordered = cache_key("https://example.test/x?a=2&a=1", "t", 0, "x").unwrap();
        assert_ne!(duplicate, reordered);
    }

    #[test]
    fn allowlist_excludes_sensitive_and_arbitrary_gets() {
        assert!(is_allowlisted("/api/v1/payments/balance"));
        assert!(is_allowlisted("/api/v1/usage?limit=20"));
        assert!(!is_allowlisted("/api/v1/me"));
        assert!(!is_allowlisted("/api/v1/me/referral/code"));
        assert!(!is_allowlisted("/api/v1/payments/orders/secret"));
        assert!(!is_allowlisted("/api/v1/admin/secret"));
    }

    fn test_key(path: &str, generation: u64) -> CacheKey {
        cache_key(
            &format!("https://example.test{path}"),
            "token",
            generation,
            "json",
        )
        .expect("test URL is valid")
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_flight_and_cache_success() {
        let cache = QueryCache::default();
        let starts = Arc::new(AtomicUsize::new(0));
        let key = test_key("/api/v1/usage", 1);
        let make = |starts: Arc<AtomicUsize>| {
            let cache = cache.clone();
            let key = key.clone();
            cache.get_or_start(key, move |state, key, id, epoch| {
                starts.fetch_add(1, Ordering::SeqCst);
                box_cache_future(async move {
                    let result = Ok(serde_json::json!({"requests": 1}));
                    QueryCache::finish(&state, &key, id, epoch, &result)
                })
            })
        };
        let first = make(starts.clone());
        let second = make(starts.clone());
        let (first, second) = futures::join!(first, second);
        assert_eq!(first.unwrap(), serde_json::json!({"requests": 1}));
        assert_eq!(second.unwrap(), serde_json::json!({"requests": 1}));
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        let cached = make(starts).await.unwrap();
        assert_eq!(cached, serde_json::json!({"requests": 1}));
    }

    #[tokio::test]
    async fn successful_entries_expire_after_the_freshness_budget() {
        let cache = QueryCache::default();
        let starts = Arc::new(AtomicUsize::new(0));
        let key = test_key("/api/v1/usage", 1);
        let request = |cache: &QueryCache, starts: &Arc<AtomicUsize>| {
            cache.get_or_start(key.clone(), {
                let starts = starts.clone();
                move |state, key, id, epoch| {
                    starts.fetch_add(1, Ordering::SeqCst);
                    box_cache_future(async move {
                        let result = Ok(serde_json::json!({"fresh": true}));
                        QueryCache::finish(&state, &key, id, epoch, &result)
                    })
                }
            })
        };
        request(&cache, &starts).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2_050)).await;
        request(&cache, &starts).await.unwrap();
        assert_eq!(starts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn identity_generation_and_invalidation_fence_results() {
        let cache = QueryCache::default();
        let starts = Arc::new(AtomicUsize::new(0));
        let first_key = test_key("/api/v1/usage", 1);
        let second_key = test_key("/api/v1/usage", 2);
        let first = cache.get_or_start(first_key.clone(), {
            let starts = starts.clone();
            move |state, key, id, epoch| {
                starts.fetch_add(1, Ordering::SeqCst);
                box_cache_future(async move {
                    let result = Ok(serde_json::json!({"generation": 1}));
                    QueryCache::finish(&state, &key, id, epoch, &result)
                })
            }
        });
        cache.invalidate_all();
        let old = first.await;
        assert!(old.is_err());
        let second = cache.get_or_start(second_key, {
            let starts = starts.clone();
            move |state, key, id, epoch| {
                starts.fetch_add(1, Ordering::SeqCst);
                box_cache_future(async move {
                    let result = Ok(serde_json::json!({"generation": 2}));
                    QueryCache::finish(&state, &key, id, epoch, &result)
                })
            }
        });
        assert_eq!(second.await.unwrap(), serde_json::json!({"generation": 2}));
        assert_eq!(starts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn dropped_callers_release_flights_and_capacity_is_bounded() {
        let cache = QueryCache::default();
        let mut flights = Vec::new();
        for index in 0..MAX_PENDING {
            let key = test_key(&format!("/api/v1/usage?index={index}"), 1);
            flights.push(cache.get_or_start(key, |_, _, _, _| {
                box_cache_future(async { Ok(serde_json::json!({"pending": true})) })
            }));
        }
        let rejected = cache.get_or_start(test_key("/api/v1/usage?overflow=1", 1), |_, _, _, _| {
            box_cache_future(async { Ok(serde_json::json!({"unexpected": true})) })
        });
        assert!(
            matches!(rejected.await, Err(crate::ClientError::Other(message)) if message.contains("limit"))
        );
        drop(flights);
        let admitted = cache
            .get_or_start(test_key("/api/v1/usage?after-drop=1", 1), |_, _, _, _| {
                box_cache_future(async { Ok(serde_json::json!({"admitted": true})) })
            });
        assert_eq!(
            admitted.await.unwrap(),
            serde_json::json!({"admitted": true})
        );
    }
    #[tokio::test]
    async fn entry_and_serialized_byte_budgets_are_enforced() {
        let cache = QueryCache::default();
        async fn insert(cache: &QueryCache, index: usize, value: Value) {
            cache
                .get_or_start(
                    test_key(&format!("/api/v1/usage?page={index}"), 1),
                    move |state, key, id, epoch| {
                        box_cache_future(async move {
                            QueryCache::finish(&state, &key, id, epoch, &Ok(value))
                        })
                    },
                )
                .await
                .unwrap();
        }
        for index in 0..MAX_ENTRIES + 10 {
            insert(&cache, index, serde_json::json!({"small":index})).await;
        }
        assert_eq!(cache.state.lock().unwrap().entries.len(), MAX_ENTRIES);
        cache.invalidate_all();
        insert(&cache, 0, Value::String("x".repeat(MAX_RESPONSE_BYTES + 1))).await;
        assert!(cache.state.lock().unwrap().entries.is_empty());
        for index in 0..24 {
            insert(
                &cache,
                index,
                Value::String("x".repeat(MAX_RESPONSE_BYTES / 2)),
            )
            .await;
        }
        let state = cache.state.lock().unwrap();
        assert!(state.cache_bytes <= MAX_CACHE_BYTES);
        assert!(state.entries.len() < 24);
    }

    #[tokio::test]
    async fn invalidation_does_not_hide_still_active_flights_from_budget() {
        let cache = QueryCache::default();
        let active = (0..MAX_PENDING)
            .map(|i| {
                cache.get_or_start(
                    test_key(&format!("/api/v1/usage?page={i}"), 1),
                    |_, _, _, _| box_cache_future(futures::future::pending()),
                )
            })
            .collect::<Vec<_>>();
        cache.invalidate_all();
        let rejected = cache
            .get_or_start(test_key("/api/v1/usage?new=1", 2), |_, _, _, _| {
                box_cache_future(async { Ok(Value::Null) })
            })
            .await;
        assert!(rejected.is_err());
        drop(active);
        let result = cache
            .get_or_start(test_key("/api/v1/usage?new=2", 2), |_, _, _, _| {
                box_cache_future(async { Ok(Value::Null) })
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn completed_shared_waiter_must_not_deliver_after_invalidation() {
        let cache = QueryCache::default();
        let key = test_key("/api/v1/usage", 1);
        let first = cache.get_or_start(key.clone(), |state, key, id, epoch| {
            box_cache_future(async move {
                QueryCache::finish(
                    &state,
                    &key,
                    id,
                    epoch,
                    &Ok(serde_json::json!({"old": true})),
                )
            })
        });
        let delayed = cache.get_or_start(key, |_, _, _, _| panic!("must coalesce"));
        assert!(first.await.is_ok());
        cache.invalidate_all();
        assert!(
            delayed.await.is_err(),
            "completed result must be fenced for every waiter"
        );
    }

    #[tokio::test]
    async fn ready_cache_hit_is_fenced_before_delivery() {
        let cache = QueryCache::default();
        let key = test_key("/api/v1/usage", 1);
        cache
            .get_or_start(key.clone(), |state, key, id, epoch| {
                box_cache_future(async move {
                    QueryCache::finish(
                        &state,
                        &key,
                        id,
                        epoch,
                        &Ok(serde_json::json!({"old": true})),
                    )
                })
            })
            .await
            .unwrap();
        let hit = cache.get_or_start(key, |_, _, _, _| panic!("must hit cache"));
        cache.invalidate_all();
        assert!(hit.await.is_err());
    }

    #[tokio::test]
    async fn delayed_waiter_preserves_authorization_errors_after_invalidation() {
        for denied in [
            crate::ClientError::Unauthorized("revoked".into()),
            crate::ClientError::Forbidden("denied".into()),
        ] {
            let cache = QueryCache::default();
            let key = test_key("/api/v1/usage", 1);
            let first = cache.get_or_start(key.clone(), move |state, key, id, epoch| {
                box_cache_future(async move {
                    QueryCache::finish(&state, &key, id, epoch, &Err(denied))
                })
            });
            let delayed = cache.get_or_start(key, |_, _, _, _| panic!("must coalesce"));
            assert!(first.await.is_err());
            cache.invalidate_all();
            assert!(matches!(
                delayed.await,
                Err(crate::ClientError::Unauthorized(_) | crate::ClientError::Forbidden(_))
            ));
        }
    }
}
