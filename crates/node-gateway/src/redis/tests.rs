//! Real Redis regressions. CI must provide Redis; local runs may omit REDIS_URL.
use super::*;
use deadpool_redis::redis::{self, aio::MultiplexedConnection};
use keycompute_ratelimit::{RateLimitConfig, RateLimitKey, RateLimitService};
use tokio::{task::JoinHandle, time::timeout};

struct TestRedis {
    node: NodeGatewayRedis,
    fast: Pool,
    observer: MultiplexedConnection,
}

impl TestRedis {
    async fn new() -> Option<Self> {
        let url = std::env::var("REDIS_URL").or_else(|_| std::env::var("KC__REDIS__URL"));
        let url = match url {
            Ok(url) => url,
            Err(_) if std::env::var_os("CI").is_some() => "redis://127.0.0.1:6379".to_string(),
            Err(_) => {
                eprintln!("Redis isolation test skipped locally: set REDIS_URL to run it");
                return None;
            }
        };
        let config = RedisConfig {
            url: url.clone(),
            pool_size: 1,
            node_poll_pool_size: 1,
            node_result_pool_size: 1,
            pool_wait_timeout_ms: 100,
            command_timeout_ms: 500,
            ..Default::default()
        };
        let fast = RedisRuntimeStore::create_pool_with_timeouts(
            &url,
            1,
            Duration::from_secs(2),
            Duration::from_millis(100),
            Duration::from_millis(500),
        )
        .unwrap();
        let node = NodeGatewayRedis::new(
            Arc::new(RedisRuntimeStore::with_pool(fast.clone())),
            &config,
        )
        .unwrap();
        let observer = timeout(
            Duration::from_secs(3),
            redis::Client::open(url)
                .unwrap()
                .get_multiplexed_async_connection(),
        )
        .await
        .expect("Redis test connection timed out")
        .expect("configured Redis is required, not silently skipped");
        Some(Self {
            node,
            fast,
            observer,
        })
    }

    async fn fast_progress(&self) {
        timeout(Duration::from_secs(2), async {
            let limiter = RateLimitService::with_redis_pool(self.fast.clone());
            let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
            limiter
                .check_and_record_with_config(&key, &RateLimitConfig::default())
                .await
                .unwrap();
            let mut conn = self.fast.get().await.unwrap();
            let pong: String = redis::cmd("PING").query_async(&mut conn).await.unwrap();
            assert_eq!(pong, "PONG");
        })
        .await
        .expect("blocking work starved the fast Redis pool");
    }

    async fn wait_for_client(&mut self, id: i64, blocked: bool) {
        timeout(Duration::from_secs(3), async {
            loop {
                let clients: String = redis::cmd("CLIENT")
                    .arg("LIST")
                    .query_async(&mut self.observer)
                    .await
                    .unwrap();
                let line = clients.lines().find(|line| {
                    line.split_whitespace()
                        .any(|field| field == format!("id={id}"))
                });
                let matched = if blocked {
                    line.is_some_and(|line| {
                        line.split_whitespace().any(|field| {
                            field
                                .strip_prefix("flags=")
                                .is_some_and(|flags| flags.contains('b'))
                        })
                    })
                } else {
                    line.is_none()
                };
                if matched {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Redis client did not reach the expected state");
    }
}

async fn client_id(pool: &Pool) -> i64 {
    let mut connection = pool.get().await.unwrap();
    redis::cmd("CLIENT")
        .arg("ID")
        .query_async(&mut connection)
        .await
        .unwrap()
}

// Aborting on unwind prevents a failed assertion leaving a 30-second waiter
// behind in a shared CI Redis instance.
struct Waiter<T>(JoinHandle<T>);
impl<T> Drop for Waiter<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl<T> Waiter<T> {
    async fn finish(mut self) -> T {
        timeout(Duration::from_secs(3), &mut self.0)
            .await
            .expect("waiter did not finish")
            .unwrap()
    }
    async fn cancel(mut self) {
        self.0.abort();
        assert!(
            (&mut self.0)
                .await
                .as_ref()
                .is_err_and(|error| error.is_cancelled())
        );
    }
}

#[tokio::test]
async fn redis_isolation_both_blocking_pools_leave_rate_limits_and_producers_live() {
    let Some(mut env) = TestRedis::new().await else {
        return;
    };
    let poll_id = client_id(&env.node.poll_pool).await;
    let result_id = client_id(&env.node.result_pool).await;
    assert_ne!(poll_id, result_id);
    let model = format!("isolation-{}", Uuid::new_v4());
    let task = Uuid::new_v4();
    let node = env.node.clone();
    let waiting_model = model.clone();
    let poll = Waiter(tokio::spawn(async move {
        node.pop_from_model_queue(&waiting_model, 30).await
    }));
    let node = env.node.clone();
    let result = Waiter(tokio::spawn(
        async move { node.wait_for_result(task, 30).await },
    ));
    env.wait_for_client(poll_id, true).await;
    env.wait_for_client(result_id, true).await;
    env.fast_progress().await;
    timeout(Duration::from_secs(2), async {
        env.node.push_to_model_queue(&model, task).await.unwrap();
        env.node
            .push_result_notification(task, "succeeded")
            .await
            .unwrap();
    })
    .await
    .expect("queue producers or completion notifications were starved");
    assert_eq!(poll.finish().await.unwrap(), Some(task));
    assert_eq!(result.finish().await.unwrap().as_deref(), Some("succeeded"));
    // A normal reply recycles the connection instead of forcing a reconnect.
    assert_eq!(client_id(&env.node.poll_pool).await, poll_id);
    assert_eq!(client_id(&env.node.result_pool).await, result_id);
}

#[tokio::test]
async fn redis_isolation_poll_waiters_do_not_starve_result_waiters() {
    let Some(mut env) = TestRedis::new().await else {
        return;
    };
    let id = client_id(&env.node.poll_pool).await;
    let node = env.node.clone();
    let model = format!("isolation-{}", Uuid::new_v4());
    let waiting_model = model.clone();
    let poll = Waiter(tokio::spawn(async move {
        node.pop_from_model_queue(&waiting_model, 30).await
    }));
    env.wait_for_client(id, true).await;
    let task = Uuid::new_v4();
    env.node
        .push_result_notification(task, "succeeded")
        .await
        .unwrap();
    assert_eq!(
        timeout(Duration::from_secs(2), env.node.wait_for_result(task, 1))
            .await
            .unwrap()
            .unwrap()
            .as_deref(),
        Some("succeeded")
    );
    poll.cancel().await;
    env.wait_for_client(id, false).await;
}

#[tokio::test]
async fn redis_isolation_result_waiters_do_not_starve_task_claims() {
    let Some(mut env) = TestRedis::new().await else {
        return;
    };
    let id = client_id(&env.node.result_pool).await;
    let node = env.node.clone();
    let task = Uuid::new_v4();
    let waiter = Waiter(tokio::spawn(
        async move { node.wait_for_result(task, 30).await },
    ));
    env.wait_for_client(id, true).await;
    let model = format!("isolation-{}", Uuid::new_v4());
    env.node.push_to_model_queue(&model, task).await.unwrap();
    assert_eq!(
        timeout(
            Duration::from_secs(2),
            env.node.pop_from_model_queue(&model, 1)
        )
        .await
        .unwrap()
        .unwrap(),
        Some(task)
    );
    waiter.cancel().await;
    env.wait_for_client(id, false).await;
}

#[tokio::test]
async fn redis_isolation_saturated_pools_fail_with_bounded_wait() {
    let Some(mut env) = TestRedis::new().await else {
        return;
    };
    for result_pool in [false, true] {
        let pool = if result_pool {
            env.node.result_pool.clone()
        } else {
            env.node.poll_pool.clone()
        };
        let id = client_id(&pool).await;
        let node = env.node.clone();
        let task = Uuid::new_v4();
        let model = format!("isolation-{}", Uuid::new_v4());
        let waiting_model = model.clone();
        let waiter = Waiter(tokio::spawn(async move {
            if result_pool {
                node.wait_for_result(task, 30).await.map(|_| ())
            } else {
                node.pop_from_model_queue(&waiting_model, 30)
                    .await
                    .map(|_| ())
            }
        }));
        env.wait_for_client(id, true).await;
        let started = Instant::now();
        let result = timeout(Duration::from_secs(2), async {
            if result_pool {
                env.node
                    .wait_for_result(Uuid::new_v4(), 30)
                    .await
                    .map(|_| ())
            } else {
                env.node.pop_from_model_queue(&model, 30).await.map(|_| ())
            }
        })
        .await
        .expect("pool admission was unbounded");
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        env.fast_progress().await;
        waiter.cancel().await;
        env.wait_for_client(id, false).await;
        assert_eq!(pool.status().size, 0);
    }
}

#[tokio::test]
async fn redis_isolation_cancelled_brpop_does_not_steal_future_work_or_leak_capacity() {
    let Some(mut env) = TestRedis::new().await else {
        return;
    };
    for result_pool in [false, true] {
        for _ in 0..5 {
            let pool = if result_pool {
                env.node.result_pool.clone()
            } else {
                env.node.poll_pool.clone()
            };
            let id = client_id(&pool).await;
            let task = Uuid::new_v4();
            let model = format!("isolation-{}", Uuid::new_v4());
            let node = env.node.clone();
            let waiting_model = model.clone();
            let waiter = Waiter(tokio::spawn(async move {
                if result_pool {
                    node.wait_for_result(task, 30).await.map(|_| ())
                } else {
                    node.pop_from_model_queue(&waiting_model, 30)
                        .await
                        .map(|_| ())
                }
            }));
            env.wait_for_client(id, true).await;
            waiter.cancel().await;
            env.wait_for_client(id, false).await;
            assert_eq!(pool.status().size, 0, "cancelled socket was recycled");
            if result_pool {
                env.node
                    .push_result_notification(task, "succeeded")
                    .await
                    .unwrap();
                assert_eq!(
                    env.node.wait_for_result(task, 1).await.unwrap().as_deref(),
                    Some("succeeded")
                );
            } else {
                env.node.push_to_model_queue(&model, task).await.unwrap();
                assert_eq!(
                    env.node.pop_from_model_queue(&model, 1).await.unwrap(),
                    Some(task)
                );
            }
        }
    }
}

#[tokio::test]
async fn redis_isolation_transport_failure_discards_blocked_connection() {
    let Some(mut env) = TestRedis::new().await else {
        return;
    };
    let id = client_id(&env.node.poll_pool).await;
    let node = env.node.clone();
    let model = format!("isolation-{}", Uuid::new_v4());
    let waiter = Waiter(tokio::spawn(async move {
        node.pop_from_model_queue(&model, 30).await
    }));
    env.wait_for_client(id, true).await;
    let killed: i64 = redis::cmd("CLIENT")
        .arg("KILL")
        .arg("ID")
        .arg(id)
        .query_async(&mut env.observer)
        .await
        .unwrap();
    assert_eq!(killed, 1);
    assert!(waiter.finish().await.is_err());
    assert_eq!(env.node.poll_pool.status().size, 0);
    env.fast_progress().await;
}

#[tokio::test]
async fn redis_isolation_normal_timeout_recycles_and_zero_timeout_is_rejected() {
    let Some(env) = TestRedis::new().await else {
        return;
    };
    let id = client_id(&env.node.poll_pool).await;
    let model = format!("isolation-{}", Uuid::new_v4());
    assert!(env.node.pop_from_model_queue(&model, 0).await.is_err());
    assert!(env.node.wait_for_result(Uuid::new_v4(), 0).await.is_err());
    assert_eq!(
        timeout(
            Duration::from_secs(3),
            env.node.pop_from_model_queue(&model, 1)
        )
        .await
        .unwrap()
        .unwrap(),
        None
    );
    assert_eq!(client_id(&env.node.poll_pool).await, id);
}

#[tokio::test]
async fn redis_isolation_fast_pool_waits_and_commands_have_timeouts() {
    let Some(env) = TestRedis::new().await else {
        return;
    };
    let mut connection = env.fast.get().await.unwrap();
    assert!(
        timeout(Duration::from_secs(2), env.fast.get())
            .await
            .unwrap()
            .is_err()
    );
    // Deliberately issue an otherwise infinite command on an isolated test
    // fast connection to verify the factory's finite response timeout.
    let result: redis::RedisResult<Option<(String, String)>> = timeout(
        Duration::from_secs(2),
        connection.brpop(format!("isolation-{}", Uuid::new_v4()), 0.0),
    )
    .await
    .expect("short-command response timeout was not installed");
    assert!(result.is_err());
    drop(Connection::take(connection));
    env.fast_progress().await;
}

#[tokio::test]
async fn redis_isolation_fractional_deadline_bounds_pool_admission() {
    let Some(env) = TestRedis::new().await else {
        return;
    };
    let held = env.node.poll_pool.get().await.unwrap();
    let model = format!("isolation-{}", Uuid::new_v4());
    let started = Instant::now();
    let result = timeout(
        Duration::from_secs(1),
        env.node
            .pop_from_model_queue_with_timeout(&model, Duration::from_millis(25)),
    )
    .await
    .expect("fractional request budget was lost while acquiring a pool slot");
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_millis(500));
    drop(held);
    assert_eq!(
        timeout(
            Duration::from_secs(1),
            env.node
                .pop_from_model_queue_with_timeout(&model, Duration::from_millis(25)),
        )
        .await
        .unwrap()
        .unwrap(),
        None
    );
}

#[tokio::test]
async fn redis_isolation_outer_timeout_closes_pending_blocking_connection() {
    let Some(mut env) = TestRedis::new().await else {
        return;
    };
    for result_pool in [false, true] {
        let pool = if result_pool {
            env.node.result_pool.clone()
        } else {
            env.node.poll_pool.clone()
        };
        let id = client_id(&pool).await;
        let node = env.node.clone();
        let model = format!("isolation-{}", Uuid::new_v4());
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let waiter = Waiter(tokio::spawn(async move {
            // Arm a short outer deadline only after CLIENT LIST confirms this
            // specific BRPOP is in flight, rather than racing pool creation.
            let operation = async {
                if result_pool {
                    node.wait_for_result(Uuid::new_v4(), 30).await.map(|_| ())
                } else {
                    node.pop_from_model_queue(&model, 30).await.map(|_| ())
                }
            };
            tokio::pin!(operation);
            tokio::select! {
                result = &mut operation => panic!("BRPOP finished before outer timeout: {result:?}"),
                _ = cancel_rx => {}
            }
            timeout(Duration::from_millis(25), operation).await
        }));
        env.wait_for_client(id, true).await;
        cancel_tx.send(()).unwrap();
        assert!(waiter.finish().await.is_err());
        env.wait_for_client(id, false).await;
        assert_eq!(pool.status().size, 0);
        env.fast_progress().await;
    }
}

#[tokio::test]
async fn redis_isolation_cancelled_pool_admission_does_not_leak_waiters() {
    let Some(env) = TestRedis::new().await else {
        return;
    };
    for result_pool in [false, true] {
        let pool = if result_pool {
            env.node.result_pool.clone()
        } else {
            env.node.poll_pool.clone()
        };
        let held = pool.get().await.unwrap();
        let node = env.node.clone();
        let waiter = Waiter(tokio::spawn(async move {
            if result_pool {
                node.wait_for_result(Uuid::new_v4(), 30).await.map(|_| ())
            } else {
                node.pop_from_model_queue(&format!("isolation-{}", Uuid::new_v4()), 30)
                    .await
                    .map(|_| ())
            }
        }));
        timeout(Duration::from_secs(1), async {
            while pool.status().waiting == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("operation never entered pool admission");
        waiter.cancel().await;
        assert_eq!(pool.status().waiting, 0);
        drop(held);
        assert_eq!(pool.status().available, 1);
        assert!(pool.get().await.is_ok());
    }
}

/// A private RESP2 peer acknowledges connection setup, then deliberately never
/// answers BRPOP. It observes EOF to prove that timing out disposes the socket,
/// not merely the Rust request future. No shared Redis server is paused.
async fn unresponsive_brpop_peer(
    listener: tokio::net::TcpListener,
    started: tokio::sync::oneshot::Sender<()>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let (socket, _) = listener.accept().await.unwrap();
    let mut socket = BufReader::new(socket);
    loop {
        let mut line = String::new();
        assert_ne!(socket.read_line(&mut line).await.unwrap(), 0);
        let count: usize = line.strip_prefix('*').unwrap().trim().parse().unwrap();
        assert!(count <= 16);
        let mut args = Vec::with_capacity(count);
        for _ in 0..count {
            line.clear();
            socket.read_line(&mut line).await.unwrap();
            let size: usize = line.strip_prefix('$').unwrap().trim().parse().unwrap();
            assert!(size <= 1024);
            let mut arg = vec![0; size + 2];
            socket.read_exact(&mut arg).await.unwrap();
            assert_eq!(&arg[size..], b"\r\n");
            arg.truncate(size);
            args.push(arg);
        }
        if args[0].eq_ignore_ascii_case(b"BRPOP") {
            started.send(()).unwrap();
            let error = socket
                .read_u8()
                .await
                .expect_err("socket was reused after timeout");
            assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
            return;
        }
        let response = if args[0].eq_ignore_ascii_case(b"INFO") {
            let section = args.get(1).expect("INFO section");
            let value = if section.eq_ignore_ascii_case(b"server") {
                format!("redis_version:7.0.0\r\nrun_id:{}\r\n", "a".repeat(40))
            } else if section.eq_ignore_ascii_case(b"memory") {
                "maxmemory:1048576\r\nmaxmemory_policy:noeviction\r\n".into()
            } else {
                assert!(section.eq_ignore_ascii_case(b"replication"));
                "role:master\r\n".into()
            };
            format!("${}\r\n{}\r\n", value.len(), value)
        } else if args[0].eq_ignore_ascii_case(b"PING") {
            args.get(1).map_or_else(
                || "+PONG\r\n".to_string(),
                |value| format!("${}\r\n{}\r\n", value.len(), String::from_utf8_lossy(value)),
            )
        } else {
            assert!(
                args[0].eq_ignore_ascii_case(b"CLIENT")
                    || args[0].eq_ignore_ascii_case(b"SELECT")
                    || args[0].eq_ignore_ascii_case(b"UNWATCH"),
                "unexpected setup command: {args:?}"
            );
            "+OK\r\n".to_string()
        };
        socket
            .get_mut()
            .write_all(response.as_bytes())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn redis_isolation_internal_deadline_discards_unresponsive_socket() {
    for result_pool in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = RedisConfig {
            url: format!("redis://{}", listener.local_addr().unwrap()),
            node_poll_pool_size: 1,
            node_result_pool_size: 1,
            // Blocking operations must override this short-command timeout.
            command_timeout_ms: 20,
            ..Default::default()
        };
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let peer = Waiter(tokio::spawn(unresponsive_brpop_peer(listener, started_tx)));
        let fast = RedisRuntimeStore::new(&config.url).unwrap();
        let node = NodeGatewayRedis::new(Arc::new(fast), &config).unwrap();
        let pool = if result_pool {
            node.result_pool.clone()
        } else {
            node.poll_pool.clone()
        };
        let operation_pool = pool.clone();
        let operation = Waiter(tokio::spawn(async move {
            let start = Instant::now();
            let result = node
                .blocking_pop(
                    &operation_pool,
                    "isolation:stalled",
                    Duration::from_millis(80),
                )
                .await;
            (result, start.elapsed())
        }));
        timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        let (result, elapsed) = operation.finish().await;
        assert!(
            result.is_err(),
            "a missing Redis reply cannot be a normal timeout/Nil"
        );
        assert!(
            elapsed >= Duration::from_millis(80),
            "short-command timeout leaked into BRPOP"
        );
        assert_eq!(pool.status().size, 0, "unresponsive socket was recycled");
        peer.finish().await;
    }
}
