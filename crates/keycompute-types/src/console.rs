//! Shared HTTP traffic classification; never derived from client-supplied headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleClass {
    Read,
    HeavyRead,
    Write,
}
impl ConsoleClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "console_read",
            Self::HeavyRead => "console_heavy_read",
            Self::Write => "console_write",
        }
    }
    pub const fn index(self) -> usize {
        match self {
            Self::Read => 0,
            Self::HeavyRead => 1,
            Self::Write => 2,
        }
    }
}
pub fn classify(method: &str, path: &str) -> Option<ConsoleClass> {
    if !path.starts_with("/api/v1/")
        || path.starts_with("/api/v1/auth/")
        || path.starts_with("/api/v1/payments/notify/")
        || matches!(path, "/api/v1/settings/public" | "/api/v1/requirements")
    {
        return None;
    }
    if !matches!(method, "GET" | "HEAD") {
        return Some(ConsoleClass::Write);
    }
    if path.starts_with("/api/v1/platform/operations/") && path.ends_with("/usage") {
        return Some(ConsoleClass::HeavyRead);
    }
    if [
        "/stats",
        "/earnings",
        "/trend",
        "/overview",
        "/referrals",
        "/export",
    ]
    .iter()
    .any(|suffix| path.ends_with(suffix))
    {
        Some(ConsoleClass::HeavyRead)
    } else {
        Some(ConsoleClass::Read)
    }
}

#[cfg(test)]
mod operations_tests {
    use super::*;
    #[test]
    fn platform_usage_aggregates_use_bounded_heavy_read_admission() {
        for path in [
            "/api/v1/platform/operations/usage",
            "/api/v1/platform/operations/tenants/tenant-id/usage",
        ] {
            assert_eq!(classify("GET", path), Some(ConsoleClass::HeavyRead));
        }
        assert_eq!(
            classify("GET", "/api/v1/platform/operations/tenants"),
            Some(ConsoleClass::Read)
        );
        assert_eq!(
            classify("GET", "/api/v1/tenants/tenant-id/usage"),
            Some(ConsoleClass::Read)
        );
        assert_eq!(
            classify("POST", "/api/v1/platform/operations/usage"),
            Some(ConsoleClass::Write)
        );
    }
}
