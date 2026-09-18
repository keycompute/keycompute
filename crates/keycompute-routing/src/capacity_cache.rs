//! Bounded advisory snapshot cache. No authorization, credentials or errors are
//! cached. Per-account async locks collapse concurrent refreshes; their owners
//! are the requesting futures, so cancellation cannot strand a refresh task.
use keycompute_config::gateway::RoutingCapacityConfig;
use keycompute_types::{AccountCapacityPolicy, AccountCapacitySnapshot, KeyComputeError, Result};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex as AsyncMutex, Semaphore},
    time::{Instant, timeout},
};
use uuid::Uuid;

#[derive(Debug)]
struct Entry {
    value: AsyncMutex<Option<(Instant, AccountCapacitySnapshot)>>,
    touched: AtomicU64,
}
#[derive(Debug)]
pub(crate) struct CapacityCache {
    config: RoutingCapacityConfig,
    entries: Mutex<HashMap<Uuid, Arc<Entry>>>,
    clock: AtomicU64,
    refreshes: Semaphore,
}
impl CapacityCache {
    pub fn new(config: RoutingCapacityConfig) -> Result<Self> {
        config
            .validate()
            .map_err(|m| KeyComputeError::ConfigError(m.into()))?;
        Ok(Self {
            refreshes: Semaphore::new(config.concurrency),
            config,
            entries: Mutex::new(HashMap::new()),
            clock: AtomicU64::new(0),
        })
    }
    pub fn cleared(&self) -> Self {
        Self::new(self.config.clone()).expect("previously validated snapshot configuration")
    }

    pub fn candidate_limit(&self) -> usize {
        self.config.candidate_limit
    }
    fn entry(&self, id: Uuid) -> Result<Arc<Entry>> {
        let mut entries = self.entries.lock().expect("capacity cache poisoned");
        let stamp = self.clock.fetch_add(1, Ordering::Relaxed);
        if let Some(entry) = entries.get(&id) {
            entry.touched.store(stamp, Ordering::Relaxed);
            return Ok(Arc::clone(entry));
        }
        if entries.len() == self.config.cache_entries {
            // In-use entries cannot be evicted: doing so would create duplicate
            // refreshes for one account and unbounded memory outside this map.
            let evict = entries
                .iter()
                .filter(|(_, e)| Arc::strong_count(e) == 1)
                .min_by_key(|(_, e)| e.touched.load(Ordering::Relaxed))
                .map(|(id, _)| *id);
            if let Some(id) = evict {
                entries.remove(&id);
            } else {
                return Err(KeyComputeError::ServiceUnavailable(
                    "routing snapshot cache is busy".into(),
                ));
            }
        }
        let entry = Arc::new(Entry {
            value: AsyncMutex::new(None),
            touched: AtomicU64::new(stamp),
        });
        entries.insert(id, Arc::clone(&entry));
        Ok(entry)
    }
    pub async fn snapshot(
        &self,
        policy: &dyn AccountCapacityPolicy,
        id: Uuid,
    ) -> Result<AccountCapacitySnapshot> {
        let entry = self.entry(id)?;
        timeout(Duration::from_millis(self.config.timeout_ms), async {
            let mut value = entry.value.lock().await;
            if let Some((created, snapshot)) = *value
                && created.elapsed() < Duration::from_millis(self.config.ttl_ms)
            {
                return Ok(snapshot);
            }
            // Never serve a stale value on refresh failure.
            *value = None;
            let _permit = self.refreshes.acquire().await.map_err(|_| {
                KeyComputeError::ServiceUnavailable("snapshot service closed".into())
            })?;
            let sampled_at = Instant::now();
            let snapshot = policy.snapshot(id).await?;
            *value = Some((sampled_at, snapshot));
            Ok(snapshot)
        })
        .await
        .map_err(|_| {
            KeyComputeError::ServiceUnavailable("routing snapshot deadline exceeded".into())
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    #[derive(Debug, Default)]
    struct Policy {
        calls: AtomicUsize,
        active: AtomicUsize,
        peak: AtomicUsize,
        fail: AtomicBool,
        delay: Duration,
    }
    struct Active<'a>(&'a AtomicUsize);
    impl Drop for Active<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    #[async_trait::async_trait]
    impl AccountCapacityPolicy for Policy {
        async fn snapshot(&self, _: Uuid) -> Result<AccountCapacitySnapshot> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let _guard = Active(&self.active);
            self.peak.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            if self.fail.load(Ordering::SeqCst) {
                Err(KeyComputeError::ServiceUnavailable(
                    "test unavailable".into(),
                ))
            } else {
                Ok(AccountCapacitySnapshot {
                    in_flight_limit: 32,
                    ..Default::default()
                })
            }
        }
        async fn admit(
            &self,
            _: &keycompute_types::RequestContext,
            _: &keycompute_types::ExecutionTarget,
        ) -> Result<Box<dyn keycompute_types::AccountAttemptLease>> {
            panic!("hint cannot authorize")
        }
    }
    fn cache() -> Arc<CapacityCache> {
        Arc::new(
            CapacityCache::new(RoutingCapacityConfig {
                concurrency: 2,
                ..Default::default()
            })
            .unwrap(),
        )
    }
    #[tokio::test(start_paused = true)]
    async fn snapshot_single_flight_and_ttl_share_work_but_not_stale_results() {
        let cache = cache();
        let policy = Arc::new(Policy {
            delay: Duration::from_millis(10),
            ..Default::default()
        });
        let id = Uuid::new_v4();
        let mut joins = tokio::task::JoinSet::new();
        for _ in 0..64 {
            let c = cache.clone();
            let p = policy.clone();
            joins.spawn(async move { c.snapshot(p.as_ref(), id).await });
        }
        while let Some(result) = joins.join_next().await {
            result.unwrap().unwrap();
        }
        assert_eq!(policy.calls.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_millis(101)).await;
        policy.fail.store(true, Ordering::SeqCst);
        assert!(cache.snapshot(policy.as_ref(), id).await.is_err());
        assert!(cache.snapshot(policy.as_ref(), id).await.is_err());
        assert_eq!(policy.calls.load(Ordering::SeqCst), 3);
        policy.fail.store(false, Ordering::SeqCst);
        cache.snapshot(policy.as_ref(), id).await.unwrap();
        assert_eq!(policy.calls.load(Ordering::SeqCst), 4);
    }
    #[tokio::test(start_paused = true)]
    async fn snapshot_refresh_concurrency_is_process_scoped() {
        let cache = cache();
        let policy = Arc::new(Policy {
            delay: Duration::from_millis(10),
            ..Default::default()
        });
        let mut joins = tokio::task::JoinSet::new();
        for _ in 0..32 {
            let c = cache.clone();
            let p = policy.clone();
            joins.spawn(async move { c.snapshot(p.as_ref(), Uuid::new_v4()).await });
        }
        while let Some(result) = joins.join_next().await {
            result.unwrap().unwrap();
        }
        assert_eq!(policy.peak.load(Ordering::SeqCst), 2);
        assert_eq!(policy.active.load(Ordering::SeqCst), 0);
        assert_eq!(cache.refreshes.available_permits(), 2);
    }
    #[tokio::test(start_paused = true)]
    async fn snapshot_cancellation_and_timeout_leave_refresh_reusable() {
        let cache = cache();
        let policy = Arc::new(Policy {
            delay: Duration::from_secs(10),
            ..Default::default()
        });
        let id = Uuid::new_v4();
        let c = cache.clone();
        let p = policy.clone();
        let task = tokio::spawn(async move { c.snapshot(p.as_ref(), id).await });
        while policy.active.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(policy.active.load(Ordering::SeqCst), 0);
        assert_eq!(cache.refreshes.available_permits(), 2);
        assert!(cache.snapshot(policy.as_ref(), id).await.is_err());
        assert_eq!(cache.refreshes.available_permits(), 2);
        cache.snapshot(&Policy::default(), id).await.unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn snapshot_cache_is_bounded_and_does_not_evict_inflight_owners() {
        let cache = CapacityCache::new(RoutingCapacityConfig {
            candidate_limit: 4,
            cache_entries: 4,
            ..Default::default()
        })
        .unwrap();
        let owned: Vec<_> = (0..4)
            .map(|_| cache.entry(Uuid::new_v4()).unwrap())
            .collect();
        assert!(cache.entry(Uuid::new_v4()).is_err());
        assert_eq!(cache.entries.lock().unwrap().len(), 4);
        drop(owned);
        for _ in 0..100 {
            cache
                .snapshot(&Policy::default(), Uuid::new_v4())
                .await
                .unwrap();
        }
        assert_eq!(cache.entries.lock().unwrap().len(), 4);
    }
}
