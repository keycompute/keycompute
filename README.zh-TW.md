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

**新一代高效能 AI Token 算力服務平台**

<p align="center">
  <a href="https://github.com/keycompute/keycompute/stargazers"><img src="https://img.shields.io/github/stars/keycompute/keycompute?style=social" alt="GitHub Stars" /></a>
  <a href="https://github.com/keycompute/keycompute/issues"><img src="https://img.shields.io/github/issues/keycompute/keycompute" alt="GitHub Issues" /></a>
  <a href="./LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="MIT License" /></a>
  <a href="./CONTRIBUTING.md"><img src="https://img.shields.io/badge/PRs-welcome-brightgreen" alt="PRs Welcome" /></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-1.92%2B-orange?logo=rust" alt="Rust Version" /></a>
</p>

<p align="center">
  <a href="#功能特色">功能特色</a> •
  <a href="#快速開始">快速開始</a> •
  <a href="#設定說明">設定說明</a> •
  <a href="#專案結構">專案結構</a>
</p>

</div>

---

## 專案簡介

KeyCompute 是一個**高效能**、**易於擴充**、**開箱即用**的 AI Token 算力服務平台，提供統一的大模型接入、智慧路由、計量計費、多層分銷與可觀測性等企業級能力。

> **注意**：本專案僅供個人學習使用，使用者必須在遵循 OpenAI [使用條款](https://openai.com/policies) 以及相關法律法規的前提下使用，不得用於非法用途。根據《生成式人工智慧服務管理暫行辦法》的要求，請勿向中國地區公眾提供任何未經備案的生成式人工智慧服務。

---

## 功能特色

### 多模型支援

透過標準 **OpenAI API 格式** 存取所有主流模型，開箱即用：

| Provider | 模型系列 | 狀態 |
|:---|:---|:---:|
| 🟢 OpenAI | GPT-5/GPT-4/GPT-4o/... | ✅ |
| 🟣 Anthropic | Claude 4/3.7/3.5/... | ✅ |
| 🔵 Google | Gemini 3/2.5/2.0/... | ✅ |
| 🔴 DeepSeek | DeepSeek-V4/V3/R1/... | ✅ |
| 🟠 Zhipu | GLM-5.1/5/4.7/... | ✅ |
| 🔴 MiniMax | MiniMax-M2.7/M2.5/... | ✅ |
| 🟤 Ollama | 本地模型 (Llama/Qwen/...) | ✅ |
| 🟡 vLLM | 自行部署模型 | ✅ |

### 智慧路由

- **雙層路由引擎**：模型層路由 + 帳號池路由
- **負載平衡**：支援多帳號加權隨機分配
- **失敗自動重試**：請求失敗時自動切換通道
- **健康檢查**：即時監控 Provider 可用性

### 計費與支付

- **即時計費**：請求級價格快照，事後精準結算
- **線上儲值**：支付寶、微信支付
- **用量統計**：詳細的 Token 消耗明細
- **餘額管理**：使用者儲值與消費追蹤

### 二級分銷

- **推薦獎勵**：邀請新使用者可獲得獎勵
- **分銷規則**：可彈性設定分銷比例
- **收益統計**：即時檢視分銷收益
- **邀請連結**：一鍵產生專屬邀請連結

### 使用者與權限

- **多使用者支援**：使用者註冊、登入與權限管理
- **電子郵件驗證碼**：註冊驗證碼與密碼重設
- **API Key 管理**：建立、刪除與檢視 API Key
- **分組限流**：使用者級請求限流

### 可觀測性

- **Prometheus 指標**：請求量、延遲與錯誤率
- **結構化日誌**：JSON 格式日誌，方便分析
- **健康檢查端點**：`/health` 介面監控服務狀態

---

## 快速開始

### 環境需求

| 元件 | 版本要求 |
|:---|:---|
| Rust | ≥ 1.92 |
| Axum | ≥ 0.8.0 |
| Dioxus | ≥ 0.7.1 (前端開發) |
| PostgreSQL | ≥ 16 |
| Redis | ≥ 7 (選用，用於分散式限流) |
| Docker | 最新版 (容器部署) |

### 方式一：Docker Compose 部署（推薦）

```bash
# 複製專案
git clone https://github.com/your-org/keycompute.git
cd keycompute

# 複製並編輯環境變數
cp .env.example .env
# 編輯 .env 並填入實際設定

# 啟動所有服務
docker compose up -d

# 檢查服務狀態
docker compose ps
```

部署完成後，造訪 `http://localhost:8080` 即可開始使用。

預設帳號：`admin@keycompute.local`，密碼：`12345`

> 正式環境請立即修改預設管理員密碼。

### 方式二：本機開發

```bash
# 建立網路
docker network create keycompute-internal

# PostgreSQL（使用 .env 中的密碼）
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

# Redis（使用 .env 中的密碼）
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

# 安裝 dioxus-cli
curl -sSL http://dioxus.dev/install.sh | sh

# 啟動後端服務
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

# 啟動前端開發伺服器（另一個終端）
API_BASE_URL=http://localhost:3000 dx serve --package web --platform web --addr 0.0.0.0
```

---

## 專案結構

```text
keycompute/
├── crates/                    # 後端核心模組 (Rust)
│   ├── keycompute-server/      # Axum HTTP 服務
│   ├── keycompute-types/       # 全域共享型別
│   ├── keycompute-db/          # 資料庫存取層
│   ├── keycompute-auth/        # 認證與授權
│   ├── keycompute-ratelimit/   # 分散式限流
│   ├── keycompute-pricing/     # 定價引擎
│   ├── keycompute-routing/     # 智慧路由
│   ├── keycompute-runtime/     # 執行時狀態
│   ├── keycompute-billing/     # 計費與結算
│   ├── keycompute-distribution/# 二級分銷
│   ├── keycompute-observability/# 可觀測性
│   ├── keycompute-config/      # 設定管理
│   ├── keycompute-emailserver/ # 郵件服務
│   ├── llm-gateway/            # LLM 執行閘道
│   └── llm-provider/           # Provider 適配器
│       ├── keycompute-openai/  # OpenAI/Claude/Gemini
│       ├── keycompute-deepseek/# DeepSeek
│       ├── keycompute-ollama/  # Ollama 本地模型
│       └── keycompute-vllm/    # vLLM 自行部署模型
├── packages/                   # 前端 (Dioxus 0.7)
│   ├── web/                    # Web 管理後台
│   ├── ui/                     # 共用 UI 元件
│   └── client-api/             # API 用戶端
├── nginx/                      # Nginx 設定
├── Dockerfile.server           # 後端映像
├── Dockerfile.web              # 前端映像
└── docker-compose.yml          # 容器編排
```

---

## 設定說明

### 環境變數

主要環境變數如下：

| 變數名 | 說明 | 必填 |
|:---|:---|:---:|
| `KC__DATABASE__URL` | PostgreSQL 連線字串 | ✅ |
| `KC__REDIS__URL` | Redis 連線字串 | ⚪ |
| `KC__AUTH__JWT_SECRET` | JWT 簽名金鑰 | ✅ |
| `KC__CRYPTO__SECRET_KEY` | API Key 加密金鑰 | ✅ |
| `KC__EMAIL__SMTP_HOST` | SMTP 伺服器位址 | ⚪ |
| `KC__EMAIL__SMTP_PORT` | SMTP 伺服器連接埠 | ⚪ |
| `KC__EMAIL__SMTP_USERNAME` | SMTP 使用者名稱 | ⚪ |
| `KC__EMAIL__SMTP_PASSWORD` | SMTP 密碼 | ⚪ |
| `KC__EMAIL__FROM_ADDRESS` | 寄件者電子郵件地址 | ⚪ |
| `KC__EMAIL__FROM_NAME` | 寄件者顯示名稱 | ⚪ |
| `APP_BASE_URL` | 用於密碼重設與邀請連結的公開前端位址；啟用郵件或邀請連結時必須明確設定 | ⚪ |
| `KC__DEFAULT_ADMIN_EMAIL` | 預設管理員電子郵件 | ⚪ |
| `KC__DEFAULT_ADMIN_PASSWORD` | 預設管理員密碼 | ⚪ |

> 💡 提示：`KC__CRYPTO__SECRET_KEY` 一旦資料庫寫入資料後便不可更改，否則歷史資料將無法解密。

---

## API 介面

### OpenAI 相容 API

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
# 範例
curl -s http://192.168.100.100:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-329939d678d24433bc0277311c576481bc23b86ebc724354" \
  -d '{"model":"deepseek-chat","messages":[{"role":"user","content":"hello"}],"stream":true}'

data: {"id":"chatcmpl-7370f2606a6a4f5fa516fe54d9196c9d-kc","object":"chat.completion.chunk","created":1775231430,"model":"deepseek-chat","system_fingerprint":"fp_deepseek","choices":[{"index":0,"delta":{"role":"assistant","content":"你好！👋 很高興見到你！\n今天有什麼我可以幫忙的嗎？"},"finish_reason":null}]}
```

```bash
# 列出模型
curl https://your-domain/v1/models \
  -H "Authorization: Bearer sk-xxx"
```

### 管理 API

| 介面 | 說明 |
|:---|:---|
| `POST /api/v1/auth/register` | 使用者註冊 |
| `POST /api/v1/auth/login` | 使用者登入 |
| `GET /api/v1/me` | 取得目前使用者 |
| `GET /api/v1/keys` | 列出我的 API Keys |
| `POST /api/v1/keys` | 建立 API Key |
| `GET /api/v1/usage` | 用量統計 |
| `GET /api/v1/billing/records` | 帳單紀錄 |
| `POST /api/v1/payments/orders` | 建立支付訂單 |

---

## 開發指南

```bash
# 編譯
cargo build --workspace --exclude desktop --exclude mobile --verbose

# 執行測試
cargo test --lib --workspace --exclude desktop --exclude mobile --verbose
cargo test --package client-api --tests --verbose
cargo test --package integration-tests --tests --verbose

# 程式碼檢查
cargo clippy --workspace --exclude desktop --exclude mobile --all-targets --all-features --future-incompat-report -- -D warnings
cargo fmt --all --check

# 啟用 Redis 後端
cargo build -p keycompute-server --features redis
```

---

# 如何貢獻

我們歡迎各種形式的貢獻！請閱讀 [CONTRIBUTING.md](CONTRIBUTING.md) 了解如何參與專案開發。

- 🐛 [回報 Bug](https://github.com/aiqubits/keycompute/issues/new?template=bug_report.yml)
- 💡 [功能建議](https://github.com/aiqubits/keycompute/issues/new?template=feature_request.yml)
- 🔧 [提交程式碼](CONTRIBUTING.md)

---

# 授權條款

本專案採用 [MIT](LICENSE) 授權條款開源。

---

<div align="center">

### 💖 感謝使用 KeyCompute

如果這個專案對你有幫助，歡迎給我們一個 ⭐️ Star！

**[快速開始](#快速開始)** • **[問題回報](https://github.com/aiqubits/keycompute/issues)** • **[最新版本](https://github.com/aiqubits/keycompute/releases)**

</div>
