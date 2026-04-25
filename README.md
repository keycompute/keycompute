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

**Next-generation high-performance AI token compute service platform**

<p align="center">
  <a href="https://github.com/keycompute/keycompute/stargazers"><img src="https://img.shields.io/github/stars/keycompute/keycompute?style=social" alt="GitHub Stars" /></a>
  <a href="https://github.com/keycompute/keycompute/issues"><img src="https://img.shields.io/github/issues/keycompute/keycompute" alt="GitHub Issues" /></a>
  <a href="./LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="MIT License" /></a>
  <a href="./CONTRIBUTING.md"><img src="https://img.shields.io/badge/PRs-welcome-brightgreen" alt="PRs Welcome" /></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-1.92%2B-orange?logo=rust" alt="Rust Version" /></a>
</p>

<p align="center">
  <a href="#features">Features</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#configuration">Configuration</a> •
  <a href="#project-structure">Project Structure</a>
</p>

</div>

---

## Overview

KeyCompute is a **high-performance**, **extensible**, and **out-of-the-box** AI token compute service platform. It provides enterprise-grade capabilities including unified LLM access, smart routing, metering and billing, multi-level distribution, and observability.

> **Note**: This project is for personal learning only. You must use it in compliance with OpenAI [Terms of Use](https://openai.com/policies) and applicable laws and regulations. Do not use it for illegal purposes. In accordance with the Interim Measures for the Administration of Generative Artificial Intelligence Services, do not provide any unregistered generative AI services to the public in China.

---

## Features

### Multi-model support

Access all major models through the standard **OpenAI API format** out of the box:

| Provider | Model Families | Status |
|:---|:---|:---:|
| 🟢 OpenAI | GPT-5/GPT-4/GPT-4o/... | ✅ |
| 🟣 Anthropic | Claude 4/3.7/3.5/... | ✅ |
| 🔵 Google | Gemini 3/2.5/2.0/... | ✅ |
| 🔴 DeepSeek | DeepSeek-V4/V3/R1/... | ✅ |
| 🟠 Zhipu | GLM-5.1/5/4.7/... | ✅ |
| 🔴 MiniMax | MiniMax-M2.7/M2.5/... | ✅ |
| 🟤 Ollama | Local models (Llama/Qwen/...) | ✅ |
| 🟡 vLLM | Self-hosted models | ✅ |

### Smart routing

- **Two-layer routing engine**: model-level routing + account-pool routing
- **Load balancing**: weighted random allocation across multiple accounts
- **Automatic retry on failure**: switch channels automatically when a request fails
- **Health checks**: monitor provider availability in real time

### Billing & payments

- **Real-time billing**: request-level price snapshots with precise post-settlement
- **Online top-up**: Alipay and WeChat Pay
- **Usage analytics**: detailed token consumption breakdowns
- **Balance management**: top-up and spending tracking

### Referral distribution

- **Referral rewards**: earn rewards for inviting new users
- **Distribution rules**: flexibly configure commission ratios
- **Revenue analytics**: view referral earnings in real time
- **Invite links**: generate exclusive invite links with one click

### Users & permissions

- **Multi-user support**: user registration, login, and permission management
- **Email codes**: signup email codes and password reset
- **API key management**: create, delete, and view API keys
- **Group-based rate limiting**: user-level request throttling

### Observability

- **Prometheus metrics**: request volume, latency, and error rate
- **Structured logging**: JSON logs for easier analysis
- **Health check endpoint**: `/health` for service status monitoring

---

## Quick Start

### Requirements

| Component | Version Requirement |
|:---|:---|
| Rust | ≥ 1.92 |
| Axum | ≥ 0.8.0 |
| Dioxus | ≥ 0.7.1 (frontend development) |
| PostgreSQL | ≥ 16 |
| Redis | ≥ 7 (optional, for distributed rate limiting) |
| Docker | Latest (container deployment) |

### Option 1: Docker Compose deployment (recommended)

```bash
# Clone the project
git clone https://github.com/your-org/keycompute.git
cd keycompute

# Copy and edit environment variables
cp .env.example .env
# Edit .env and fill in real configuration values

# Start all services
docker compose up -d

# Check service status
docker compose ps
```

After deployment, visit `http://localhost:8080` to get started.

Default account: `admin@keycompute.local`, password: `12345`

> Change the default administrator password immediately in production.

### Option 2: Local development

```bash
# Create the network
docker network create keycompute-internal

# PostgreSQL (using the password from .env)
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

# Redis (using the password from .env)
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

# Install dioxus-cli
curl -sSL http://dioxus.dev/install.sh | sh

# Start the backend service
export KC__DATABASE__URL="postgres://keycompute:ObpipdGz00wLxK1u1OupDP4rWVu1NEUpB5QGIiIGbek=@localhost:5432/keycompute"
export KC__REDIS__URL="redis://:1VoCAza2HoaOmCafAdM+oxj165CiYpgp2XmD9tTeLN0=@localhost:6379"
export KC__AUTH__JWT_SECRET="ea2fe6dd660639d1401c0c4c9fbd71cfe627785ae2359f3b0179efa7c0e24245f966a586295ed598db795da5a942dff7"
export KC__CRYPTO__SECRET_KEY="H8AS+HwrYBp/KSAWRLh9jcLnsV+SIvOtohDPRun+GXA="
export KC__EMAIL__SMTP_HOST="smtp.example.com"
export KC__EMAIL__SMTP_PORT="465"
export KC__EMAIL__SMTP_USERNAME="noreply@example.com"
export KC__EMAIL__SMTP_PASSWORD="your-smtp-password"
export KC__EMAIL__FROM_ADDRESS="noreply@example.com"
export APP_BASE_URL="https://app.example.com"
export KC__DEFAULT_ADMIN_EMAIL="admin@keycompute.local"
export KC__DEFAULT_ADMIN_PASSWORD="12345"

cargo run -p keycompute-server --features redis

# Start the frontend development server (in another terminal)
API_BASE_URL=http://localhost:3000 dx serve --package web --platform web --addr 0.0.0.0
```

---

## Project Structure

```text
keycompute/
├── crates/                    # Backend core modules (Rust)
│   ├── keycompute-server/      # Axum HTTP service
│   ├── keycompute-types/       # Shared types
│   ├── keycompute-db/          # Database access layer
│   ├── keycompute-auth/        # Authentication and authorization
│   ├── keycompute-ratelimit/   # Distributed rate limiting
│   ├── keycompute-pricing/     # Pricing engine
│   ├── keycompute-routing/     # Smart routing
│   ├── keycompute-runtime/     # Runtime state
│   ├── keycompute-billing/     # Billing and settlement
│   ├── keycompute-distribution/# Referral distribution
│   ├── keycompute-observability/# Observability
│   ├── keycompute-config/      # Configuration management
│   ├── keycompute-emailserver/ # Email service
│   ├── llm-gateway/            # LLM execution gateway
│   └── llm-provider/           # Provider adapters
│       ├── keycompute-openai/  # OpenAI/Claude/Gemini
│       ├── keycompute-deepseek/# DeepSeek
│       ├── keycompute-ollama/  # Ollama local models
│       └── keycompute-vllm/    # vLLM self-hosted models
├── packages/                   # Frontend (Dioxus 0.7)
│   ├── web/                    # Web admin dashboard
│   ├── ui/                     # Shared UI components
│   └── client-api/             # API client
├── nginx/                      # Nginx configuration
├── Dockerfile.server           # Backend image
├── Dockerfile.web              # Frontend image
└── docker-compose.yml          # Container orchestration
```

---

## Configuration

### Environment variables

Primary environment variables:

| Variable | Description | Required |
|:---|:---|:---:|
| `KC__DATABASE__URL` | PostgreSQL connection string | ✅ |
| `KC__REDIS__URL` | Redis connection string | ⚪ |
| `KC__AUTH__JWT_SECRET` | JWT signing secret | ✅ |
| `KC__CRYPTO__SECRET_KEY` | API key encryption secret | ✅ |
| `KC__EMAIL__SMTP_HOST` | SMTP host | ⚪ |
| `KC__EMAIL__SMTP_PORT` | SMTP port | ⚪ |
| `KC__EMAIL__SMTP_USERNAME` | SMTP username | ⚪ |
| `KC__EMAIL__SMTP_PASSWORD` | SMTP password | ⚪ |
| `KC__EMAIL__FROM_ADDRESS` | Sender email address | ⚪ |
| `KC__EMAIL__FROM_NAME` | Sender display name | ⚪ |
| `APP_BASE_URL` | Public frontend base URL for password reset and invite links; must be explicitly configured when email or invite links are enabled | ⚪ |
| `KC__DEFAULT_ADMIN_EMAIL` | Default administrator email | ⚪ |
| `KC__DEFAULT_ADMIN_PASSWORD` | Default administrator password | ⚪ |

> 💡 Tip: Once data has been written to the database, `KC__CRYPTO__SECRET_KEY` must not be changed, or historical data will no longer be decryptable.

---

## API

### OpenAI-compatible API

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
# Example
curl -s http://192.168.100.100:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-329939d678d24433bc0277311c576481bc23b86ebc724354" \
  -d '{"model":"deepseek-chat","messages":[{"role":"user","content":"hello"}],"stream":true}'

data: {"id":"chatcmpl-7370f2606a6a4f5fa516fe54d9196c9d-kc","object":"chat.completion.chunk","created":1775231430,"model":"deepseek-chat","system_fingerprint":"fp_deepseek","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello! 👋 Nice to meet you!\nHow can I help today?"},"finish_reason":null}]}
```

```bash
# List models
curl https://your-domain/v1/models \
  -H "Authorization: Bearer sk-xxx"
```

### Admin API

| Endpoint | Description |
|:---|:---|
| `POST /api/v1/auth/register` | User registration |
| `POST /api/v1/auth/login` | User login |
| `GET /api/v1/me` | Get current user |
| `GET /api/v1/keys` | List my API keys |
| `POST /api/v1/keys` | Create an API key |
| `GET /api/v1/usage` | Usage statistics |
| `GET /api/v1/billing/records` | Billing records |
| `POST /api/v1/payments/orders` | Create a payment order |

---

## Development Guide

```bash
# Build
cargo build --workspace --exclude desktop --exclude mobile --verbose

# Run tests
cargo test --lib --workspace --exclude desktop --exclude mobile --verbose
cargo test --package client-api --tests --verbose
cargo test --package integration-tests --tests --verbose

# Code checks
cargo clippy --workspace --exclude desktop --exclude mobile --all-targets --all-features --future-incompat-report -- -D warnings
cargo fmt --all --check

# Enable the Redis backend
cargo build -p keycompute-server --features redis
```

---

# Contributing

We welcome contributions of all kinds. Please read [CONTRIBUTING.md](CONTRIBUTING.md) to learn how to get involved.

- 🐛 [Report bugs](https://github.com/aiqubits/keycompute/issues/new?template=bug_report.yml)
- 💡 [Feature requests](https://github.com/aiqubits/keycompute/issues/new?template=feature_request.yml)
- 🔧 [Submit code](CONTRIBUTING.md)

---

# License

This project is open sourced under the [MIT](LICENSE) License.

---

<div align="center">

### 💖 Thanks for using KeyCompute

If this project helps you, feel free to give it a ⭐️ star.

**[Quick Start](#quick-start)** • **[Report Issues](https://github.com/aiqubits/keycompute/issues)** • **[Latest Releases](https://github.com/aiqubits/keycompute/releases)**

</div>
