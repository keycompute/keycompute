//! Validate Redis storage roles on creation and every pooled checkout.
//! INFO is read-only: this module never changes a live Redis configuration.
use deadpool_redis::{
    Pool,
    redis::{self, aio::MultiplexedConnection},
};

#[derive(Debug, Clone)]
pub enum RedisConnectionRole {
    /// Generic library pools retain caller-owned policy; server pools use the
    /// two explicit roles below. This is not a production safety default.
    Unrestricted,
    CriticalState,
    EvictableCache {
        critical_pool: Pool,
    },
}

#[derive(Debug)]
struct PeerInfo {
    run_id: String,
    major_version: u32,
    policy: String,
    maxmemory: u64,
    role: String,
}

fn invalid(message: &'static str) -> redis::RedisError {
    redis::RedisError::from((redis::ErrorKind::InvalidClientConfig, message))
}
fn field<'a>(info: &'a str, key: &str) -> Option<&'a str> {
    info.lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(name, value)| (name == key).then_some(value.trim()))
}
fn parse_peer(server: &str, memory: &str, replication: &str) -> redis::RedisResult<PeerInfo> {
    let run_id = field(server, "run_id")
        .filter(|id| id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| invalid("Redis INFO did not provide a valid server identity"))?;
    Ok(PeerInfo {
        run_id: run_id.into(),
        major_version: field(server, "redis_version")
            .and_then(|v| v.split('.').next()?.parse().ok())
            .filter(|major| *major >= 7)
            .ok_or_else(|| invalid("Redis 7 or newer is required for safe Lua OOM semantics"))?,
        policy: field(memory, "maxmemory_policy")
            .ok_or_else(|| invalid("Redis INFO memory policy is unavailable"))?
            .into(),
        maxmemory: field(memory, "maxmemory")
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| invalid("Redis INFO memory budget is unavailable"))?,
        role: field(replication, "role")
            .ok_or_else(|| invalid("Redis INFO replication role is unavailable"))?
            .into(),
    })
}
async fn peer_info(conn: &mut MultiplexedConnection) -> redis::RedisResult<PeerInfo> {
    let (server, memory, replication): (String, String, String) = redis::pipe()
        .cmd("INFO")
        .arg("server")
        .cmd("INFO")
        .arg("memory")
        .cmd("INFO")
        .arg("replication")
        .query_async(conn)
        .await?;
    parse_peer(&server, &memory, &replication)
}
fn critical_peer(peer: &PeerInfo) -> redis::RedisResult<()> {
    if peer.major_version < 7 || peer.policy != "noeviction" || peer.role != "master" {
        return Err(invalid(
            "Critical Redis must be a writable primary using maxmemory-policy noeviction",
        ));
    }
    // An external managed instance can own its memory limit; deployments in
    // this repository set explicit maxmemory and container headroom separately.
    Ok(())
}
fn cache_peer(cache: &PeerInfo, critical: &PeerInfo) -> redis::RedisResult<()> {
    critical_peer(critical)?;
    if cache.run_id == critical.run_id {
        return Err(invalid(
            "Cache Redis must not be the critical Redis server (including aliases or database numbers)",
        ));
    }
    if cache.major_version < 7
        || cache.role != "master"
        || cache.maxmemory == 0
        || !matches!(
            cache.policy.as_str(),
            "allkeys-lru"
                | "allkeys-lfu"
                | "allkeys-random"
                | "volatile-lru"
                | "volatile-lfu"
                | "volatile-random"
                | "volatile-ttl"
        )
    {
        return Err(invalid(
            "Cache Redis must be a separate writable primary with a finite evictable memory budget",
        ));
    }
    Ok(())
}

pub(crate) async fn validate_connection(
    conn: &mut MultiplexedConnection,
    role: &RedisConnectionRole,
) -> redis::RedisResult<()> {
    match role {
        RedisConnectionRole::Unrestricted => Ok(()),
        RedisConnectionRole::CriticalState => critical_peer(&peer_info(conn).await?),
        RedisConnectionRole::EvictableCache { critical_pool } => {
            let cache = peer_info(conn).await?;
            // Only this direction exists: a critical pool never borrows cache
            // connections. The outer role-validation deadline bounds the wait.
            let mut critical = critical_pool.get().await.map_err(|_| {
                invalid("Critical Redis identity could not be verified for cache isolation")
            })?;
            cache_peer(&cache, &peer_info(&mut critical).await?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn peer(id: char, policy: &str) -> PeerInfo {
        PeerInfo {
            run_id: id.to_string().repeat(40),
            major_version: 7,
            policy: policy.into(),
            maxmemory: 128 * 1024 * 1024,
            role: "master".into(),
        }
    }
    #[test]
    fn critical_role_rejects_eviction_and_read_replicas() {
        assert!(critical_peer(&peer('a', "allkeys-lru")).is_err());
        let mut critical = peer('a', "noeviction");
        critical.role = "slave".into();
        assert!(critical_peer(&critical).is_err());
        assert!(critical_peer(&peer('a', "noeviction")).is_ok());
    }
    #[test]
    fn cache_role_requires_a_distinct_bounded_evictable_primary() {
        let critical = peer('a', "noeviction");
        assert!(cache_peer(&peer('a', "allkeys-lru"), &critical).is_err());
        assert!(cache_peer(&peer('b', "noeviction"), &critical).is_err());
        let mut cache = peer('b', "allkeys-lru");
        cache.maxmemory = 0;
        assert!(cache_peer(&cache, &critical).is_err());
        cache.maxmemory = 1;
        cache.role = "slave".into();
        assert!(cache_peer(&cache, &critical).is_err());
        assert!(cache_peer(&peer('b', "allkeys-lru"), &critical).is_ok());
    }
    #[test]
    fn incomplete_info_never_silently_proves_isolation() {
        assert!(parse_peer("", "", "").is_err());
        let id = format!("redis_version:7.0.0\r\nrun_id:{}\r\n", "a".repeat(40));
        let parsed = parse_peer(
            &id,
            "maxmemory:123\r\nmaxmemory_policy:noeviction\r\n",
            "role:master\r\n",
        )
        .unwrap();
        assert!(critical_peer(&parsed).is_ok());
    }
    #[test]
    fn redis_versions_without_flagged_script_safety_are_rejected() {
        let info = format!("run_id:{}\r\n", "a".repeat(40));
        let memory = "maxmemory:123\r\nmaxmemory_policy:noeviction\r\n";
        for version in ["6.2.0", "garbled", ""] {
            let server = format!("{info}redis_version:{version}\r\n");
            assert!(parse_peer(&server, memory, "role:master\r\n").is_err());
        }
    }
}
