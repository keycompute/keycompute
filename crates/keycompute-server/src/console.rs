//! Independent console ingress, resource admission, and abuse budgets.
//!
//! Console requests are deliberately separate from billable generation RPM/TPM.
//! The first gate is process-local and runs before authentication/maintenance
//! database work. Identity/tenant/aggregate counters are then recorded in a
//! namespaced memory or Redis limiter after one authoritative token lookup.

use crate::{
    ApiError,
    extractors::{AuthExtractor, GlobalConsoleAuth},
    state::AppState,
};
use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use keycompute_config::ConsoleConfig;
use keycompute_ratelimit::{RateLimitConfig, RateLimitKey};
use keycompute_runtime::admission::{
    AdmissionError, AdmissionLimits, AdmissionPermit, BoundedAdmission,
};
use keycompute_types::console::ConsoleClass;
use sha2::{Digest, Sha256};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

/// Server-only proof of applied console ingress and policy.
#[derive(Clone, Copy)]
pub(crate) struct ConsoleAdmissionChecked;

#[derive(Debug)]
pub struct ConsoleAdmission {
    pub read: Arc<BoundedAdmission>,
    pub write: Arc<BoundedAdmission>,
    /// Shared by bounded cold display reads; never used for money commands.
    pub origin: Arc<BoundedAdmission>,
    pub config: ConsoleConfig,
    requests: [AtomicU64; 3],
    quota_rejected: [AtomicU64; 3],
    resource_rejected: [AtomicU64; 3],
    last_cleanup: std::sync::Mutex<std::time::Instant>,
}

impl ConsoleAdmission {
    pub fn new(config: ConsoleConfig) -> Result<Self, ApiError> {
        config
            .validate()
            .map_err(|message| ApiError::Config(message.to_string()))?;
        let make = |limit| {
            BoundedAdmission::new(AdmissionLimits {
                total: limit,
                per_key: limit,
                queue: config.queue_limit,
                queue_per_key: config.queue_limit,
                wait: Duration::from_millis(config.queue_timeout_ms),
            })
            .map_err(|message| ApiError::Config(message.to_string()))
        };
        Ok(Self {
            read: make(config.read_concurrency)?,
            write: make(config.write_concurrency)?,
            origin: BoundedAdmission::new(AdmissionLimits {
                total: config.origin_concurrency,
                per_key: 1,
                queue: config.queue_limit,
                queue_per_key: config.queue_limit.min(4),
                wait: Duration::from_millis(config.queue_timeout_ms),
            })
            .map_err(|message| ApiError::Config(message.into()))?,
            config,
            requests: std::array::from_fn(|_| AtomicU64::new(0)),
            quota_rejected: std::array::from_fn(|_| AtomicU64::new(0)),
            resource_rejected: std::array::from_fn(|_| AtomicU64::new(0)),
            last_cleanup: std::sync::Mutex::new(std::time::Instant::now()),
        })
    }

    pub fn close(&self) {
        // Shutdown middleware blocks new ordinary reads; leave the bounded
        // read gate usable by the explicitly allowed authenticated diagnostics.
        self.write.close();
        self.origin.close();
    }

    pub fn status(
        &self,
    ) -> (
        keycompute_runtime::admission::AdmissionStatus,
        keycompute_runtime::admission::AdmissionStatus,
    ) {
        (self.read.status(), self.write.status())
    }

    fn cleanup_expired_counters(&self, limiter: &keycompute_ratelimit::RateLimitService) {
        if limiter.backend() != keycompute_ratelimit::RateLimitBackend::Memory {
            return;
        }
        let mut last = self.last_cleanup.lock().unwrap_or_else(|e| e.into_inner());
        if last.elapsed() < Duration::from_secs(5) {
            return;
        }
        *last = std::time::Instant::now();
        drop(last);
        limiter.cleanup();
    }

    pub fn metrics(&self) -> serde_json::Value {
        let counts = |values: &[AtomicU64; 3]| {
            values
                .iter()
                .map(|v| v.load(Ordering::Relaxed))
                .collect::<Vec<_>>()
        };
        let admission = |gate: &BoundedAdmission| {
            let s = gate.status();
            serde_json::json!({"active":s.active,"queued":s.queued,"keys":s.keys})
        };
        serde_json::json!({"class_order":["read","heavy_read","write"],
            "requests":counts(&self.requests),"quota_rejected":counts(&self.quota_rejected),
            "resource_rejected":counts(&self.resource_rejected),
            "read":admission(&self.read),"write":admission(&self.write),"origin":admission(&self.origin)})
    }

    async fn acquire(&self, class: ConsoleClass) -> Result<AdmissionPermit, AdmissionError> {
        // One class-specific admission is acquired for the whole request. We
        // intentionally do not stack a global permit and a class permit: that
        // pattern can retain scarce global capacity while waiting on a narrow
        // class queue and deadlock under saturation.
        match class {
            ConsoleClass::Write => self.write.acquire(Uuid::nil()).await,
            ConsoleClass::Read | ConsoleClass::HeavyRead => self.read.acquire(Uuid::nil()).await,
        }
    }
}

fn class_uuid(class: ConsoleClass) -> Uuid {
    let digest = Sha256::digest(class.as_str().as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

fn class_limit(config: &ConsoleConfig, class: ConsoleClass) -> u32 {
    match class {
        ConsoleClass::Read => config.read_rpm,
        ConsoleClass::HeavyRead => config.heavy_read_rpm,
        ConsoleClass::Write => config.write_rpm,
    }
}

fn quota_key(
    class: ConsoleClass,
    tenant_id: Uuid,
    user_id: Uuid,
    api_key_id: Uuid,
) -> RateLimitKey {
    let _ = api_key_id;
    RateLimitKey::new(class_uuid(class), tenant_id, user_id)
}

fn scope_key(tenant_id: Uuid, user_id: Uuid, api_key_id: Uuid) -> RateLimitKey {
    RateLimitKey::new(tenant_id, user_id, api_key_id)
}

async fn quota_response(
    state: &AppState,
    key: &RateLimitKey,
    config: &RateLimitConfig,
    scope: &'static str,
) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        axum::Json(serde_json::json!({
            "error": {
                "message": "Console request budget exceeded. Please try again later.",
                "type": "rate_limit_error",
                "code": "console_quota_exceeded"
            }
        })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("x-ratelimit-scope", HeaderValue::from_static(scope));
    if let Ok(Ok(Some(remaining))) = tokio::time::timeout(
        Duration::from_millis(250),
        state.console_limiter.rpm_retry_after(key, config),
    )
    .await
    {
        let seconds = remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() != 0))
            .max(1);
        if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    response
}

fn resource_response(error: AdmissionError) -> Response {
    let message = match error {
        AdmissionError::Full => "Console capacity is full. Please retry later.",
        AdmissionError::Timeout => "Console capacity wait timed out. Please retry later.",
        AdmissionError::Closed => "Console capacity is closed.",
    };
    let mut response = ApiError::ServiceUnavailable(message.to_string()).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

/// Protect every classified console route before auth and maintenance DB work.
pub async fn middleware(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let Some(class) = keycompute_types::console::classify(req.method().as_str(), req.uri().path())
    else {
        return next.run(req).await;
    };

    state.console_admission.requests[class.index()].fetch_add(1, Ordering::Relaxed);
    let _permit = match state.console_admission.acquire(class).await {
        Ok(permit) => permit,
        Err(error) => {
            state.console_admission.resource_rejected[class.index()]
                .fetch_add(1, Ordering::Relaxed);
            return resource_response(error);
        }
    };
    state
        .console_admission
        .cleanup_expired_counters(&state.console_limiter);

    // A global session is an authenticated identity even without a tenant.
    let context = if let Some(context) = req
        .extensions()
        .get::<keycompute_auth::AuthContext>()
        .cloned()
    {
        Some(context)
    } else if let Some(auth) = req.extensions().get::<AuthExtractor>() {
        Some(auth.authorization_context())
    } else {
        let token = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        match token {
            Some(token) => match state.auth.verify_token(token).await {
                Ok(context) => Some(context),
                Err(error) => {
                    let mut response =
                        crate::extractors::authentication_error(error).into_response();
                    response
                        .headers_mut()
                        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                    return response;
                }
            },
            None => None,
        }
    };
    if let Some(context) = context {
        if context.credential_kind != keycompute_types::CredentialKind::Jwt
            || !context.has_permission(&keycompute_auth::Permission::AccessConsole)
        {
            let mut response =
                ApiError::Forbidden("Console session required".into()).into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            return response;
        }
        let mut checks = vec![
            (
                scope_key(Uuid::nil(), Uuid::nil(), Uuid::nil()),
                RateLimitConfig::new(state.console_admission.config.aggregate_rpm, u32::MAX),
                "console_all",
            ),
            (
                scope_key(Uuid::nil(), context.user_id, Uuid::nil()),
                RateLimitConfig::new(config_user(&state.console_admission.config), u32::MAX),
                "console_all",
            ),
        ];
        if let Some(tenant) = context.selected_tenant_id {
            checks.push((
                scope_key(tenant, Uuid::nil(), Uuid::nil()),
                RateLimitConfig::new(state.console_admission.config.tenant_rpm, u32::MAX),
                "console_all",
            ));
        }
        // Nil is an absent quota dimension, never a resource's tenant identity.
        checks.push((
            quota_key(
                class,
                context.selected_tenant_id.unwrap_or_default(),
                context.user_id,
                Uuid::nil(),
            ),
            RateLimitConfig::new(
                class_limit(&state.console_admission.config, class),
                u32::MAX,
            ),
            class.as_str(),
        ));
        for (key, config, label) in checks {
            match state
                .console_limiter
                .check_and_record_with_config(&key, &config)
                .await
            {
                Ok(()) => {}
                Err(keycompute_types::KeyComputeError::RateLimitExceeded(_)) => {
                    state.console_admission.quota_rejected[class.index()]
                        .fetch_add(1, Ordering::Relaxed);
                    return quota_response(&state, &key, &config, label).await;
                }
                Err(error) => {
                    tracing::warn!(%error, class=class.as_str(), "console quota backend failed");
                    let mut response = ApiError::ServiceUnavailable(
                        "Console quota service is temporarily unavailable".into(),
                    )
                    .into_response();
                    response
                        .headers_mut()
                        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                    return response;
                }
            }
        }
        req.extensions_mut().insert(context.clone());
        if let Ok(global) = GlobalConsoleAuth::try_from(context.clone()) {
            req.extensions_mut().insert(global);
        }
        if context.selected_tenant_id.is_some() {
            match AuthExtractor::from_auth_context(context) {
                Ok(auth) => {
                    req.extensions_mut().insert(auth);
                }
                Err(error) => return error.into_response(),
            }
        }
    }

    // Legacy heavy reads also respect the finite origin budget. The explicit
    // cached handlers acquire it only on misses, never recursively here.
    let cache_owned = matches!(
        req.uri().path(),
        "/api/v1/usage/stats"
            | "/api/v1/usage/trend"
            | "/api/v1/dashboard/overview"
            | "/api/v1/me/distribution/overview"
    );
    let _origin = if class == ConsoleClass::HeavyRead && !cache_owned {
        if let Some(context) = req.extensions().get::<keycompute_auth::AuthContext>() {
            let budget_key = context.selected_tenant_id.unwrap_or(context.user_id);
            match state.console_admission.origin.acquire(budget_key).await {
                Ok(permit) => Some(permit),
                Err(error) => {
                    state.console_admission.resource_rejected[class.index()]
                        .fetch_add(1, Ordering::Relaxed);
                    return resource_response(error);
                }
            }
        } else {
            None
        }
    } else {
        None
    };
    req.extensions_mut().insert(ConsoleAdmissionChecked);
    // Mutation fencing runs after the route's authorization, not here: a
    // console user denied by an admin route must not flush shared snapshots.
    let mut response = next.run(req).await;
    // These are private authenticated resources, never shared HTTP cache entries.
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

/// Fence self-service commands only after checking console-session authority.
/// Platform commands use the same fence from admin_auth_middleware after its
/// stronger authorization check. The outer console layer still bounds ingress
/// and counts abuse attempts, including denied authenticated requests.
pub(crate) async fn mutation_middleware(
    _auth: crate::extractors::GlobalConsoleAuth,
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    run_with_mutation_fence(&state, req, next).await
}

/// Global self-service commands do not require a selected tenant.
pub(crate) async fn global_mutation_middleware(
    _auth: crate::extractors::GlobalConsoleAuth,
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    run_with_mutation_fence(&state, req, next).await
}

pub(crate) async fn run_with_mutation_fence(
    state: &AppState,
    req: Request,
    next: Next,
) -> Response {
    // The guard fences both ends, including cancellation and uncertain results.
    let _mutation = matches!(
        keycompute_types::console::classify(req.method().as_str(), req.uri().path()),
        Some(ConsoleClass::Write)
    )
    .then(|| state.display_cache.mutation_guard());
    next.run(req).await
}

fn config_user(config: &ConsoleConfig) -> u32 {
    config.user_rpm
}

#[cfg(test)]
#[path = "console_tests.rs"]
mod tests;
