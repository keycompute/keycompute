<div align="center">

<img src="./logo.jpg" alt="KeyCompute logo" width="160" style="border-radius: 20px;" />

# KeyCompute

<p align="center">
  <a href="./README.zh-CN.md">简体中文</a> |
  <a href="./README.md">English</a> |
  <a href="./README.zh-TW.md">繁體中文</a> |
  <a href="./README.es.md">Español</a> |
  <a href="./README.ar.md">العربية</a>
</p>

**下一代高性能 AI Token 算力服务平台**

<p align="center">
  <a href="#功能特性">功能特性</a> •
  <a href="#快速开始">快速开始</a> •
  <a href="#配置说明">配置说明</a> •
  <a href="#项目结构">项目结构</a>
</p>

</div>

---

## 项目简介

KeyCompute 是一个**高性能**、**易扩展**、**开箱即用**的 AI Token 算力服务平台，提供统一的大模型接入、智能路由、计量计费、多级分销和可观测性等企业级能力。

> **注意**：本项目仅供个人学习使用，使用者必须在遵循 OpenAI [使用条款](https://openai.com/policies)以及法律法规的情况下使用，不得用于非法用途。根据《生成式人工智能服务管理暂行办法》的要求，请勿对中国地区公众提供一切未经备案的生成式人工智能服务。

---

## 功能特性

### 多模型支持

通过标准 **OpenAI API 格式**访问所有大模型，开箱即用：

| Provider | 模型系列 | 状态 |
|:---|:---|:---:|
| 🟢 OpenAI | GPT-5/GPT-4/GPT-4o/... | ✅ |
| 🟣 Anthropic | Claude 4/3.7/3.5/... | ✅ |
| 🔵 Google | Gemini 3/2.5/2.0/... | ✅ |
| 🔴 DeepSeek | DeepSeek-V4/V3/R1/... | ✅ |
| 🟠 Zhipu | GLM-5.1/5/4.7/... | ✅ |
| 🔴 MiniMax | MiniMax-M2.7/M2.5/... | ✅ |
| 🟤 Ollama | 本地模型 (Llama/Qwen/...) | ✅ |
| 🟡 vLLM | 自部署模型 | ✅ |

### 智能路由

- **双层路由引擎**：模型级路由 + 账号池路由
- **负载均衡**：支持多账号加权随机分配
- **失败自动重试**：请求失败自动切换渠道
- **健康检查**：实时监控 Provider 可用性

### 计费与支付

- **实时计费**：请求级价格快照，事后精确结算
- **在线充值**：支付宝、微信支付
- **用量统计**：详细的 Token 消耗明细
- **余额管理**：用户余额充值、消费追踪

### 二级分销

- **推荐奖励**：用户邀请新用户获得奖励
- **分销规则**：灵活配置分销比例
- **收益统计**：实时查看分销收益
- **邀请链接**：一键生成专属邀请链接

### 用户与权限

- **多用户支持**：用户注册、登录、权限管理
- **邮箱验证码**：注册验证码、密码重置
- **API Key 管理**：创建、删除、查看 API Key
- **分组限流**：用户级别请求限流

### 可观测性

- **Prometheus 指标**：请求量、延迟、错误率
- **结构化日志**：JSON 格式日志，便于分析
- **健康检查端点**：`/health` 接口监控服务状态

---

## 快速开始

### 环境要求

| 组件 | 版本要求 |
|:---|:---|
| Rust | ≥ 1.92 |
| Axum | ≥ 0.8.0 |
| Dioxus | ≥ 0.7.1 (前端开发) |
| PostgreSQL | ≥ 16 |
| Redis | ≥ 7 (可选，用于分布式限流) |
| Docker | 最新版 (容器部署) |

### 方式一：Docker Compose 部署（推荐）

```bash
# 克隆项目
git clone https://github.com/your-org/keycompute.git
cd keycompute

# 复制并编辑环境变量
cp .env.example .env
# 编辑 .env 填入真实配置

# 启动所有服务
docker compose up -d

# 查看服务状态
docker compose ps
```

部署完成后访问 `http://localhost:8080` 即可使用！

初始账号：`admin@keycompute.local`，密码：`12345`

> 生产环境请立即修改默认管理员密码！

### 方式二：本地开发

```bash
# 创建网络
docker network create keycompute-internal

# PostgreSQL（使用 .env 中的密码）
docker run -d \
  --name keycompute-postgres \
  --network keycompute-internal \
  -e POSTGRES_DB=keycompute \
  -e POSTGRES_USER=keycompute \
  -e POSTGRES_PASSWORD="ObpipdGz00wLxK1u1OupDP4rWVu1NEUpB5QGIiIGbek=" \
  -p 5432:5432 \
  -v keycompute_postgres_data:/var/lib/postgresql/data \
  --restart unless-stopped \
  postgres:16-alpine

# Redis（使用 .env 中的密码）
docker run -d \
  --name keycompute-redis \
  --network keycompute-internal \
  -p 6379:6379 \
  -v keycompute_redis_data:/data \
  --restart unless-stopped \
  redis:7-alpine \
  redis-server \
  --requirepass "1VoCAza2HoaOmCafAdM+oxj165CiYpgp2XmD9tTeLN0=" \
  --maxmemory 256mb \
  --maxmemory-policy allkeys-lru

# 安装 dioxus-cli
curl -sSL http://dioxus.dev/install.sh | sh

# 启动后端服务
export KC__DATABASE__URL="postgres://keycompute:ObpipdGz00wLxK1u1OupDP4rWVu1NEUpB5QGIiIGbek=@localhost:5432/keycompute"
export KC__REDIS__URL="redis://:1VoCAza2HoaOmCafAdM+oxj165CiYpgp2XmD9tTeLN0=@localhost:6379"
export KC__AUTH__JWT_SECRET="ea2fe6dd660639d1401c0c4c9fbd71cfe627785ae2359f3b0179efa7c0e24245f966a586295ed598db795da5a942dff7"
export KC__CRYPTO__SECRET_KEY="H8AS+HwrYBp/KSAWRLh9jcLnsV+SIvOtohDPRun+GXA="
export KC__EMAIL__SMTP_HOST="smtp.example.com"
export KC__EMAIL__SMTP_PORT="587"
export KC__EMAIL__SMTP_USERNAME="noreply@example.com"
export KC__EMAIL__SMTP_PASSWORD="your-smtp-password"
export KC__EMAIL__FROM_ADDRESS="noreply@example.com"
export APP_BASE_URL="https://app.example.com"
export KC__DEFAULT_ADMIN_EMAIL="admin@keycompute.local"
export KC__DEFAULT_ADMIN_PASSWORD="12345"

cargo run -p keycompute-server --features redis

# 启动前端开发服务器（另一个终端）
API_BASE_URL=http://localhost:3000 dx serve --package web --platform web --addr 0.0.0.0
```

---

## 项目结构

```text
keycompute/
├── crates/                    # 后端核心模块 (Rust)
│   ├── keycompute-server/      # Axum HTTP 服务
│   ├── keycompute-types/       # 全局共享类型
│   ├── keycompute-db/          # 数据库访问层
│   ├── keycompute-auth/        # 认证与鉴权
│   ├── keycompute-ratelimit/   # 分布式限流
│   ├── keycompute-pricing/     # 定价引擎
│   ├── keycompute-routing/     # 智能路由
│   ├── keycompute-runtime/     # 运行时状态
│   ├── keycompute-billing/     # 计费结算
│   ├── keycompute-distribution/# 二级分销
│   ├── keycompute-observability/# 可观测性
│   ├── keycompute-config/      # 配置管理
│   ├── keycompute-emailserver/ # 邮件服务
│   ├── llm-gateway/            # LLM 执行网关
│   └── llm-provider/           # Provider 适配器
│       ├── keycompute-openai/  # OpenAI/Claude/Gemini
│       ├── keycompute-deepseek/# DeepSeek
│       ├── keycompute-ollama/  # Ollama 本地模型
│       └── keycompute-vllm/    # vLLM 自部署模型
├── packages/                   # 前端 (Dioxus 0.7)
│   ├── web/                    # Web 管理后台
│   ├── ui/                     # 共享 UI 组件
│   └── client-api/             # API 客户端
├── nginx/                      # Nginx 配置
├── Dockerfile.server           # 后端镜像
├── Dockerfile.web              # 前端镜像
└── docker-compose.yml          # 容器编排
```

---

## 配置说明

### 环境变量

主要环境变量配置：

| 变量名 | 说明 | 必填 |
|:---|:---|:---:|
| `KC__DATABASE__URL` | PostgreSQL 连接串 | ✅ |
| `KC__REDIS__URL` | Redis 连接串 | ⚪ |
| `KC__AUTH__JWT_SECRET` | JWT 签名密钥 | ✅ |
| `KC__CRYPTO__SECRET_KEY` | API Key 加密密钥 | ✅ |
| `KC__EMAIL__SMTP_HOST` | SMTP 服务器地址 | ⚪ |
| `KC__EMAIL__SMTP_PORT` | SMTP 服务器端口 | ⚪ |
| `KC__EMAIL__SMTP_USERNAME` | SMTP 用户名 | ⚪ |
| `KC__EMAIL__SMTP_PASSWORD` | SMTP 密码 | ⚪ |
| `KC__EMAIL__FROM_ADDRESS` | 发件邮箱地址 | ⚪ |
| `KC__EMAIL__FROM_NAME` | 发件人显示名称 | ⚪ |
| `APP_BASE_URL` | 用于密码重置和邀请链接的公开前端地址；启用邮件或邀请链接时必须显式配置 | ⚪ |
| `KC__DEFAULT_ADMIN_EMAIL` | 默认管理员邮箱 | ⚪ |
| `KC__DEFAULT_ADMIN_PASSWORD` | 默认管理员密码 | ⚪ |

> 💡 提示：`KC__CRYPTO__SECRET_KEY` 一旦数据库写入数据后不可更改（会导致历史数据无法解密）

---

## API 接口

### OpenAI 兼容 API

```bash
# Chat Completions
curl https://your-domain/v1/chat/completions \
  -H "Authorization: Bearer sk-xxx" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
```

```bash
# 示例
curl -s http://192.168.100.100:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-329939d678d24433bc0277311c576481bc23b86ebc724354" \
  -d '{"model":"deepseek-chat","messages":[{"role":"user","content":"hello"}],"stream":true}'

data: {"id":"chatcmpl-7370f2606a6a4f5fa516fe54d9196c9d-kc","object":"chat.completion.chunk","created":1775231430,"model":"deepseek-chat","system_fingerprint":"fp_deepseek","choices":[{"index":0,"delta":{"role":"assistant","content":"你好！👋 很高兴见到你！\n今天有什么我可以帮忙的吗？"},"finish_reason":null}]}
```

```bash
# 列出模型
curl https://your-domain/v1/models \
  -H "Authorization: Bearer sk-xxx"
```

### 管理 API

| 接口 | 说明 |
|:---|:---|
| `POST /api/v1/auth/register` | 用户注册 |
| `POST /api/v1/auth/login` | 用户登录 |
| `GET /api/v1/me` | 获取当前用户 |
| `GET /api/v1/keys` | 列出我的 API Keys |
| `POST /api/v1/keys` | 创建 API Key |
| `GET /api/v1/usage` | 用量统计 |
| `GET /api/v1/billing/records` | 账单记录 |
| `POST /api/v1/payments/orders` | 创建支付订单 |

---

## 开发指南

```bash
# 编译
cargo build --workspace --exclude desktop --exclude mobile --verbose

# 运行测试
cargo test --lib --workspace --exclude desktop --exclude mobile --verbose
cargo test --package client-api --tests --verbose
cargo test --package integration-tests --tests --verbose

# 代码检查
cargo clippy --workspace --exclude desktop --exclude mobile --all-targets --all-features --future-incompat-report -- -D warnings
cargo fmt --all --check

# 启用 Redis 后端
cargo build -p keycompute-server --features redis
```

---

# 如何贡献

我们欢迎各种形式的贡献！请阅读 [CONTRIBUTING.md](CONTRIBUTING.md) 了解如何参与项目开发。

- 🐛 [报告 Bug](https://github.com/aiqubits/keycompute/issues/new?template=bug_report.yml)
- 💡 [功能建议](https://github.com/aiqubits/keycompute/issues/new?template=feature_request.yml)
- 🔧 [提交代码](CONTRIBUTING.md)

---

# 许可证

本项目采用 [MIT](LICENSE) 许可证开源。

---

<div align="center">

### 💖 感谢使用 KeyCompute

如果这个项目对你有帮助，欢迎给我们一个 ⭐️ Star！

**[快速开始](#快速开始)** • **[问题反馈](https://github.com/aiqubits/keycompute/issues)** • **[最新发布](https://github.com/aiqubits/keycompute/releases)**

</div>
