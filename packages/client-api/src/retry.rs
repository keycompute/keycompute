//! Bounded identity-isolated cooldowns. Server admission remains authoritative.
use crate::error::RateLimitInfo;
use reqwest::{Request, header::HeaderMap};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, time::Duration};
use web_time::Instant;

const MAX_ENTRIES: usize = 256;
const MAX_DELAY_SECS: u64 = 86_400;

#[derive(Clone, Hash, PartialEq, Eq)]
struct Key {
    origin: String,
    credential: [u8; 32],
    scope: String,
}

struct Entry {
    until: Instant,
    info: RateLimitInfo,
}

#[derive(Default)]
pub(crate) struct Cooldowns {
    entries: HashMap<Key, Entry>,
    base_path: String,
}

/// Keep category inference aligned with server console policy. Also check
/// the legacy authenticated scope while old servers are still deployed.
fn request_scope(request: &Request, base_path: &str) -> &'static str {
    let path = request.url().path();
    let path = path
        .strip_prefix(base_path)
        .filter(|path| path.starts_with('/'))
        .unwrap_or(path);
    if matches!(
        path,
        "/api/v1/auth/register" | "/api/v1/auth/register/complete" | "/api/v1/requirements"
    ) {
        return "registration";
    }
    if path.starts_with("/api/v1/auth/") {
        return "authentication";
    }
    if path == "/api/v1/settings/public" {
        return "public";
    }
    if path.starts_with("/v1/") || path.starts_with("/pt/v1/") || path.starts_with("/nt/v1/") {
        return "generation";
    }
    if !path.starts_with("/api/v1/") {
        return "public";
    }
    if let Some(class) = keycompute_types::console::classify(request.method().as_str(), path) {
        return class.as_str();
    }
    "public"
}

fn key(request: &Request, scope: &str) -> Key {
    let mut hash = Sha256::new();
    for name in ["authorization", "x-api-key"] {
        if let Some(value) = request.headers().get(name) {
            hash.update(value.as_bytes());
        }
        hash.update([0]);
    }
    Key {
        origin: request.url().origin().ascii_serialization(),
        credential: hash.finalize().into(),
        scope: scope.into(),
    }
}

impl Cooldowns {
    pub(crate) fn with_base_url(base_url: &str) -> Self {
        Self {
            entries: HashMap::new(),
            base_path: reqwest::Url::parse(base_url)
                .ok()
                .map(|url| url.path().trim_end_matches('/').to_string())
                .unwrap_or_default(),
        }
    }

    pub(crate) fn remaining(&mut self, request: &Request) -> Option<RateLimitInfo> {
        let now = Instant::now();
        self.entries.retain(|_, value| value.until > now);
        let scope = request_scope(request, &self.base_path);
        let legacy = matches!(
            scope,
            "console_read" | "console_heavy_read" | "console_write"
        );
        [
            Some(scope),
            legacy.then_some("authenticated"),
            legacy.then_some("console_all"),
        ]
        .into_iter()
        .flatten()
        .filter_map(|scope| self.entries.get(&key(request, scope)))
        .max_by_key(|entry| entry.until)
        .map(|entry| {
            let mut info = entry.info.clone();
            info.retry_after = Some(entry.until.saturating_duration_since(now));
            info
        })
    }

    pub(crate) fn record(&mut self, request: &Request, info: RateLimitInfo) {
        let now = Instant::now();
        self.entries.retain(|_, value| value.until > now);
        // Missing metadata gets a local protective backoff, not a purported
        // server reset. No 429 is automatically retried by this transport.
        let delay = info
            .retry_after
            .unwrap_or(Duration::from_secs(2))
            .max(Duration::from_secs(1))
            .min(Duration::from_secs(MAX_DELAY_SECS));
        let scope = info
            .scope
            .as_deref()
            .filter(|s| {
                *s == request_scope(request, &self.base_path)
                    || (matches!(*s, "authenticated" | "console_all")
                        && matches!(
                            request_scope(request, &self.base_path),
                            "console_read" | "console_heavy_read" | "console_write"
                        ))
            })
            .unwrap_or_else(|| request_scope(request, &self.base_path));
        let key = key(request, scope);
        let until = now + delay;
        if self
            .entries
            .get(&key)
            .is_some_and(|entry| entry.until >= until)
        {
            return;
        }
        if self.entries.len() >= MAX_ENTRIES
            && !self.entries.contains_key(&key)
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.until)
                .map(|(k, _)| k.clone())
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(key, Entry { until, info });
    }
}

pub(crate) fn response_metadata(headers: &HeaderMap) -> RateLimitInfo {
    let retry_after = headers
        .get("retry-after")
        .and_then(|h| h.to_str().ok())
        .and_then(|value| parse_retry_after(value, headers));
    let scope = headers
        .get("x-ratelimit-scope")
        .and_then(|h| h.to_str().ok())
        .filter(|s| {
            matches!(
                *s,
                "authenticated"
                    | "console_all"
                    | "console_read"
                    | "console_heavy_read"
                    | "console_write"
                    | "generation"
                    | "registration"
                    | "authentication"
                    | "public"
            )
        })
        .map(str::to_owned);
    RateLimitInfo {
        message: "Rate limit exceeded. Please try again later.".into(),
        retry_after,
        scope,
    }
}

fn parse_retry_after(value: &str, headers: &HeaderMap) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() || value.len() > 128 {
        return None;
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return Some(Duration::from_secs(
            value.parse::<u64>().ok()?.min(MAX_DELAY_SECS),
        ));
    }
    let deadline = httpdate::parse_http_date(value).ok()?;
    // Prefer server Date for clock-skew resistance. web-time is browser safe.
    let now = headers
        .get("date")
        .and_then(|h| h.to_str().ok())
        .and_then(|date| httpdate::parse_http_date(date).ok())
        .unwrap_or_else(|| {
            std::time::UNIX_EPOCH
                + web_time::SystemTime::now()
                    .duration_since(web_time::UNIX_EPOCH)
                    .unwrap_or_default()
        });
    Some(
        deadline
            .duration_since(now)
            .unwrap_or_default()
            .min(Duration::from_secs(MAX_DELAY_SECS)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn request(path: &str, token: &str) -> Request {
        reqwest::Client::new()
            .get(format!("https://example.test{path}"))
            .bearer_auth(token)
            .build()
            .unwrap()
    }

    #[test]
    fn retry_after_parses_delta_date_and_bounds_untrusted_values() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "date",
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(
            parse_retry_after("12", &headers),
            Some(Duration::from_secs(12))
        );
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:45 GMT", &headers),
            Some(Duration::from_secs(45))
        );
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:27:00 GMT", &headers),
            Some(Duration::ZERO)
        );
        assert_eq!(
            parse_retry_after("999999999", &headers),
            Some(Duration::from_secs(MAX_DELAY_SECS))
        );
        for value in ["", "-1", "1.2", "garbage", "18446744073709551616"] {
            assert_eq!(parse_retry_after(value, &headers), None);
        }
    }

    #[test]
    fn cooldowns_are_bounded_expire_and_isolate_scopes_credentials_and_origins() {
        let mut state = Cooldowns::default();
        let original = request("/api/v1/payments/balance", "a");
        let info = RateLimitInfo {
            message: "limited".into(),
            retry_after: Some(Duration::from_secs(20)),
            scope: Some("console_read".into()),
        };
        state.record(&original, info.clone());
        assert!(state.remaining(&original).is_some());
        assert!(
            state
                .remaining(&request("/api/v1/payments/balance", "b"))
                .is_none()
        );
        assert!(
            state
                .remaining(&request("/api/v1/usage/stats", "a"))
                .is_none()
        );
        let other = reqwest::Client::new()
            .get("https://other.test/api/v1/payments/balance")
            .bearer_auth("a")
            .build()
            .unwrap();
        assert!(state.remaining(&other).is_none());
        state
            .entries
            .values_mut()
            .for_each(|entry| entry.until = Instant::now() - Duration::from_secs(1));
        assert!(state.remaining(&original).is_none());
        let client = reqwest::Client::new();
        for i in 0..MAX_ENTRIES + 30 {
            let request = client
                .get("https://example.test/api/v1/keys")
                .bearer_auth(i.to_string())
                .build()
                .unwrap();
            state.record(&request, info.clone());
        }
        assert_eq!(state.entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn reverse_proxy_base_path_preserves_scope_isolation() {
        let mut state = Cooldowns::with_base_url("https://example.test/gateway/");
        state.record(
            &request("/gateway/api/v1/payments/balance", "a"),
            RateLimitInfo {
                message: "limited".into(),
                retry_after: Some(Duration::from_secs(60)),
                scope: Some("authenticated".into()),
            },
        );
        assert!(
            state
                .remaining(&request("/gateway/api/v1/usage/stats", "a"))
                .is_some()
        );
        assert!(
            state
                .remaining(&request("/gateway/api/v1/auth/login", "a"))
                .is_none()
        );
    }

    #[test]
    fn unexpected_legacy_scope_is_normalized_without_cross_class_blocking() {
        let mut state = Cooldowns::default();
        let login = request("/api/v1/auth/login", "a");
        state.record(
            &login,
            RateLimitInfo {
                message: "limited".into(),
                retry_after: Some(Duration::from_secs(60)),
                scope: Some("authenticated".into()),
            },
        );
        assert!(state.remaining(&login).is_some());
        assert!(
            state
                .remaining(&request("/api/v1/payments/balance", "a"))
                .is_none()
        );
        for scope in ["authentication", "public"] {
            let mut headers = HeaderMap::new();
            headers.insert("x-ratelimit-scope", HeaderValue::from_static(scope));
            assert_eq!(response_metadata(&headers).scope.as_deref(), Some(scope));
        }
    }

    #[test]
    fn registration_cooldown_does_not_block_login_or_token_refresh() {
        let mut state = Cooldowns::default();
        state.record(
            &request("/api/v1/auth/register", ""),
            RateLimitInfo {
                message: "limited".into(),
                retry_after: Some(Duration::from_secs(60)),
                scope: Some("registration".into()),
            },
        );
        assert!(
            state
                .remaining(&request("/api/v1/auth/register/complete", ""))
                .is_some()
        );
        assert!(
            state
                .remaining(&request("/api/v1/auth/login", ""))
                .is_none()
        );
        assert!(
            state
                .remaining(&request("/api/v1/auth/refresh-token", ""))
                .is_none()
        );
    }
    #[test]
    fn console_wide_cooldown_blocks_all_console_classes_not_login_or_generation() {
        let mut state = Cooldowns::default();
        state.record(
            &request("/api/v1/payments/balance", "a"),
            RateLimitInfo {
                message: "budget".into(),
                retry_after: Some(Duration::from_secs(10)),
                scope: Some("console_all".into()),
            },
        );
        for path in ["/api/v1/payments/balance", "/api/v1/usage/stats"] {
            assert!(state.remaining(&request(path, "a")).is_some());
            assert!(state.remaining(&request(path, "b")).is_none());
        }
        let write = reqwest::Client::new()
            .post("https://example.test/api/v1/keys")
            .bearer_auth("a")
            .build()
            .unwrap();
        assert!(state.remaining(&write).is_some());
        assert!(state.remaining(&request("/v1/models", "a")).is_none());
        assert!(state.remaining(&request("/nt/v1/models", "a")).is_none());
        assert!(
            state
                .remaining(&request("/api/v1/auth/login", "a"))
                .is_none()
        );
    }
}
