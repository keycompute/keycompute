//! Bounded presentation-only cache. Live authorization belongs to the handler.
//! Local mutations fence delivery/refill; other instances may lag at most TTL.
use crate::{ApiError, Result, extractors::AuthExtractor};
use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use keycompute_cache::CacheService;
use keycompute_runtime::admission::BoundedAdmission;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use uuid::Uuid;
const TTL: Duration = Duration::from_secs(5);
const CACHE_WAIT: Duration = Duration::from_millis(100);
const QUERY_WAIT: Duration = Duration::from_secs(3);
const MAX_ENTRIES: usize = 128;
const MAX_FLIGHTS: usize = 64;
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_VALUE_BYTES: usize = 256 * 1024;
#[derive(Clone)]
struct Snapshot {
    value: Arc<Value>,
    expires: Instant,
}
type FlightFuture = Shared<BoxFuture<'static, Result<Snapshot>>>;
struct Flight {
    future: FlightFuture,
}
struct Entry {
    snapshot: Snapshot,
    bytes: usize,
    last: u64,
}
#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    flights: HashMap<String, (u64, Weak<Flight>)>,
    live: HashMap<u64, Weak<Flight>>,
    epoch: u64,
    sequence: u64,
    clock: u64,
    bytes: usize,
    redis_blocked_until: Option<Instant>,
}
#[derive(Default)]
struct Metrics {
    hit: AtomicU64,
    miss: AtomicU64,
    coalesced: AtomicU64,
    l2_hit: AtomicU64,
    fallback: AtomicU64,
    origin: AtomicU64,
    rejected: AtomicU64,
}
#[derive(Clone, Default)]
pub struct DisplayCache {
    state: Arc<Mutex<State>>,
    metrics: Arc<Metrics>,
}
#[derive(Serialize, Deserialize)]
struct Envelope {
    born_ms: i64,
    value: Value,
}
pub struct MutationGuard(DisplayCache);
impl Drop for MutationGuard {
    fn drop(&mut self) {
        self.0.invalidate();
    }
}
fn unavailable(message: &str) -> ApiError {
    ApiError::ServiceUnavailable(message.into())
}
impl DisplayCache {
    pub fn invalidate(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.epoch = state.epoch.wrapping_add(1);
        state.entries.clear();
        state.bytes = 0;
        state.flights.clear();
        // Old L2 snapshots and concurrent pre-mutation producers expire within
        // one lifetime. Until then only primary-origin reads can refill L1.
        state.redis_blocked_until = Some(Instant::now() + TTL);
        state.live.retain(|_, v| v.strong_count() != 0);
    }
    pub fn mutation_guard(&self) -> MutationGuard {
        self.invalidate();
        MutationGuard(self.clone())
    }
    pub fn metrics(&self) -> Value {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let m = &self.metrics;
        serde_json::json!({"hit":m.hit.load(Ordering::Relaxed),"miss":m.miss.load(Ordering::Relaxed),
            "coalesced":m.coalesced.load(Ordering::Relaxed),"l2_hit":m.l2_hit.load(Ordering::Relaxed),
            "fallback":m.fallback.load(Ordering::Relaxed),"origin":m.origin.load(Ordering::Relaxed),
            "rejected":m.rejected.load(Ordering::Relaxed),"entries":s.entries.len(),"serialized_bytes":s.bytes,
            "active_flights":s.live.values().filter(|v|v.strong_count()!=0).count()})
    }
    /// Fixed-size key binds visibility dimensions, never raw credentials.
    pub fn key(auth: &AuthExtractor, resource: &str, query: &str) -> String {
        let mut h = Sha256::new();
        for p in [
            auth.tenant_id.to_string(),
            auth.user_id.to_string(),
            auth.produce_ai_key_id.to_string(),
            auth.role.clone(),
            format!("{:?}", auth.permissions),
            resource.to_owned(),
            query.to_owned(),
        ] {
            h.update((p.len() as u64).to_be_bytes());
            h.update(p.as_bytes());
        }
        format!("console-display:v1:{:x}", h.finalize())
    }
    pub async fn read<F>(
        &self,
        l2: Arc<CacheService>,
        gate: Arc<BoundedAdmission>,
        tenant: Uuid,
        key: String,
        query: F,
    ) -> Result<Value>
    where
        F: std::future::Future<Output = Result<Value>> + Send + 'static,
    {
        let (epoch, ready, flight) = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.entries.retain(|_, e| e.snapshot.expires > Instant::now());
            s.bytes = s.entries.values().map(|e| e.bytes).sum();
            s.live.retain(|_, f| f.strong_count() != 0);
            s.flights.retain(|_, (_, f)| f.strong_count() != 0);
            s.clock = s.clock.wrapping_add(1);
            let clock = s.clock;
            let epoch = s.epoch;
            if let Some(e) = s.entries.get_mut(&key) {
                e.last = clock;
                self.metrics.hit.fetch_add(1, Ordering::Relaxed);
                (epoch, Some(e.snapshot.clone()), None)
            } else if let Some(handle) = s.flights.get(&key).and_then(|(_, f)| f.upgrade()) {
                self.metrics.coalesced.fetch_add(1, Ordering::Relaxed);
                (epoch, None, Some(handle))
            } else {
                if s.live.len() >= MAX_FLIGHTS {
                    self.metrics.rejected.fetch_add(1, Ordering::Relaxed);
                    return Err(unavailable("Display query capacity exhausted"));
                }
                self.metrics.miss.fetch_add(1, Ordering::Relaxed);
                let id = s.sequence;
                s.sequence = s.sequence.wrapping_add(1);
                let weak = Arc::downgrade(&self.state);
                let m = self.metrics.clone();
                let request_key = key.clone();
                let allow_l2 = s
                    .redis_blocked_until
                    .is_none_or(|until| until <= Instant::now());
                let future = async move {
                    let result = load(l2, gate, tenant, &request_key, allow_l2, query, m).await;
                    finish(&weak, &request_key, id, epoch, result)
                }
                .boxed()
                .shared();
                let handle = Arc::new(Flight { future });
                s.live.insert(id, Arc::downgrade(&handle));
                s.flights.insert(key, (id, Arc::downgrade(&handle)));
                (epoch, None, Some(handle))
            }
        };
        // EACH waiter is fenced, including delayed waiters of completed work.
        let result = match ready {
            Some(value) => Ok(value),
            None => {
                let handle = flight.expect("cache hit or live flight");
                handle.future.clone().await
            }
        };
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.epoch != epoch {
            return match result {
                Err(e @ (ApiError::Auth(_) | ApiError::Forbidden(_))) => Err(e),
                _ => Err(unavailable("Display query was invalidated; retry the read")),
            };
        }
        let snapshot = result?;
        if snapshot.expires <= Instant::now() {
            return Err(unavailable("Display snapshot expired; retry the read"));
        }
        let mut value = (*snapshot.value).clone();
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "cache_max_age_ms".into(),
                Value::from(
                    snapshot
                        .expires
                        .saturating_duration_since(Instant::now())
                        .as_millis() as u64,
                ),
            );
        }
        Ok(value)
    }
}
fn finish(
    weak: &Weak<Mutex<State>>,
    key: &str,
    id: u64,
    epoch: u64,
    result: Result<Snapshot>,
) -> Result<Snapshot> {
    let Some(state) = weak.upgrade() else {
        return result;
    };
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    s.live.remove(&id);
    let valid = s.epoch == epoch
        && s.flights
            .get(key)
            .is_some_and(|(current, _)| *current == id);
    if !valid {
        return match result {
            Err(e @ (ApiError::Auth(_) | ApiError::Forbidden(_))) => Err(e),
            _ => Err(unavailable("Display query invalidated before completion")),
        };
    }
    s.flights.remove(key);
    if let Ok(snapshot) = &result {
        let bytes = serde_json::to_vec(&*snapshot.value)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        if bytes <= MAX_VALUE_BYTES && snapshot.expires > Instant::now() {
            while s.entries.len() >= MAX_ENTRIES || s.bytes.saturating_add(bytes) > MAX_BYTES {
                let Some(oldest) = s
                    .entries
                    .iter()
                    .min_by_key(|(_, e)| e.last)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                if let Some(e) = s.entries.remove(&oldest) {
                    s.bytes = s.bytes.saturating_sub(e.bytes);
                }
            }
            s.clock = s.clock.wrapping_add(1);
            let last = s.clock;
            if let Some(old) = s.entries.insert(
                key.to_owned(),
                Entry {
                    snapshot: snapshot.clone(),
                    bytes,
                    last,
                },
            ) {
                s.bytes = s.bytes.saturating_sub(old.bytes);
            }
            s.bytes = s.bytes.saturating_add(bytes);
        }
    }
    result
}
async fn load<F>(
    l2: Arc<CacheService>,
    gate: Arc<BoundedAdmission>,
    tenant: Uuid,
    key: &str,
    allow_l2: bool,
    query: F,
    m: Arc<Metrics>,
) -> Result<Snapshot>
where
    F: std::future::Future<Output = Result<Value>> + Send,
{
    let l2_started = Instant::now();
    if allow_l2 {
        match tokio::time::timeout(
            CACHE_WAIT,
            l2.get_with_ttl::<Envelope>(key, MAX_VALUE_BYTES + 256),
        )
        .await
        {
            Ok(Ok(Some((envelope, remaining)))) if remaining <= TTL => {
                m.l2_hit.fetch_add(1, Ordering::Relaxed);
                return Ok(Snapshot {
                    value: Arc::new(envelope.value),
                    expires: l2_started + remaining,
                });
            }
            Ok(Ok(None)) => {}
            _ => {
                m.fallback.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    let _permit = gate.acquire(tenant).await.map_err(|_| {
        m.rejected.fetch_add(1, Ordering::Relaxed);
        unavailable("Display database capacity exhausted")
    })?;
    m.origin.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let born_ms = chrono::Utc::now().timestamp_millis();
    let value = tokio::time::timeout(QUERY_WAIT, query)
        .await
        .map_err(|_| unavailable("Display query deadline exceeded"))??;
    let bytes = serde_json::to_vec(&value)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    // Release database admission before the optional cache write.
    drop(_permit);
    let remaining = TTL.saturating_sub(started.elapsed());
    if bytes <= MAX_VALUE_BYTES && remaining.as_secs() > 0 {
        let envelope = Envelope {
            born_ms,
            value: value.clone(),
        };
        if !matches!(
            tokio::time::timeout(CACHE_WAIT, l2.set(key, &envelope, remaining)).await,
            Ok(Ok(()))
        ) {
            m.fallback.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(Snapshot {
        value: Arc::new(value),
        expires: started + TTL,
    })
}

#[cfg(test)]
#[path = "display_cache_tests.rs"]
mod tests;
