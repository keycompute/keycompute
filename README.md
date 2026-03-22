# KeyCompute

KeyCompute 是一个高性能，易拓展，开箱即用的 AI 算力服务平台y，提供统一的大模型接入、智能路由、计费和可观测性等服务。

支持多种类型的 AI API 算力池聚合，包括不限于自建算力，公共厂商，OAuth 代理等。

## 特性

- **API Gateway**: OpenAI-compatible REST API
- **双层路由**: 模型路由 + 账号池路由
- **流式处理**: SSE 流式响应，实时 Token 累积
- **计费系统**: 请求级价格快照，事后结算
- **可观测性**: Prometheus 指标 + 结构化日志

## 项目结构

```
key_compute/
├── crates/          # 后端核心 (Rust)
│   ├── keycompute-types/         # 全局共享类型
│   ├── keycompute-observability/ # 日志/指标/监控
│   ├── keycompute-db/            # 数据库访问
│   ├── keycompute-auth/          # 鉴权
│   ├── keycompute-ratelimit/     # 限流
│   ├── keycompute-pricing/       # 定价
│   ├── keycompute-routing/       # 路由引擎
│   ├── keycompute-runtime/       # 运行时状态
│   ├── llm-gateway/              # LLM 执行网关
│   ├── llm-provider/             # Provider 适配器
│   ├── keycompute-billing/       # 计费
│   └── keycompute-distribution/  # 分销
└── packages/        # 前端 (Dioxus Fullstack)
    ├── web/         # Web 管理后台
    ├── ui/          # 共享 UI 组件
    └── api/         # Server Functions
```

## 快速开始

```bash
cd key_compute

# 编译
cargo build --workspace

# 运行测试
cargo test --workspace

# 检查代码
cargo clippy --workspace
cargo fmt --check
```

## 实现阶段

| 阶段 | 模块 | 状态 |
|:---:|:---|:---:|
| P0 | keycompute-types, keycompute-observability | ✅ |
| P1 | keycompute-db | ✅ |
| P2 | Provider Trait, OpenAI Adapter | ✅ |
| P3 | keycompute-runtime | ⏳ |
| P4 | Auth, RateLimit, Pricing, Routing | ⏳ |
| P5 | llm-gateway | ⏳ |
| P6 | Billing, Distribution | ⏳ |
| P7 | keycompute-server | ⏳ |
| P8 | 其他 Provider | ⏳ |
| P9 | 前端 | ⏳ |

## License

MIT
