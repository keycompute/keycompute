//! Process-local resource admission, independent of billable RPM/TPM.
//!
//! One lock commits total and tenant/account capacity together. Waiting for a
//! busy key never holds a total execution slot. Both the queue and key map are
//! bounded; cancellation removes its ticket synchronously, without spawning.
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::Notify,
    time::{Instant, timeout_at},
};
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub struct AdmissionLimits {
    pub total: usize,
    pub per_key: usize,
    pub queue: usize,
    pub queue_per_key: usize,
    pub wait: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    #[error("resource admission is draining")]
    Closed,
    #[error("resource admission queue is full")]
    Full,
    #[error("resource admission deadline exceeded")]
    Timeout,
}

#[derive(Debug, Default)]
struct Counts {
    active: usize,
    queued: usize,
}
#[derive(Debug, Default)]
struct State {
    closed: bool,
    active: usize,
    keys: HashMap<Uuid, Counts>,
    queue: VecDeque<(Uuid, Uuid)>, // ticket, scope
}

#[derive(Debug)]
pub struct BoundedAdmission {
    limits: AdmissionLimits,
    state: Mutex<State>,
    changed: Notify,
}

/// Clones share a single slot. The last owner, including a response body or
/// detached settlement worker, releases it. No async cleanup can be lost.
#[derive(Debug, Clone)]
pub struct AdmissionPermit {
    _lease: Arc<Lease>,
}
#[derive(Debug)]
struct Lease {
    owner: Arc<BoundedAdmission>,
    key: Uuid,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.owner.state.lock().expect("admission lock poisoned");
        state.active -= 1;
        let counts = state
            .keys
            .get_mut(&self.key)
            .expect("admission key missing");
        counts.active -= 1;
        if counts.active == 0 && counts.queued == 0 {
            state.keys.remove(&self.key);
        }
        drop(state);
        self.owner.changed.notify_waiters();
    }
}

struct Ticket {
    owner: Arc<BoundedAdmission>,
    id: Uuid,
    queued: bool,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if !self.queued {
            return;
        }
        let mut state = self.owner.state.lock().expect("admission lock poisoned");
        if let Some(pos) = state.queue.iter().position(|(id, _)| *id == self.id) {
            let (_, key) = state.queue.remove(pos).expect("queued ticket missing");
            let counts = state.keys.get_mut(&key).expect("queued key missing");
            counts.queued -= 1;
            if counts.active == 0 && counts.queued == 0 {
                state.keys.remove(&key);
            }
        }
        drop(state);
        self.owner.changed.notify_waiters();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionStatus {
    pub active: usize,
    pub queued: usize,
    pub keys: usize,
}

impl BoundedAdmission {
    pub fn new(limits: AdmissionLimits) -> Result<Arc<Self>, &'static str> {
        if limits.total == 0
            || limits.per_key == 0
            || limits.total > 65_536
            || limits.per_key > limits.total
            || limits.queue > 65_536
            || limits.queue_per_key > limits.queue
            || limits.wait.is_zero()
            || limits.wait > Duration::from_secs(60)
        {
            return Err("invalid resource admission capacity or deadline");
        }
        Ok(Arc::new(Self {
            limits,
            state: Mutex::new(State::default()),
            changed: Notify::new(),
        }))
    }

    fn eligible(&self, state: &State, key: Uuid) -> bool {
        state.active < self.limits.total
            && state
                .keys
                .get(&key)
                .is_none_or(|c| c.active < self.limits.per_key)
    }

    fn grant(self: &Arc<Self>, state: &mut State, key: Uuid) -> AdmissionPermit {
        state.active += 1;
        state.keys.entry(key).or_default().active += 1;
        AdmissionPermit {
            _lease: Arc::new(Lease {
                owner: Arc::clone(self),
                key,
            }),
        }
    }

    /// Fair among currently eligible tickets: a saturated tenant/account does
    /// not head-of-line block unrelated keys. New callers cannot bypass an
    /// eligible queued request. The queue bound includes every waiting future.
    pub async fn acquire(self: &Arc<Self>, key: Uuid) -> Result<AdmissionPermit, AdmissionError> {
        let deadline = Instant::now() + self.limits.wait;
        let mut ticket = Ticket {
            owner: Arc::clone(self),
            id: Uuid::new_v4(),
            queued: false,
        };
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable(); // register before inspecting shared state
            {
                let mut state = self.state.lock().expect("admission lock poisoned");
                if state.closed {
                    return Err(AdmissionError::Closed);
                }
                let eligible_ticket = state
                    .queue
                    .iter()
                    .find(|(_, scope)| self.eligible(&state, *scope))
                    .map(|(id, _)| *id);
                if self.eligible(&state, key)
                    && if ticket.queued {
                        eligible_ticket == Some(ticket.id)
                    } else {
                        eligible_ticket.is_none()
                    }
                {
                    if ticket.queued {
                        if Instant::now() >= deadline {
                            return Err(AdmissionError::Timeout);
                        }
                        let pos = state
                            .queue
                            .iter()
                            .position(|(id, _)| *id == ticket.id)
                            .unwrap();
                        state.queue.remove(pos);
                        state.keys.get_mut(&key).unwrap().queued -= 1;
                        ticket.queued = false;
                    }
                    let permit = self.grant(&mut state, key);
                    drop(state);
                    self.changed.notify_waiters();
                    return Ok(permit);
                }
                if !ticket.queued {
                    if state.queue.len() >= self.limits.queue
                        || state.keys.get(&key).map_or(0, |c| c.queued) >= self.limits.queue_per_key
                    {
                        return Err(AdmissionError::Full);
                    }
                    state.queue.push_back((ticket.id, key));
                    state.keys.entry(key).or_default().queued += 1;
                    ticket.queued = true;
                }
            }
            timeout_at(deadline, &mut changed)
                .await
                .map_err(|_| AdmissionError::Timeout)?;
        }
    }

    /// Close new grants and wake queued tickets without revoking active work.
    pub fn close(&self) {
        self.state.lock().expect("admission lock poisoned").closed = true;
        self.changed.notify_waiters();
    }

    pub fn status(&self) -> AdmissionStatus {
        let state = self.state.lock().expect("admission lock poisoned");
        AdmissionStatus {
            active: state.active,
            queued: state.queue.len(),
            keys: state.keys.len(),
        }
    }

    pub fn active_for(&self, key: Uuid) -> usize {
        self.state
            .lock()
            .expect("admission lock poisoned")
            .keys
            .get(&key)
            .map_or(0, |c| c.active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limiter(total: usize, per_key: usize, queue: usize) -> Arc<BoundedAdmission> {
        BoundedAdmission::new(AdmissionLimits {
            total,
            per_key,
            queue,
            queue_per_key: queue,
            wait: Duration::from_millis(100),
        })
        .unwrap()
    }
    async fn queued(limiter: &BoundedAdmission, count: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while limiter.status().queued != count {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn admission_global_and_key_limits_commit_atomically() {
        let pool = limiter(2, 1, 0);
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let first = pool.acquire(a).await.unwrap();
        assert_eq!(pool.acquire(a).await.unwrap_err(), AdmissionError::Full);
        let second = pool.acquire(b).await.unwrap();
        assert_eq!(
            pool.acquire(Uuid::new_v4()).await.unwrap_err(),
            AdmissionError::Full
        );
        drop((first, second));
        assert_eq!(
            pool.status(),
            AdmissionStatus {
                active: 0,
                queued: 0,
                keys: 0
            }
        );
    }
    #[tokio::test]
    async fn admission_queue_bound_timeout_and_cancellation_release_all_state() {
        let pool = limiter(1, 1, 1);
        let key = Uuid::new_v4();
        let active = pool.acquire(key).await.unwrap();
        let copy = pool.clone();
        let waiter = tokio::spawn(async move { copy.acquire(key).await });
        queued(&pool, 1).await;
        assert_eq!(
            pool.acquire(Uuid::new_v4()).await.unwrap_err(),
            AdmissionError::Full
        );
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert_eq!(pool.status().queued, 0);
        assert_eq!(
            pool.acquire(key).await.unwrap_err(),
            AdmissionError::Timeout
        );
        assert_eq!(pool.status().queued, 0);
        drop(active);
        assert_eq!(pool.status().keys, 0);
    }
    #[tokio::test]
    async fn admission_hot_key_does_not_hold_total_capacity_while_queued() {
        let pool = limiter(2, 1, 2);
        let a = Uuid::new_v4();
        let held = pool.acquire(a).await.unwrap();
        let copy = pool.clone();
        let waiter = tokio::spawn(async move { copy.acquire(a).await });
        queued(&pool, 1).await;
        let other = pool.acquire(Uuid::new_v4()).await.unwrap();
        assert_eq!(pool.status().active, 2);
        drop(held);
        let next = waiter.await.unwrap().unwrap();
        drop((other, next));
        assert_eq!(pool.status().keys, 0);
    }
    #[tokio::test]
    async fn admission_clone_retains_slot_until_last_owner_drops() {
        let pool = limiter(1, 1, 0);
        let permit = pool.acquire(Uuid::new_v4()).await.unwrap();
        let clone = permit.clone();
        drop(permit);
        assert_eq!(pool.status().active, 1);
        drop(clone);
        assert_eq!(pool.status().active, 0);
    }
    #[tokio::test]
    async fn admission_burst_does_not_leak_scoped_maps() {
        let pool = limiter(4, 2, 64);
        let mut tasks = Vec::new();
        for i in 0..64 {
            let pool = pool.clone();
            tasks.push(tokio::spawn(async move {
                let permit = pool.acquire(Uuid::from_u128((i % 8) + 1)).await.unwrap();
                assert!(pool.status().active <= 4);
                tokio::task::yield_now().await;
                drop(permit);
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(
            pool.status(),
            AdmissionStatus {
                active: 0,
                queued: 0,
                keys: 0
            }
        );
    }
}

#[cfg(test)]
mod drain_tests {
    use super::*;
    #[tokio::test]
    async fn close_wakes_tickets_without_revoking_paid_work() {
        let pool = BoundedAdmission::new(AdmissionLimits {
            total: 1,
            per_key: 1,
            queue: 2,
            queue_per_key: 2,
            wait: Duration::from_secs(5),
        })
        .unwrap();
        let key = Uuid::new_v4();
        let permit = pool.acquire(key).await.unwrap();
        let copy = pool.clone();
        let task = tokio::spawn(async move { copy.acquire(key).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while pool.status().queued != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        pool.close();
        pool.close();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err(),
            AdmissionError::Closed
        );
        assert_eq!(
            pool.acquire(Uuid::new_v4()).await.unwrap_err(),
            AdmissionError::Closed
        );
        assert_eq!(pool.status().active, 1);
        assert_eq!(pool.status().queued, 0);
        drop(permit);
        assert_eq!(pool.status().keys, 0);
    }
}
