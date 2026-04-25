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

**منصة خدمات حوسبة رموز الذكاء الاصطناعي من الجيل التالي وعالية الأداء**

<p align="center">
  <a href="https://github.com/keycompute/keycompute/stargazers"><img src="https://img.shields.io/github/stars/keycompute/keycompute?style=social" alt="GitHub Stars" /></a>
  <a href="https://github.com/keycompute/keycompute/issues"><img src="https://img.shields.io/github/issues/keycompute/keycompute" alt="GitHub Issues" /></a>
  <a href="./LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="MIT License" /></a>
  <a href="./CONTRIBUTING.md"><img src="https://img.shields.io/badge/PRs-welcome-brightgreen" alt="PRs Welcome" /></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-1.92%2B-orange?logo=rust" alt="Rust Version" /></a>
</p>

<p align="center">
  <a href="#الميزات">الميزات</a> •
  <a href="#البدء-السريع">البدء السريع</a> •
  <a href="#الإعدادات">الإعدادات</a> •
  <a href="#هيكل-المشروع">هيكل المشروع</a>
</p>

</div>

---

## نظرة عامة

KeyCompute هي منصة خدمات حوسبة رموز ذكاء اصطناعي **عالية الأداء** و**قابلة للتوسعة** و**جاهزة للاستخدام مباشرة**. توفر قدرات على مستوى المؤسسات تشمل الوصول الموحد إلى النماذج الكبيرة، والتوجيه الذكي، والقياس والفوترة، والتوزيع متعدد المستويات، وقابلية المراقبة.

> **ملاحظة**: هذا المشروع مخصص للتعلم الشخصي فقط. يجب استخدامه بما يتوافق مع [شروط استخدام](https://openai.com/policies) OpenAI ومع القوانين واللوائح المعمول بها. لا تستخدمه لأغراض غير قانونية. ووفقًا للتدابير المؤقتة لإدارة خدمات الذكاء الاصطناعي التوليدي، لا يجوز تقديم أي خدمات ذكاء اصطناعي توليدي غير مسجلة لعامة الجمهور في الصين.

---

## الميزات

### دعم نماذج متعددة

يمكنك الوصول إلى جميع النماذج الرئيسية من خلال تنسيق **OpenAI API** القياسي مباشرة:

| Provider | عائلات النماذج | الحالة |
|:---|:---|:---:|
| 🟢 OpenAI | GPT-5/GPT-4/GPT-4o/... | ✅ |
| 🟣 Anthropic | Claude 4/3.7/3.5/... | ✅ |
| 🔵 Google | Gemini 3/2.5/2.0/... | ✅ |
| 🔴 DeepSeek | DeepSeek-V4/V3/R1/... | ✅ |
| 🟠 Zhipu | GLM-5.1/5/4.7/... | ✅ |
| 🔴 MiniMax | MiniMax-M2.7/M2.5/... | ✅ |
| 🟤 Ollama | نماذج محلية (Llama/Qwen/...) | ✅ |
| 🟡 vLLM | نماذج مستضافة ذاتيًا | ✅ |

### التوجيه الذكي

- **محرك توجيه ثنائي الطبقات**: توجيه على مستوى النموذج + توجيه على مستوى مجمع الحسابات
- **موازنة الحمل**: توزيع عشوائي مرجح عبر حسابات متعددة
- **إعادة المحاولة التلقائية عند الفشل**: التبديل التلقائي إلى قناة أخرى عند فشل الطلب
- **فحوصات الصحة**: مراقبة توفر مزود الخدمة في الوقت الفعلي

### الفوترة والمدفوعات

- **فوترة آنية**: لقطات سعرية على مستوى الطلب مع تسوية دقيقة لاحقًا
- **شحن الرصيد عبر الإنترنت**: Alipay و WeChat Pay
- **تحليلات الاستخدام**: تفصيل دقيق لاستهلاك الرموز
- **إدارة الرصيد**: تتبع الشحن والاستهلاك

### نظام الإحالة والتوزيع

- **مكافآت الإحالة**: احصل على مكافآت عند دعوة مستخدمين جدد
- **قواعد التوزيع**: إعداد نسب العمولات بمرونة
- **تحليلات الإيرادات**: عرض أرباح الإحالات في الوقت الفعلي
- **روابط الدعوة**: إنشاء روابط دعوة حصرية بنقرة واحدة

### المستخدمون والصلاحيات

- **دعم متعدد المستخدمين**: التسجيل وتسجيل الدخول وإدارة الصلاحيات
- **رموز البريد الإلكتروني**: رموز التسجيل وإعادة تعيين كلمة المرور
- **إدارة مفاتيح API**: إنشاء وحذف وعرض مفاتيح API
- **تحديد المعدل حسب المجموعات**: تقييد الطلبات على مستوى المستخدم

### قابلية المراقبة

- **مقاييس Prometheus**: حجم الطلبات وزمن الاستجابة ومعدل الأخطاء
- **سجلات منظمة**: سجلات JSON لتسهيل التحليل
- **نقطة فحص الصحة**: `/health` لمراقبة حالة الخدمة

---

## البدء السريع

### المتطلبات

| المكوّن | الإصدار المطلوب |
|:---|:---|
| Rust | ≥ 1.92 |
| Axum | ≥ 0.8.0 |
| Dioxus | ≥ 0.7.1 (لتطوير الواجهة الأمامية) |
| PostgreSQL | ≥ 16 |
| Redis | ≥ 7 (اختياري، لتحديد المعدل الموزع) |
| Docker | أحدث إصدار (للنشر عبر الحاويات) |

### الخيار 1: النشر عبر Docker Compose (موصى به)

```bash
# استنساخ المشروع
git clone https://github.com/your-org/keycompute.git
cd keycompute

# نسخ متغيرات البيئة وتعديلها
cp .env.example .env
# عدل ملف .env وأدخل القيم الفعلية

# تشغيل جميع الخدمات
docker compose up -d

# التحقق من حالة الخدمات
docker compose ps
```

بعد النشر، افتح `http://localhost:8080` للبدء.

الحساب الافتراضي: `admin@keycompute.local`، كلمة المرور: `12345`

> غيّر كلمة مرور المدير الافتراضية فورًا في بيئة الإنتاج.

### الخيار 2: التطوير المحلي

```bash
# إنشاء الشبكة
docker network create keycompute-internal

# PostgreSQL (باستخدام كلمة المرور من .env)
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

# Redis (باستخدام كلمة المرور من .env)
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

# تثبيت dioxus-cli
curl -sSL http://dioxus.dev/install.sh | sh

# تشغيل خدمة الخلفية
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

# تشغيل خادم تطوير الواجهة الأمامية (في طرفية أخرى)
API_BASE_URL=http://localhost:3000 dx serve --package web --platform web --addr 0.0.0.0
```

---

## هيكل المشروع

```text
keycompute/
├── crates/                    # الوحدات الأساسية للخلفية (Rust)
│   ├── keycompute-server/      # خدمة HTTP مبنية على Axum
│   ├── keycompute-types/       # أنواع مشتركة
│   ├── keycompute-db/          # طبقة الوصول إلى قاعدة البيانات
│   ├── keycompute-auth/        # المصادقة والتفويض
│   ├── keycompute-ratelimit/   # تحديد المعدل الموزع
│   ├── keycompute-pricing/     # محرك التسعير
│   ├── keycompute-routing/     # التوجيه الذكي
│   ├── keycompute-runtime/     # حالة وقت التشغيل
│   ├── keycompute-billing/     # الفوترة والتسوية
│   ├── keycompute-distribution/# نظام الإحالة والتوزيع
│   ├── keycompute-observability/# قابلية المراقبة
│   ├── keycompute-config/      # إدارة الإعدادات
│   ├── keycompute-emailserver/ # خدمة البريد الإلكتروني
│   ├── llm-gateway/            # بوابة تنفيذ LLM
│   └── llm-provider/           # موائمات مزودي الخدمة
│       ├── keycompute-openai/  # OpenAI/Claude/Gemini
│       ├── keycompute-deepseek/# DeepSeek
│       ├── keycompute-ollama/  # نماذج Ollama المحلية
│       └── keycompute-vllm/    # نماذج vLLM المستضافة ذاتيًا
├── packages/                   # الواجهة الأمامية (Dioxus 0.7)
│   ├── web/                    # لوحة الإدارة عبر الويب
│   ├── ui/                     # مكونات UI المشتركة
│   └── client-api/             # عميل API
├── nginx/                      # إعدادات Nginx
├── Dockerfile.server           # صورة الخلفية
├── Dockerfile.web              # صورة الواجهة الأمامية
└── docker-compose.yml          # تنسيق الحاويات
```

---

## الإعدادات

### متغيرات البيئة

أهم متغيرات البيئة:

| المتغير | الوصف | مطلوب |
|:---|:---|:---:|
| `KC__DATABASE__URL` | سلسلة اتصال PostgreSQL | ✅ |
| `KC__REDIS__URL` | سلسلة اتصال Redis | ⚪ |
| `KC__AUTH__JWT_SECRET` | سر توقيع JWT | ✅ |
| `KC__CRYPTO__SECRET_KEY` | سر تشفير مفاتيح API | ✅ |
| `KC__EMAIL__SMTP_HOST` | مضيف SMTP | ⚪ |
| `KC__EMAIL__SMTP_PORT` | منفذ SMTP | ⚪ |
| `KC__EMAIL__SMTP_USERNAME` | اسم مستخدم SMTP | ⚪ |
| `KC__EMAIL__SMTP_PASSWORD` | كلمة مرور SMTP | ⚪ |
| `KC__EMAIL__FROM_ADDRESS` | عنوان بريد المرسل | ⚪ |
| `KC__EMAIL__FROM_NAME` | الاسم الظاهر للمرسل | ⚪ |
| `APP_BASE_URL` | العنوان العام لواجهة التطبيق لروابط إعادة التعيين والدعوات؛ ويجب ضبطه صراحةً عند تفعيل البريد أو روابط الدعوة | ⚪ |
| `KC__DEFAULT_ADMIN_EMAIL` | بريد المدير الافتراضي | ⚪ |
| `KC__DEFAULT_ADMIN_PASSWORD` | كلمة مرور المدير الافتراضية | ⚪ |

> 💡 تلميح: بمجرد كتابة بيانات في قاعدة البيانات، يجب عدم تغيير `KC__CRYPTO__SECRET_KEY`، وإلا فلن يمكن فك تشفير البيانات التاريخية.

---

## API

### API متوافقة مع OpenAI

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
# مثال
curl -s http://192.168.100.100:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-329939d678d24433bc0277311c576481bc23b86ebc724354" \
  -d '{"model":"deepseek-chat","messages":[{"role":"user","content":"hello"}],"stream":true}'

data: {"id":"chatcmpl-7370f2606a6a4f5fa516fe54d9196c9d-kc","object":"chat.completion.chunk","created":1775231430,"model":"deepseek-chat","system_fingerprint":"fp_deepseek","choices":[{"index":0,"delta":{"role":"assistant","content":"مرحبًا! 👋 سعدت بلقائك!\nكيف يمكنني مساعدتك اليوم؟"},"finish_reason":null}]}
```

```bash
# عرض النماذج
curl https://your-domain/v1/models \
  -H "Authorization: Bearer sk-xxx"
```

### API الإدارة

| Endpoint | الوصف |
|:---|:---|
| `POST /api/v1/auth/register` | تسجيل مستخدم |
| `POST /api/v1/auth/login` | تسجيل الدخول |
| `GET /api/v1/me` | الحصول على المستخدم الحالي |
| `GET /api/v1/keys` | عرض مفاتيح API الخاصة بي |
| `POST /api/v1/keys` | إنشاء مفتاح API |
| `GET /api/v1/usage` | إحصاءات الاستخدام |
| `GET /api/v1/billing/records` | سجلات الفوترة |
| `POST /api/v1/payments/orders` | إنشاء طلب دفع |

---

## دليل التطوير

```bash
# البناء
cargo build --workspace --exclude desktop --exclude mobile --verbose

# تشغيل الاختبارات
cargo test --lib --workspace --exclude desktop --exclude mobile --verbose
cargo test --package client-api --tests --verbose
cargo test --package integration-tests --tests --verbose

# فحوصات الكود
cargo clippy --workspace --exclude desktop --exclude mobile --all-targets --all-features --future-incompat-report -- -D warnings
cargo fmt --all --check

# تفعيل خلفية Redis
cargo build -p keycompute-server --features redis
```

---

# المساهمة

نرحب بجميع أنواع المساهمات. يرجى مراجعة [CONTRIBUTING.md](CONTRIBUTING.md) لمعرفة كيفية المشاركة.

- 🐛 [الإبلاغ عن الأخطاء](https://github.com/aiqubits/keycompute/issues/new?template=bug_report.yml)
- 💡 [طلب ميزات جديدة](https://github.com/aiqubits/keycompute/issues/new?template=feature_request.yml)
- 🔧 [إرسال الكود](CONTRIBUTING.md)

---

# الترخيص

هذا المشروع متاح بموجب ترخيص [MIT](LICENSE).

---

<div align="center">

### 💖 شكرًا لاستخدام KeyCompute

إذا كان هذا المشروع مفيدًا لك، فسنكون ممتنين لمنحه ⭐️.

**[البدء السريع](#البدء-السريع)** • **[الإبلاغ عن المشكلات](https://github.com/aiqubits/keycompute/issues)** • **[أحدث الإصدارات](https://github.com/aiqubits/keycompute/releases)**

</div>
