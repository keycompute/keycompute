# 控制台读路径与流量保护

控制台优化不能通过放宽资金、权限或模型执行约束换取吞吐。展示读取与命令处理必须分别评估；浏览器取消请求也不代表服务器已经撤销命令。

## 429 与客户端重试

RPM 拒绝响应可以携带 `Retry-After` 和 `X-RateLimit-Scope`，并声明 `Cache-Control: no-store`。这些头对跨域客户端可见。

- 内存限流器根据当前固定窗口计算剩余时间。
- Redis 根据自身时钟和当前滑动窗口计算恢复时间。动态降低限额时，使用足以释放一个名额的事件位置，而不是一律使用最旧事件。
- 元数据查询不计入请求数，不延长 Redis 桶的有效期，只在拒绝路径执行，等待预算为 250 毫秒。无法获得可靠元数据时保留拒绝，不伪造服务器恢复时间。
- 恢复时间只是建议，后续请求仍必须重新通过原子准入检查。

客户端不自动重试 429。相同身份、源站和类别的后续调用共享冷却状态；不同身份不会互相阻塞。旧版 `authenticated` 类别仅作为控制台类别的兼容约束，不让控制台限流误伤登录和刷新令牌。

客户端接受 `Retry-After` 的秒数及 HTTP 日期形式；HTTP 日期优先参考服务端 `Date`，减少时钟偏差。异常大值最多保留一天的本地冷却。缺失有效恢复信息时使用两秒本地保护退避，这不代表服务端窗口会在两秒后恢复。

网络错误与部分 5xx 仅允许安全读取重试。普通 POST、PUT、DELETE 不自动重放；显式 `post_json_with_idempotency_key` 只适用于服务端确实提供幂等保障的命令，并在各次尝试中保持相同键和请求体。不能因请求体可以克隆或携带任意幂等头就推定操作安全。

每个逻辑请求共享一个截止时间，覆盖发送、读取响应与退避。重试次数受配置和五次硬上限约束，退避包含随机抖动。Native 和 WASM 均有截止时间；固定依赖中的 reqwest WASM `AbortGuard` 在 future/response 被丢弃时中止浏览器 fetch，但这不能回滚服务器已经提交的命令。

冷却表最多 256 项，不持久化原始凭证。客户端 Debug 输出不包含认证令牌。错误和加载状态不会作为金额或邀请链接展示。

## 验证要求

数据库和 Redis 测试必须使用专门创建的隔离实例。不要让测试回落到开发主机默认的 5432/6379 端口，更不能使用生产数据。设置 `CI=1` 可让依赖连接失败明确导致测试失败，而不是把 Redis 集成测试误记为通过。

本阶段回归入口：

```sh
cargo test -p keycompute-ratelimit --features redis
cargo test -p keycompute-server --lib
cargo test -p client-api
cargo test -p web
cargo check -p web -p client-api --target wasm32-unknown-unknown
cargo clippy -p client-api --all-targets -- -D warnings
cargo clippy -p client-api --target wasm32-unknown-unknown -- -D warnings
```

新增测试覆盖固定/滑动窗口恢复时间、动态降低配额、Redis TTL 不被延长、认证/公共类别隔离、反向代理路径前缀、跨身份隔离、冷却表有界性、429 后续请求抑制、安全读取重试、幂等命令请求体与键不变、非幂等写不重试、逻辑请求截止时间及错误不进入金额显示。
