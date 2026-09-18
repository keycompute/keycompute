//! Shared account-only quotas, isolated from every caller's tenant/API-key bucket.
//!
//! Reuse the existing atomic reservation scripts, terminal fencing and bounded
//! expiry cleanup rather than introducing a second non-atomic counter protocol.
use crate::{MemoryRateLimiter, RateLimitBackend, RateLimitConfig, RateLimitKey, RateLimitService};
use keycompute_observability::account_leases::{self, LeaseEvent, LeaseOwner};
use keycompute_types::{AccountAttemptLease, AccountCapacitySnapshot, KeyComputeError, Result};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

const IO_BUDGET: Duration = Duration::from_secs(5);
const RENEW_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub struct AccountQuotaService {
    quota: Arc<RateLimitService>,
    slots: Arc<RateLimitService>,
    in_flight_limit: u32,
    #[cfg(feature = "redis")]
    redis_readers: Option<(crate::RedisRateLimiter, crate::RedisRateLimiter)>,
}

impl AccountQuotaService {
    fn validate(limit: u32, window: Duration) -> Result<()> {
        if limit == 0
            || limit > 65_536
            || window.as_secs() < 60
            || window.as_secs() > 86_400
            || window.subsec_nanos() != 0
        {
            return Err(KeyComputeError::ConfigError(
                "invalid account in-flight lease budget".into(),
            ));
        }
        Ok(())
    }

    pub fn memory(in_flight_limit: u32, lease_window: Duration) -> Result<Arc<Self>> {
        Self::validate(in_flight_limit, lease_window)?;
        let mut slots = MemoryRateLimiter::new();
        slots.window_size = lease_window;
        Ok(Arc::new(Self {
            #[cfg(feature = "redis")]
            redis_readers: None,
            quota: Arc::new(RateLimitService::default_memory()),
            slots: Arc::new(RateLimitService::new(
                Arc::new(slots),
                RateLimitBackend::Memory,
            )),
            in_flight_limit,
        }))
    }

    #[cfg(feature = "redis")]
    pub fn redis(
        pool: deadpool_redis::Pool,
        in_flight_limit: u32,
        lease_window: Duration,
    ) -> Result<Arc<Self>> {
        Self::validate(in_flight_limit, lease_window)?;
        let quota = RateLimitService::with_redis_pool_and_prefix(
            pool.clone(),
            "keycompute:account-quota:v1",
        );
        let quota_reader =
            crate::RedisRateLimiter::with_prefix(pool.clone(), "keycompute:account-quota:v1");
        let slots = crate::RedisRateLimiter::with_prefix(pool, "keycompute:account-inflight:v1")
            .with_reservation_window(lease_window);
        Ok(Arc::new(Self {
            redis_readers: Some((quota_reader, slots.clone())),
            quota: Arc::new(quota),
            slots: Arc::new(RateLimitService::new(
                Arc::new(slots),
                RateLimitBackend::Redis,
            )),
            in_flight_limit,
        }))
    }

    fn key(account_id: Uuid) -> RateLimitKey {
        // The fixed namespace plus account ID aggregates ALL visible tenants,
        // users and API keys sharing this one upstream account.
        RateLimitKey::new(Uuid::nil(), Uuid::nil(), account_id)
    }

    pub async fn snapshot(&self, account_id: Uuid) -> Result<AccountCapacitySnapshot> {
        let key = Self::key(account_id);
        #[cfg(feature = "redis")]
        if let Some((quota, slots)) = &self.redis_readers {
            return tokio::time::timeout(
                IO_BUDGET,
                quota.account_snapshot(slots, &key, self.in_flight_limit),
            )
            .await
            .map_err(|_| {
                KeyComputeError::ServiceUnavailable("account snapshot timed out".into())
            })?;
        }
        let (rpm, tpm, in_flight) = tokio::time::timeout(IO_BUDGET, async {
            tokio::try_join!(
                self.quota.get_rpm_count(&key),
                self.quota.get_tpm_count(&key),
                self.slots.get_tpm_count(&key)
            )
        })
        .await
        .map_err(|_| {
            KeyComputeError::ServiceUnavailable("account quota snapshot timed out".into())
        })??;
        Ok(AccountCapacitySnapshot {
            rpm,
            tpm,
            in_flight,
            in_flight_limit: self.in_flight_limit,
        })
    }

    pub async fn admit(
        self: &Arc<Self>,
        account_id: Uuid,
        predicted_tokens: u32,
        limits: RateLimitConfig,
    ) -> Result<AccountQuotaLease> {
        let mut lease = AccountQuotaLease {
            service: Arc::clone(self),
            key: Self::key(account_id),
            attempt_id: Uuid::new_v4(),
            predicted_tokens,
            finished: false,
            telemetry: None,
        };
        // Distinct attempt IDs count every retry/fallback against the account.
        // Reservations precede RPM debit; a rejected stage never contacts upstream.
        let result = tokio::time::timeout(IO_BUDGET, async {
            self.slots
                .reserve_token_usage(
                    &lease.key,
                    lease.attempt_id,
                    lease.attempt_id,
                    1,
                    &RateLimitConfig::new(u32::MAX, self.in_flight_limit),
                )
                .await?;
            self.quota
                .reserve_token_usage(
                    &lease.key,
                    lease.attempt_id,
                    lease.attempt_id,
                    predicted_tokens,
                    &limits,
                )
                .await?;
            self.quota
                .check_and_record_with_config(&lease.key, &limits)
                .await
        })
        .await
        .map_err(|_| {
            KeyComputeError::ServiceUnavailable("account quota admission timed out".into())
        })
        .and_then(|result| result);
        if let Err(error) = result {
            account_leases::record(if matches!(error, KeyComputeError::RateLimitExceeded(_)) {
                LeaseEvent::Rejected
            } else {
                LeaseEvent::AdmissionError
            });
            // Safe before dispatch. An ambiguous late backend write can retain
            // capacity until expiry, but cannot permit an unmetered dispatch.
            lease.release_unstarted().await;
            return Err(error);
        }
        lease.telemetry = Some(LeaseOwner::admitted());
        Ok(lease)
    }
}

/// No detached heartbeat owns this lease. Executor cancellation drops its
/// keep-alive future; remaining distributed reservations expire conservatively.
#[derive(Debug)]
pub struct AccountQuotaLease {
    service: Arc<AccountQuotaService>,
    key: RateLimitKey,
    attempt_id: Uuid,
    predicted_tokens: u32,
    finished: bool,
    telemetry: Option<LeaseOwner>,
}
impl AccountQuotaLease {
    async fn release_unstarted(&mut self) {
        let _ = tokio::time::timeout(IO_BUDGET, async {
            let (tokens, slots) = tokio::join!(
                self.service
                    .quota
                    .release_token_reservation(&self.key, self.attempt_id),
                self.service
                    .slots
                    .release_token_reservation(&self.key, self.attempt_id)
            );
            tokens.and(slots)
        })
        .await;
        self.finished = true;
    }
}
#[async_trait::async_trait]
impl AccountAttemptLease for AccountQuotaLease {
    async fn keep_alive(&self) -> Result<()> {
        loop {
            tokio::time::sleep(RENEW_INTERVAL).await;
            let (tokens_live, slot_live) = tokio::time::timeout(IO_BUDGET, async {
                tokio::try_join!(
                    self.service.quota.renew_token_reservation(
                        &self.key,
                        self.attempt_id,
                        self.attempt_id,
                        self.predicted_tokens
                    ),
                    self.service.slots.renew_token_reservation(
                        &self.key,
                        self.attempt_id,
                        self.attempt_id,
                        1
                    ),
                )
            })
            .await
            .map_err(|_| {
                KeyComputeError::ServiceUnavailable("account quota renewal timed out".into())
            })
            .and_then(|result| result)
            .inspect_err(|_| account_leases::record(LeaseEvent::RenewalLost))?;
            if !tokens_live || !slot_live {
                account_leases::record(LeaseEvent::RenewalLost);
                return Err(KeyComputeError::ServiceUnavailable(
                    "account quota lease was lost".into(),
                ));
            }
        }
    }

    async fn finish(&mut self, exact_tokens: Option<u32>, release_in_flight: bool) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        let result = tokio::time::timeout(IO_BUDGET, async {
            // Completion-time accounting conservatively keeps long-running work
            // in the next minute; unknown usage is never silently charged zero.
            self.service
                .quota
                .reconcile_token_usage_now(
                    &self.key,
                    self.attempt_id,
                    self.attempt_id,
                    exact_tokens.unwrap_or(self.predicted_tokens),
                )
                .await?;
            if release_in_flight {
                self.service
                    .slots
                    .release_token_reservation(&self.key, self.attempt_id)
                    .await?;
            }
            Ok(())
        })
        .await
        .map_err(|_| {
            KeyComputeError::ServiceUnavailable("account quota settlement timed out".into())
        })
        .and_then(|result| result);
        if result.is_ok() {
            self.finished = true;
            if let Some(telemetry) = self.telemetry.as_mut() {
                telemetry.finish(release_in_flight);
            }
        } else {
            account_leases::record(LeaseEvent::SettlementError);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn exercise_shared_quotas(a: Arc<AccountQuotaService>, b: Arc<AccountQuotaService>) {
        let id = Uuid::new_v4();
        let config = RateLimitConfig::new(10, 100);
        let mut first = a.admit(id, 70, config.clone()).await.unwrap();
        assert!(b.admit(id, 40, config.clone()).await.is_err());
        assert_eq!(b.snapshot(id).await.unwrap().tpm, 70);
        assert_eq!(b.snapshot(id).await.unwrap().in_flight, 1);
        first.finish(Some(20), true).await.unwrap();
        first.finish(Some(20), true).await.unwrap();
        assert_eq!(b.snapshot(id).await.unwrap().tpm, 20);
        let mut second = b.admit(id, 80, config.clone()).await.unwrap();
        second.finish(Some(80), true).await.unwrap();
        assert!(a.admit(id, 1, config).await.is_err());
        let rpm_id = Uuid::new_v4();
        for _ in 0..2 {
            let mut lease = a
                .admit(rpm_id, 1, RateLimitConfig::new(2, 100))
                .await
                .unwrap();
            lease.finish(Some(1), true).await.unwrap();
        }
        assert!(
            b.admit(rpm_id, 1, RateLimitConfig::new(2, 100))
                .await
                .is_err()
        );
        assert_eq!(a.snapshot(rpm_id).await.unwrap().in_flight, 0);
    }
    #[tokio::test]
    async fn account_quota_memory_shares_across_callers() {
        let service = AccountQuotaService::memory(2, Duration::from_secs(120)).unwrap();
        exercise_shared_quotas(service.clone(), service).await;
    }
    #[tokio::test]
    async fn account_quota_unknown_usage_stays_charged_and_attempts_are_distinct() {
        let s = AccountQuotaService::memory(1, Duration::from_secs(120)).unwrap();
        let id = Uuid::new_v4();
        let mut lease = s
            .admit(id, 50, RateLimitConfig::new(100, 100))
            .await
            .unwrap();
        assert!(
            s.admit(id, 1, RateLimitConfig::new(100, 100))
                .await
                .is_err()
        );
        lease.finish(None, true).await.unwrap();
        assert_eq!(s.snapshot(id).await.unwrap().tpm, 50);
        let mut next = s
            .admit(id, 50, RateLimitConfig::new(100, 100))
            .await
            .unwrap();
        next.finish(Some(10), true).await.unwrap();
        assert_eq!(s.snapshot(id).await.unwrap().tpm, 60);
    }
    #[tokio::test]
    async fn account_quota_concurrent_callers_cannot_overbook_slots() {
        let s = AccountQuotaService::memory(3, Duration::from_secs(120)).unwrap();
        let id = Uuid::new_v4();
        let mut tasks = Vec::new();
        for _ in 0..30 {
            let s = s.clone();
            tasks.push(tokio::spawn(async move {
                s.admit(id, 1, RateLimitConfig::new(100, 1000)).await
            }));
        }
        let mut leases = Vec::new();
        for task in tasks {
            if let Ok(lease) = task.await.unwrap() {
                leases.push(lease);
            }
        }
        assert_eq!(leases.len(), 3);
        assert_eq!(s.snapshot(id).await.unwrap().in_flight, 3);
        for mut lease in leases {
            lease.finish(Some(1), true).await.unwrap();
        }
        assert_eq!(s.snapshot(id).await.unwrap().in_flight, 0);
    }
    #[cfg(feature = "redis")]
    #[tokio::test]
    async fn account_quota_two_redis_instances_share_account_capacity() {
        let url = std::env::var("REDIS_URL").or_else(|_| std::env::var("KC__REDIS__URL"));
        let url = match url {
            Ok(url) => url,
            Err(_) if std::env::var_os("CI").is_none() => return,
            Err(_) => "redis://127.0.0.1:6379".into(),
        };
        let pool = deadpool_redis::Config::from_url(url)
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .unwrap();
        let a = AccountQuotaService::redis(pool.clone(), 2, Duration::from_secs(120)).unwrap();
        let b = AccountQuotaService::redis(pool, 2, Duration::from_secs(120)).unwrap();
        exercise_shared_quotas(a, b).await;
    }
    #[tokio::test(start_paused = true)]
    async fn account_quota_lost_reservation_never_resurrects_on_renewal() {
        let service = AccountQuotaService::memory(1, Duration::from_secs(120)).unwrap();
        let lease = service
            .admit(Uuid::new_v4(), 50, RateLimitConfig::new(10, 100))
            .await
            .unwrap();
        service
            .quota
            .release_token_reservation(&lease.key, lease.attempt_id)
            .await
            .unwrap();
        assert!(lease.keep_alive().await.is_err());
        assert_eq!(service.quota.get_tpm_count(&lease.key).await.unwrap(), 0);
    }

    #[cfg(feature = "redis")]
    #[tokio::test]
    async fn account_quota_distributed_slot_admission_is_atomic_under_burst() {
        let url = std::env::var("REDIS_URL").or_else(|_| std::env::var("KC__REDIS__URL"));
        let url = match url {
            Ok(url) => url,
            Err(_) if std::env::var_os("CI").is_none() => return,
            Err(_) => "redis://127.0.0.1:6379".into(),
        };
        let pool = deadpool_redis::Config::from_url(url)
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .unwrap();
        let a = AccountQuotaService::redis(pool.clone(), 3, Duration::from_secs(120)).unwrap();
        let b = AccountQuotaService::redis(pool, 3, Duration::from_secs(120)).unwrap();
        let id = Uuid::new_v4();
        let mut tasks = Vec::new();
        for i in 0..24 {
            let service = if i % 2 == 0 { a.clone() } else { b.clone() };
            tasks.push(tokio::spawn(async move {
                service.admit(id, 1, RateLimitConfig::new(100, 1000)).await
            }));
        }
        let mut leases = Vec::new();
        for task in tasks {
            if let Ok(lease) = task.await.unwrap() {
                leases.push(lease);
            }
        }
        assert_eq!(leases.len(), 3);
        assert_eq!(a.snapshot(id).await.unwrap().in_flight, 3);
        for mut lease in leases {
            lease.finish(Some(1), true).await.unwrap();
        }
        assert_eq!(b.snapshot(id).await.unwrap().in_flight, 0);
    }
    #[tokio::test(start_paused = true)]
    async fn account_quota_missing_shared_slot_also_stops_execution() {
        let service = AccountQuotaService::memory(1, Duration::from_secs(120)).unwrap();
        let lease = service
            .admit(Uuid::new_v4(), 50, RateLimitConfig::new(10, 100))
            .await
            .unwrap();
        service
            .slots
            .release_token_reservation(&lease.key, lease.attempt_id)
            .await
            .unwrap();
        assert!(lease.keep_alive().await.is_err());
        assert_eq!(service.slots.get_tpm_count(&lease.key).await.unwrap(), 0);
    }
}
