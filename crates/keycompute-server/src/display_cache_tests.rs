use super::*;
use keycompute_runtime::admission::AdmissionLimits;
use std::sync::atomic::AtomicUsize;
use tokio::{sync::Notify, task::JoinSet};
fn gate(total: usize) -> Arc<BoundedAdmission> {
    BoundedAdmission::new(AdmissionLimits {
        total,
        per_key: total,
        queue: 64,
        queue_per_key: 64,
        wait: Duration::from_millis(100),
    })
    .unwrap()
}
fn l2() -> Arc<CacheService> {
    Arc::new(CacheService::disabled())
}
#[tokio::test]
async fn concurrent_reads_share_one_origin_and_errors_are_not_cached() {
    let c = DisplayCache::default();
    let g = gate(2);
    let calls = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut jobs = JoinSet::new();
    for _ in 0..16 {
        let (c, g, calls, started, release) = (
            c.clone(),
            g.clone(),
            calls.clone(),
            started.clone(),
            release.clone(),
        );
        jobs.spawn(async move {
            c.read(l2(), g, Uuid::nil(), "same".into(), async move {
                calls.fetch_add(1, Ordering::SeqCst);
                started.notify_one();
                release.notified().await;
                Ok(serde_json::json!({"value":7}))
            })
            .await
        });
    }
    started.notified().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while c.metrics()["coalesced"] != 15 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    release.notify_one();
    while let Some(r) = jobs.join_next().await {
        assert_eq!(r.unwrap().unwrap()["value"], 7);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let cached = c
        .read(l2(), g.clone(), Uuid::nil(), "same".into(), async {
            panic!("must be cached")
        })
        .await
        .unwrap();
    assert_eq!(cached["value"], 7);
    assert_eq!(g.status().active, 0);
    for _ in 0..2 {
        assert!(matches!(
            c.read(l2(), g.clone(), Uuid::nil(), "denied".into(), async {
                Err(ApiError::Forbidden("denied".into()))
            })
            .await,
            Err(ApiError::Forbidden(_))
        ));
    }
    assert_eq!(c.metrics()["origin"], 3);
}
#[tokio::test]
async fn delayed_completed_waiter_and_midflight_refill_are_fenced() {
    let c = DisplayCache::default();
    let g = gate(2);
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let owner = {
        let (c, g, started, release) = (c.clone(), g.clone(), started.clone(), release.clone());
        tokio::spawn(async move {
            c.read(l2(), g, Uuid::nil(), "same".into(), async move {
                started.notify_one();
                release.notified().await;
                Ok(serde_json::json!({"old":true}))
            })
            .await
        })
    };
    started.notified().await;
    let waiter = c.read(l2(), g.clone(), Uuid::nil(), "same".into(), async {
        panic!("must join")
    });
    tokio::pin!(waiter);
    assert!(futures::poll!(&mut waiter).is_pending());
    release.notify_one();
    assert!(owner.await.unwrap().is_ok());
    c.invalidate();
    assert!(waiter.await.is_err());
    assert_eq!(c.metrics()["entries"], 0);
    let pending = c.read(l2(), g.clone(), Uuid::nil(), "late".into(), async {
        tokio::task::yield_now().await;
        Ok(serde_json::json!({"old":true}))
    });
    tokio::pin!(pending);
    assert!(futures::poll!(&mut pending).is_pending());
    c.invalidate();
    assert!(pending.await.is_err());
    assert_eq!(c.metrics()["entries"], 0);
    assert_eq!(g.status().active, 0);
}
#[tokio::test]
async fn cancellation_releases_origin_and_last_flight_without_a_cycle() {
    let c = DisplayCache::default();
    let g = gate(1);
    let future = c.read(
        l2(),
        g.clone(),
        Uuid::nil(),
        "cancel".into(),
        futures::future::pending(),
    );
    let mut pinned = Box::pin(future);
    assert!(futures::poll!(pinned.as_mut()).is_pending());
    assert_eq!(g.status().active, 1);
    drop(pinned);
    assert_eq!(g.status().active, 0);
    assert_eq!(c.metrics()["active_flights"], 0);
    assert!(
        c.read(l2(), g, Uuid::nil(), "cancel".into(), async {
            Ok(serde_json::json!({"new":true}))
        })
        .await
        .is_ok()
    );
}
#[tokio::test]
async fn expiry_bytes_entries_and_identity_are_bounded() {
    let c = DisplayCache::default();
    let g = gate(2);
    for i in 0..150 {
        c.read(l2(), g.clone(), Uuid::nil(), format!("k{i}"), async {
            Ok(serde_json::json!({"v":"x".repeat(40000)}))
        })
        .await
        .unwrap();
    }
    assert!(c.metrics()["entries"].as_u64().unwrap() <= 128);
    assert!(c.metrics()["serialized_bytes"].as_u64().unwrap() <= MAX_BYTES as u64);
    c.read(l2(), g.clone(), Uuid::nil(), "huge".into(), async {
        Ok(serde_json::json!({"v":"x".repeat(MAX_VALUE_BYTES+1)}))
    })
    .await
    .unwrap();
    assert!(!c.state.lock().unwrap().entries.contains_key("huge"));
    c.read(l2(), g.clone(), Uuid::nil(), "expire".into(), async {
        Ok(serde_json::json!({"v":1}))
    })
    .await
    .unwrap();
    c.state
        .lock()
        .unwrap()
        .entries
        .get_mut("expire")
        .unwrap()
        .snapshot
        .expires = Instant::now() - Duration::from_secs(1);
    assert_eq!(
        c.read(l2(), g, Uuid::nil(), "expire".into(), async {
            Ok(serde_json::json!({"v":2}))
        })
        .await
        .unwrap()["v"],
        2
    );
    let id = Uuid::new_v4();
    let mut a = AuthExtractor::new(id, id, Uuid::nil(), "user");
    let k = DisplayCache::key(&a, "summary", "q");
    a.user_id = Uuid::new_v4();
    assert_ne!(k, DisplayCache::key(&a, "summary", "q"));
    a.user_id = id;
    a.permissions.push(keycompute_auth::Permission::SystemAdmin);
    assert_ne!(k, DisplayCache::key(&a, "summary", "q"));
}
#[tokio::test]
#[cfg(feature = "redis")]
async fn optional_redis_failure_does_not_bypass_the_origin_gate() {
    let pool = deadpool_redis::Config::from_url("redis://127.0.0.1:1")
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .unwrap();
    let broken = Arc::new(CacheService::with_pool(pool));
    let c = DisplayCache::default();
    let g = gate(1);
    let held = g.acquire(Uuid::nil()).await.unwrap();
    let ran = Arc::new(AtomicUsize::new(0));
    let copy = ran.clone();
    let result = c
        .read(
            broken,
            g.clone(),
            Uuid::new_v4(),
            "outage".into(),
            async move {
                copy.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!({}))
            },
        )
        .await;
    assert!(matches!(result, Err(ApiError::ServiceUnavailable(_))));
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    // CacheService intentionally converts pool-unavailable into a miss.
    // The regression concerns preserved origin admission, not that internal metric.
    assert_eq!(c.metrics()["l2_hit"], 0);
    assert_eq!(c.metrics()["origin"], 0);
    drop(held);
    assert_eq!(g.status().active, 0);
    assert_eq!(g.status().queued, 0);
}

#[tokio::test]
async fn invalidated_live_work_still_counts_against_the_flight_limit() {
    let cache = DisplayCache::default();
    let origin = gate(MAX_FLIGHTS);
    let mut pending = Vec::new();
    for index in 0..MAX_FLIGHTS {
        let mut request = Box::pin(cache.read(
            l2(),
            origin.clone(),
            Uuid::nil(),
            format!("live-{index}"),
            futures::future::pending(),
        ));
        assert!(futures::poll!(request.as_mut()).is_pending());
        pending.push(request);
    }
    assert_eq!(origin.status().active, MAX_FLIGHTS);
    cache.invalidate();
    assert!(
        cache
            .read(
                l2(),
                origin.clone(),
                Uuid::nil(),
                "overflow".into(),
                async { panic!("capacity rejection must occur before query execution") }
            )
            .await
            .is_err()
    );
    drop(pending);
    assert_eq!(origin.status().active, 0);
    assert!(
        cache
            .read(l2(), origin, Uuid::nil(), "after-drop".into(), async {
                Ok(serde_json::json!({"ok": true}))
            })
            .await
            .is_ok()
    );
}
