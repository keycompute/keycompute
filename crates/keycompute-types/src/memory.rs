//! Process-wide admission for retained payload working sets, not an allocator
//! replacement or an exact RSS cap. Reserve BEFORE copying/parsing and retain
//! the permit with every queued buffer. OS/runtime/DB overhead needs headroom.
use std::sync::{Arc, Mutex, OnceLock};

pub const DEFAULT_MANAGED_MEMORY_BYTES: usize = 512 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryStatus {
    pub limit: usize,
    pub used: usize,
    pub peak: usize,
}
#[derive(Debug)]
pub struct MemoryBudget {
    state: Mutex<MemoryStatus>,
}
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[error("process payload memory budget exhausted")]
pub struct MemoryExhausted;
#[derive(Debug)]
struct Claim {
    budget: Arc<MemoryBudget>,
    bytes: Mutex<usize>,
}
impl Drop for Claim {
    fn drop(&mut self) {
        let bytes = *self.bytes.get_mut().expect("memory claim poisoned");
        let mut state = self.budget.state.lock().expect("memory budget poisoned");
        state.used = state
            .used
            .checked_sub(bytes)
            .expect("memory permit accounting underflow");
    }
}
/// Clones share ownership of one reservation; cloning data requires a NEW
/// reservation or explicitly budgeted copy headroom, not just cloning a permit.
#[derive(Debug, Clone)]
pub struct MemoryPermit {
    claim: Arc<Claim>,
}
impl MemoryBudget {
    pub fn new(limit: usize) -> Result<Arc<Self>, &'static str> {
        if limit == 0 {
            return Err("managed memory limit must be positive");
        }
        Ok(Arc::new(Self {
            state: Mutex::new(MemoryStatus {
                limit,
                used: 0,
                peak: 0,
            }),
        }))
    }
    pub fn reserve(self: &Arc<Self>, bytes: usize) -> Result<MemoryPermit, MemoryExhausted> {
        let mut state = self.state.lock().expect("memory budget poisoned");
        if bytes > state.limit.saturating_sub(state.used) {
            return Err(MemoryExhausted);
        }
        state.used += bytes;
        state.peak = state.peak.max(state.used);
        Ok(MemoryPermit {
            claim: Arc::new(Claim {
                budget: Arc::clone(self),
                bytes: Mutex::new(bytes),
            }),
        })
    }
    pub fn status(&self) -> MemoryStatus {
        *self.state.lock().expect("memory budget poisoned")
    }
}
impl MemoryPermit {
    /// Grow atomically without waiting while holding a partial reservation.
    /// Waiting here would deadlock independent buffers each holding a fraction.
    pub fn grow_to(&self, bytes: usize) -> Result<(), MemoryExhausted> {
        let mut current = self.claim.bytes.lock().expect("memory claim poisoned");
        if bytes <= *current {
            return Ok(());
        }
        let delta = bytes - *current;
        let mut state = self
            .claim
            .budget
            .state
            .lock()
            .expect("memory budget poisoned");
        if delta > state.limit.saturating_sub(state.used) {
            return Err(MemoryExhausted);
        }
        state.used += delta;
        state.peak = state.peak.max(state.used);
        *current = bytes;
        Ok(())
    }
    /// Only the unique owner may shrink: other clones can still retain data.
    pub fn shrink_to(&mut self, bytes: usize) {
        if Arc::strong_count(&self.claim) != 1 {
            return;
        }
        let mut current = self.claim.bytes.lock().expect("memory claim poisoned");
        if bytes >= *current {
            return;
        }
        self.claim
            .budget
            .state
            .lock()
            .expect("memory budget poisoned")
            .used -= *current - bytes;
        *current = bytes;
    }
    pub fn bytes(&self) -> usize {
        *self.claim.bytes.lock().expect("memory claim poisoned")
    }
}
static PROCESS: OnceLock<Arc<MemoryBudget>> = OnceLock::new();
pub fn process_memory_budget() -> &'static Arc<MemoryBudget> {
    PROCESS.get_or_init(|| {
        MemoryBudget::new(DEFAULT_MANAGED_MEMORY_BYTES).expect("default memory limit")
    })
}
/// Set once during startup, before protocol components allocate. Multiple app
/// states in a process must agree; never change a live budget under its owners.
pub fn configure_process_memory_budget(bytes: usize) -> Result<(), &'static str> {
    if bytes == 0 {
        return Err("managed memory limit must be positive");
    }
    let configured =
        PROCESS.get_or_init(|| MemoryBudget::new(bytes).expect("validated positive budget"));
    if configured.status().limit != bytes {
        return Err("process memory budget already initialized with a different limit");
    }
    Ok(())
}
pub fn reserve_process_memory(bytes: usize) -> Result<MemoryPermit, MemoryExhausted> {
    process_memory_budget().reserve(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_payloads_share_one_budget_and_reject_before_growth() {
        let budget = MemoryBudget::new(1024).unwrap();
        let http = budget.reserve(400).unwrap();
        let upstream = budget.reserve(300).unwrap();
        let websocket = budget.reserve(300).unwrap();
        assert!(http.grow_to(500).is_err());
        assert!(budget.reserve(25).is_err());
        assert_eq!(budget.status().used, 1000);
        drop(upstream);
        http.grow_to(600).unwrap();
        drop((http, websocket));
        assert_eq!(budget.status().used, 0);
        assert!(budget.status().peak <= budget.status().limit);
    }
    #[test]
    fn cloned_owners_keep_reservations_until_the_last_buffer_leaves() {
        let budget = MemoryBudget::new(100).unwrap();
        let mut first = budget.reserve(80).unwrap();
        let second = first.clone();
        first.shrink_to(0);
        assert_eq!(budget.status().used, 80);
        drop(first);
        assert_eq!(budget.status().used, 80);
        drop(second);
        assert_eq!(budget.status().used, 0);
    }
    #[test]
    fn concurrent_claims_never_exceed_global_bytes() {
        let budget = MemoryBudget::new(2048).unwrap();
        std::thread::scope(|s| {
            for _ in 0..16 {
                let b = budget.clone();
                s.spawn(move || {
                    for _ in 0..1000 {
                        if let Ok(p) = b.reserve(128) {
                            let _ = p.grow_to(512);
                            assert!(b.status().used <= 2048);
                            drop(p);
                        }
                    }
                });
            }
        });
        assert_eq!(budget.status().used, 0);
    }
}
