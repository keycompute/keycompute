//! Canonical route composition for the tenant-aware API surface.
//!
//! Existing resource methods still use the routes currently implemented by
//! the server. New callers can use these helpers while platform/tenant route
//! cutover is coordinated with the backend.

fn scoped(scope: &str, path: &str) -> String {
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        format!("/api/v1/{scope}")
    } else {
        format!("/api/v1/{scope}/{path}")
    }
}

pub fn platform(path: &str) -> String {
    scoped("platform", path)
}

pub fn tenant(path: &str) -> String {
    scoped("tenant", path)
}

pub fn me(path: &str) -> String {
    scoped("me", path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composes_canonical_scopes_without_double_slashes() {
        assert_eq!(platform("users"), "/api/v1/platform/users");
        assert_eq!(tenant("/memberships"), "/api/v1/tenant/memberships");
        assert_eq!(me(""), "/api/v1/me");
    }
}
