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

**新一代高性能 AI Token 算力服務平台**

<p align="center">
  <a href="https://github.com/keycompute/keycompute/stargazers"><img src="https://img.shields.io/github/stars/keycompute/keycompute?style=social" alt="GitHub Stars" /></a>
  <a href="https://github.com/keycompute/keycompute/issues"><img src="https://img.shields.io/github/issues/aiqubits/keycompute" alt="GitHub Issues" /></a>
  <a href="./LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="MIT License" /></a>
  <a href="./CONTRIBUTING.md"><img src="https://img.shields.io/badge/PRs-welcome-brightgreen" alt="PRs Welcome" /></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-1.92%2B-orange?logo=rust" alt="Rust Version" /></a>
</p>

<p align="center">
  <a href="#功能特色">功能特色</a> •
  <a href="#架構總覽">架構總覽</a> •
  <a href="#快速開始">快速開始</a> •
  <a href="#設定說明">設定說明</a> •
  <a href="#專案結構">專案結構</a> •
  <a href="#api-介面">API 介面</a> •
  <a href="#開發指南">開發指南</a>
</p>

</div>

---

## 專案簡介

KeyCompute 是一個**高性能**、**易擴充**、**開箱即用**的 AI Token 算力服務平台，提供統一的大模型接入、智慧路由、計量計費、算力節點租賃、多層分銷與可觀測性等企業級能力。

> **純 Rust 全棧**：後端 (Axum) + 前端 (Dioxus WASM) + CLI 客戶端，共享型別與邏輯，極致效能與安全。

> **注意**：本專案僅供個人學習使用，使用者必須在遵循 OpenAI [使用條款](https://openai.com/policies) 以及相關法律法規的前提下使用，不得用於非法用途。根據《生成式人工智慧服務管理暫行辦法》的要求，請勿向中國地區公眾提供任何未經備案的生成式人工智慧服務。

---

## 功能特色

### 算力節點租賃
算力節點透過**拉取式輪詢**接入成為計算節點，**無需公網 IP**，在本地硬體上運行託管模型，按貢獻獲取收益。

- **一鍵接入**：運行獨立 CLI 二進位即可自動註冊 → 心跳 → 輪詢任務 → 本地執行 → 提交結果
- **節點路由**：使用 `node:<模型名>` 顯式將請求路由到節點池
- **自動故障轉移**：失敗節點自動排除，任務重新入隊
- **會話持久化**：本地 Session 避免重複註冊，優雅關閉保障任務完整性
- **小費機制**：節點所有者可賺取並提現小費

### 統一多模型閘道
透過標準 **OpenAI API** 一行程式碼切換所有大模型：

| Provider | 模型系列 | 實現 |
|:---|:---|:---:|
| 🟢 OpenAI | GPT-4o / GPT-4 / GPT-3.5 等 | ✅ |
| 🟣 Anthropic | Claude 3.5 Sonnet / Opus / Haiku 等 | ✅ |
| 🔵 Google | Gemini 1.5 / 2.0 Flash / Pro 等 | ✅ |
| 🔴 DeepSeek | DeepSeek-V3 / R1 / Chat 等 | ✅ |
| 🟤 Ollama | 本地部署模型 (Llama / Qwen / GLM / MiniMax 等) | ✅ |
| 🟡 vLLM | 自部署任意模型 | ✅ |

> GLM（智譜）和 MiniMax 等可透過 Ollama 配接器本地部署執行，而非獨立 Provider 實現。

### 智慧路由引擎
**雙層路由架構**，多因子加權評分保障最優選擇：

```text
score = 0.30 × 成本因子 + 0.25 × 延遲因子 + 0.25 × 成功率 + 0.20 × 健康狀態
```

- **模型級路由** → **帳號池路由**：自動在 Provider 和帳號間擇優分配
- **回退鏈機制**：主目標失敗自動切換備用目標
- **指數退避重試**：最多 3 次重試，初始 100ms，最大 10s
- **請求級代理**：支援 Provider 級/帳號級/通配符級 HTTP 代理

### 計費與支付體系

- **流結束結算**：請求完成後精確計算，不預扣餘額，不影響執行結果
- **三層定價**：租戶特定定價 → 資料庫預設 → 硬編碼兜底（LRU 快取）
- **精確用量**：優先 Provider 精確 usage，回退 tiktoken 估算
- **線上儲值**：支付寶/微信支付 + 餘額管理
- **用量統計**：詳細的 Token 消耗明細與視覺化

### 二級分銷系統

- **推薦返傭**：預設一級 3% + 二級 2% 自動計算
- **邀請連結**：一鍵產生專屬邀請連結
- **靈活配置**：管理員可透過 API 配置分銷比例
- **收益統計**：即時檢視分銷收益與推薦列表

### 認證與權限

- **雙認證體系**：JWT（使用者工作階段）+ API Key（`sk-...`，API 存取）
- **權限分離**：API Key 即便有 admin 角色也無法存取管理介面
- **完整使用者管理**：註冊 → 郵箱驗證 → 登入 → 密碼重設 → 角色管理
- **分組限流**：使用者級/租戶級/API Key 級限流（記憶體 / Redis 雙後端）

### 可觀測性

- **Prometheus 指標**：請求量、延遲、錯誤率、Provider 健康度
- **分散式追蹤**：Provider Span / Request Span / Stream Span
- **結構化日誌**：JSON 格式，開發/生產分層輸出
- **主機監控**：CPU / 記憶體 / 磁碟 / 網路即時指標
- **健康檢查**：`/health` 介面一鍵監控服務狀態

### 跨平台前端

- **Web 管理後台**：Dioxus WASM SPA，9 個管理模組
- **桌面端**：Dioxus Desktop 原生應用
- **行動端**：Dioxus Mobile 跨平台支援
- **路由級權限控制**：Admin 角色驗證，安全可控

---

## 架構總覽

```text
[客戶端: Web / Desktop / Mobile (Dioxus)]
                ↕ HTTP/SSE
[API 層: keycompute-server (Axum)]
       ├── 認證 (JWT + API Key)
       ├── 限流 (記憶體/Redis)
       ├── 路由 (雙層引擎)
       └── Gateway (唯一上游執行層)
                ↕
[Provider 適配層]
  ├── OpenAI / Anthropic / Google
  ├── DeepSeek
  ├── Ollama (本地模型)
  └── vLLM (自部署)

[節點計算網路]
  node-token (CLI) ↔ node-gateway ↔ Redis 任務佇列 ↔ 本地推理
```

---

## 快速開始

### 環境需求

| 元件 | 版本要求 |
|:---|:---|
| Rust | ≥ 1.92 |
| Axum | ≥ 0.8.0 |
| Dioxus | ≥ 0.7.1 (前端開發) |
| PostgreSQL | ≥ 16 |
| Redis | ≥ 7 (選用，用於分散式限流/節點佇列) |
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

預設帳號：`admin@keycompute.local`，密碼：`change-me-admin-password`

> 正式環境請立即修改預設管理員密碼。

### 方式二：本機開發

> ⚠️ **安全警告**：下方顯示的預設值（`change-me-*`）僅用於示範。
> **切勿在生產環境中使用！** 請使用以下命令產生強隨機密碼：
> ```bash
> openssl rand -base64 32
> ```

```bash
# 建立網路
docker network create keycompute-internal

# PostgreSQL（使用 .env 中的密碼）
docker run -d \
  --name keycompute-postgres \
  --network keycompute-internal \
  -e POSTGRES_DB=keycompute \
  -e POSTGRES_USER=keycompute \
  -e POSTGRES_PASSWORD="${POSTGRES_PASSWORD:-change-me-strong-password}" \
  -p 5432:5432 \
  -v keycompute_postgres_data:/var/lib/postgresql/data \
  --restart unless-stopped \
  postgres:16-alpine

# Redis（選用，用於分散式限流和節點佇列）
docker run -d \
  --name keycompute-redis \
  --network keycompute-internal \
  -p 6379:6379 \
  -v keycompute_redis_data:/data \
  --restart unless-stopped \
  redis:7-alpine \
  redis-server \
  --requirepass "${REDIS_PASSWORD:-change-me-redis-password}" \
  --maxmemory 256mb \
  --maxmemory-policy allkeys-lru

# 安裝 dioxus-cli
curl -sSL http://dioxus.dev/install.sh | sh

# 載入環境變數（推薦使用 .env 檔案）
cp .env.example .env
# 編輯 .env 填入實際設定值
set -a && source .env && set +a

# 啟動後端
cargo run -p keycompute-server --features redis

# 啟動前端開發伺服器（另一個終端）
API_BASE_URL=http://localhost:3000 dx serve --package web --platform web --addr 0.0.0.0
```

---

## 專案結構

```text
keycompute/
├── crates/                          # 後端核心模組 (Rust)
│   ├── keycompute-server/            # Axum HTTP 服務（整合所有模組）
│   ├── keycompute-types/             # 全域共享型別與巨集
│   ├── keycompute-db/                # 資料庫 ORM（23 張表）
│   ├── keycompute-auth/              # 認證與鑑權（JWT + API Key + 密碼）
│   ├── keycompute-ratelimit/         # 限流引擎（記憶體/Redis 雙後端）
│   ├── keycompute-pricing/           # 定價引擎（三層兜底 + LRU 快取）
│   ├── keycompute-routing/           # 雙層智慧路由引擎
│   ├── keycompute-runtime/           # 執行時（AES-256-GCM 加密 + 儲存抽象）
│   ├── keycompute-billing/           # 計費結算（流結束後精確結算）
│   ├── keycompute-distribution/      # 二級分銷系統
│   ├── keycompute-observability/     # 可觀測性三大支柱
│   ├── keycompute-config/            # 設定管理（環境變數 + TOML）
│   ├── keycompute-emailserver/       # SMTP 郵件服務
│   ├── keycompute-payment/           # 支付整合
│   │   ├── keycompute-alipay/        # 支付寶支付
│   │   └── keycompute-wechatpay/     # 微信支付
│   ├── llm-gateway/                  # LLM 執行閘道（唯一上游層）
│   ├── llm-provider/                 # Provider 適配器
│   │   ├── keycompute-openai/        # OpenAI
│   │   ├── keycompute-claude/        # Anthropic Claude
│   │   ├── keycompute-gemini/        # Google Gemini
│   │   ├── keycompute-deepseek/      # DeepSeek
│   │   ├── keycompute-ollama/        # Ollama 本地模型
│   │   └── keycompute-vllm/          # vLLM 自部署
│   ├── node-gateway/                 # 節點閘道（註冊/心跳/任務管理）
│   └── integration-tests/           # 端到端整合測試（30+ 場景）
├── packages/                         # 前端 (Dioxus 0.7)
│   ├── web/                          # Web 管理後台（9 個管理模組）
│   ├── ui/                           # 共享 UI 元件庫
│   ├── desktop/                      # 桌面端原生應用
│   ├── mobile/                       # 行動端跨平台應用
│   └── client-api/                   # API 用戶端封裝（17 個模組）
├── nginx/                            # Nginx 反向代理設定
├── Dockerfile.server                 # 後端容器映像
├── Dockerfile.web                    # 前端容器映像
└── docker-compose.yml                # 容器編排
```

---

## 設定說明

### 環境變數

| 變數名 | 說明 | 必填 |
|:---|:---|:---:|
| `KC__DATABASE__URL` | PostgreSQL 連線字串 | ✅ |
| `KC__AUTH__JWT_SECRET` | JWT 簽名金鑰 | ✅ |
| `KC__CRYPTO__SECRET_KEY` | API Key AES-256-GCM 加密金鑰（寫入後不可更改） | ✅ |
| `KC__NODE_GATEWAY__REGISTRATION_TOKEN_SECRET` | HMAC 簽名金鑰；用於簽發一次性節點註冊 token | ✅ |
| `KC__REDIS__URL` | Redis 連線字串（選用，透過 `--features redis` 啟用） | ⚪ |
| `KC__EMAIL__SMTP_HOST` | SMTP 伺服器位址（選用） | ⚪ |
| `KC__EMAIL__SMTP_PORT` | SMTP 伺服器連接埠（選用） | ⚪ |
| `KC__EMAIL__SMTP_USERNAME` | SMTP 使用者名稱（選用） | ⚪ |
| `KC__EMAIL__SMTP_PASSWORD` | SMTP 密碼（選用） | ⚪ |
| `KC__EMAIL__FROM_ADDRESS` | 寄件者電子郵件地址（選用） | ⚪ |
| `KC__EMAIL__FROM_NAME` | 寄件者顯示名稱（選用） | ⚪ |
| `APP_BASE_URL` | 公開前端地址（密碼重設/邀請連結必需） | ⚪ |
| `KC__DEFAULT_ADMIN_EMAIL` | 預設管理員電子郵件 | ⚪ |
| `KC__DEFAULT_ADMIN_PASSWORD` | 預設管理員密碼 | ⚪ |

---

## API 介面

### OpenAI 相容 API

```bash
# Chat Completions（串流 + 非串流）
curl http://localhost:3000/v1/chat/completions \
  -H "Authorization: Bearer sk-xxx" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "deepseek-chat",
    "messages": [{"role": "user", "content": "Hello!"}],
    "stream": true
  }'

# 列出可用模型
curl http://localhost:3000/v1/models \
  -H "Authorization: Bearer sk-xxx"
```

### 管理 API 概覽

| 分類 | 介面 | 說明 |
|:---|:---|:---|
| 認證 | `POST /api/v1/auth/register` | 使用者註冊 |
| | `POST /api/v1/auth/login` | 使用者登入 |
| | `POST /api/v1/auth/forgot-password` | 忘記密碼 |
| 使用者 | `GET /api/v1/me` | 目前使用者資訊 |
| | `GET/POST /api/v1/keys` | API Key 管理 |
| 計費 | `GET /api/v1/usage` | 用量統計 |
| | `GET /api/v1/billing/records` | 帳單記錄 |
| 支付 | `POST /api/v1/payments/orders` | 建立支付訂單 |
| | `GET /api/v1/payments/balance` | 餘額查詢 |
| 分銷 | `GET /api/v1/me/distribution/earnings` | 分銷收益 |
| 節點 | `GET /api/v1/me/node-gateway/token` | 節點令牌 |
| | `GET /api/v1/me/tips` | 小費摘要 |
| 管理 | `GET/POST /api/v1/accounts` | 上游帳號管理 |
| | `GET/POST /api/v1/settings` | 系統設定 |
| | `GET/POST /api/v1/pricing` | 定價管理 |
| | `GET /api/v1/admin/monitoring/overview` | 監控概覽 |

> 完整的 API 文件請參考專案原始碼中的路由定義。

---

## 開發指南

```bash
# 編譯（排除桌面端和行動端）
cargo build --workspace --exclude desktop --exclude mobile --verbose

# 執行單元測試
cargo test --lib --workspace --exclude desktop --exclude mobile --verbose

# 執行整合測試
cargo test --package integration-tests --tests --verbose

# 執行前端 API 用戶端測試
cargo test --package client-api --tests --verbose

# Clippy 程式碼檢查
cargo clippy --workspace --exclude desktop --exclude mobile --all-targets --all-features --future-incompat-report -- -D warnings

# 程式碼格式化檢查
cargo fmt --all --check

# 啟用 Redis 後端構建
cargo build -p keycompute-server --features redis
```

---

## 如何貢獻

我們歡迎各種形式的貢獻！請閱讀 [CONTRIBUTING.md](CONTRIBUTING.md) 了解如何參與專案開發。

- 🐛 [回報 Bug](https://github.com/aiqubits/keycompute/issues/new?template=bug_report.yml)
- 💡 [功能建議](https://github.com/aiqubits/keycompute/issues/new?template=feature_request.yml)
- 🔧 [提交程式碼](CONTRIBUTING.md)

---

## 授權條款

本專案採用 [MIT](LICENSE) 授權條款開源。

---

<div align="center">

### 💖 感謝使用 KeyCompute

如果這個專案對你有幫助，歡迎給我們一個 ⭐️ Star！

**[快速開始](#快速開始)** • **[問題回報](https://github.com/aiqubits/keycompute/issues)** • **[最新版本](https://github.com/aiqubits/keycompute/releases)**

</div>
