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

**Plataforma de servicios de cómputo de tokens de IA de nueva generación y alto rendimiento**

<p align="center">
  <a href="#características">Características</a> •
  <a href="#inicio-rápido">Inicio rápido</a> •
  <a href="#configuración">Configuración</a> •
  <a href="#estructura-del-proyecto">Estructura del proyecto</a>
</p>

</div>

---

## Descripción general

KeyCompute es una plataforma de servicios de cómputo de tokens de IA **de alto rendimiento**, **extensible** y **lista para usar**. Proporciona capacidades de nivel empresarial como acceso unificado a LLMs, enrutamiento inteligente, medición y facturación, distribución multinivel y observabilidad.

> **Nota**: Este proyecto es solo para aprendizaje personal. Debe utilizarse de acuerdo con los [Términos de uso](https://openai.com/policies) de OpenAI y con las leyes y normativas aplicables. No lo utilice para fines ilegales. De conformidad con las Medidas Provisionales para la Administración de Servicios de Inteligencia Artificial Generativa, no proporcione servicios de IA generativa no registrados al público en China.

---

## Características

### Soporte multimodelo

Accede a todos los modelos principales mediante el formato estándar de **OpenAI API** desde el primer momento:

| Provider | Familias de modelos | Estado |
|:---|:---|:---:|
| 🟢 OpenAI | GPT-5/GPT-4/GPT-4o/... | ✅ |
| 🟣 Anthropic | Claude 4/3.7/3.5/... | ✅ |
| 🔵 Google | Gemini 3/2.5/2.0/... | ✅ |
| 🔴 DeepSeek | DeepSeek-V4/V3/R1/... | ✅ |
| 🟠 Zhipu | GLM-5.1/5/4.7/... | ✅ |
| 🔴 MiniMax | MiniMax-M2.7/M2.5/... | ✅ |
| 🟤 Ollama | Modelos locales (Llama/Qwen/...) | ✅ |
| 🟡 vLLM | Modelos autohospedados | ✅ |

### Enrutamiento inteligente

- **Motor de enrutamiento de dos capas**: enrutamiento a nivel de modelo + enrutamiento de pool de cuentas
- **Balanceo de carga**: asignación aleatoria ponderada entre múltiples cuentas
- **Reintento automático ante fallos**: cambia de canal automáticamente cuando una solicitud falla
- **Comprobaciones de salud**: monitoriza la disponibilidad del provider en tiempo real

### Facturación y pagos

- **Facturación en tiempo real**: instantáneas de precios por solicitud con liquidación precisa posterior
- **Recargas en línea**: Alipay y WeChat Pay
- **Analítica de uso**: desglose detallado del consumo de tokens
- **Gestión de saldo**: seguimiento de recargas y consumo

### Distribución por referidos

- **Recompensas por recomendación**: gana recompensas al invitar nuevos usuarios
- **Reglas de distribución**: configura de forma flexible los porcentajes de comisión
- **Analítica de ingresos**: consulta las ganancias por referidos en tiempo real
- **Enlaces de invitación**: genera enlaces exclusivos con un clic

### Usuarios y permisos

- **Soporte multiusuario**: registro, inicio de sesión y gestión de permisos
- **Códigos por correo**: códigos de registro y restablecimiento de contraseña
- **Gestión de API keys**: crear, eliminar y ver API keys
- **Límite por grupos**: limitación de solicitudes a nivel de usuario

### Observabilidad

- **Métricas de Prometheus**: volumen de solicitudes, latencia y tasa de errores
- **Logs estructurados**: logs en formato JSON para facilitar el análisis
- **Endpoint de salud**: `/health` para monitorizar el estado del servicio

---

## Inicio rápido

### Requisitos

| Componente | Versión requerida |
|:---|:---|
| Rust | ≥ 1.92 |
| Axum | ≥ 0.8.0 |
| Dioxus | ≥ 0.7.1 (desarrollo frontend) |
| PostgreSQL | ≥ 16 |
| Redis | ≥ 7 (opcional, para limitación distribuida) |
| Docker | Última versión (despliegue en contenedores) |

### Opción 1: despliegue con Docker Compose (recomendado)

```bash
# Clonar el proyecto
git clone https://github.com/your-org/keycompute.git
cd keycompute

# Copiar y editar las variables de entorno
cp .env.example .env
# Edita .env y completa la configuración real

# Iniciar todos los servicios
docker compose up -d

# Comprobar el estado de los servicios
docker compose ps
```

Después del despliegue, abre `http://localhost:8080` para comenzar.

Cuenta predeterminada: `admin@keycompute.local`, contraseña: `12345`

> Cambia inmediatamente la contraseña predeterminada del administrador en producción.

### Opción 2: desarrollo local

```bash
# Crear la red
docker network create keycompute-internal

# PostgreSQL (usando la contraseña de .env)
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

# Redis (usando la contraseña de .env)
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

# Instalar dioxus-cli
curl -sSL http://dioxus.dev/install.sh | sh

# Iniciar el servicio backend
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

# Iniciar el servidor de desarrollo frontend (en otra terminal)
API_BASE_URL=http://localhost:3000 dx serve --package web --platform web --addr 0.0.0.0
```

---

## Estructura del proyecto

```text
keycompute/
├── crates/                    # Módulos principales del backend (Rust)
│   ├── keycompute-server/      # Servicio HTTP con Axum
│   ├── keycompute-types/       # Tipos compartidos
│   ├── keycompute-db/          # Capa de acceso a base de datos
│   ├── keycompute-auth/        # Autenticación y autorización
│   ├── keycompute-ratelimit/   # Limitación distribuida
│   ├── keycompute-pricing/     # Motor de precios
│   ├── keycompute-routing/     # Enrutamiento inteligente
│   ├── keycompute-runtime/     # Estado en tiempo de ejecución
│   ├── keycompute-billing/     # Facturación y liquidación
│   ├── keycompute-distribution/# Distribución por referidos
│   ├── keycompute-observability/# Observabilidad
│   ├── keycompute-config/      # Gestión de configuración
│   ├── keycompute-emailserver/ # Servicio de correo
│   ├── llm-gateway/            # Gateway de ejecución LLM
│   └── llm-provider/           # Adaptadores de providers
│       ├── keycompute-openai/  # OpenAI/Claude/Gemini
│       ├── keycompute-deepseek/# DeepSeek
│       ├── keycompute-ollama/  # Modelos locales de Ollama
│       └── keycompute-vllm/    # Modelos autohospedados con vLLM
├── packages/                   # Frontend (Dioxus 0.7)
│   ├── web/                    # Panel de administración web
│   ├── ui/                     # Componentes UI compartidos
│   └── client-api/             # Cliente API
├── nginx/                      # Configuración de Nginx
├── Dockerfile.server           # Imagen del backend
├── Dockerfile.web              # Imagen del frontend
└── docker-compose.yml          # Orquestación de contenedores
```

---

## Configuración

### Variables de entorno

Variables de entorno principales:

| Variable | Descripción | Obligatoria |
|:---|:---|:---:|
| `KC__DATABASE__URL` | Cadena de conexión de PostgreSQL | ✅ |
| `KC__REDIS__URL` | Cadena de conexión de Redis | ⚪ |
| `KC__AUTH__JWT_SECRET` | Secreto de firma JWT | ✅ |
| `KC__CRYPTO__SECRET_KEY` | Secreto de cifrado para API keys | ✅ |
| `KC__EMAIL__SMTP_HOST` | Host SMTP | ⚪ |
| `KC__EMAIL__SMTP_PORT` | Puerto SMTP | ⚪ |
| `KC__EMAIL__SMTP_USERNAME` | Usuario SMTP | ⚪ |
| `KC__EMAIL__SMTP_PASSWORD` | Contraseña SMTP | ⚪ |
| `KC__EMAIL__FROM_ADDRESS` | Dirección de correo del remitente | ⚪ |
| `KC__EMAIL__FROM_NAME` | Nombre visible del remitente | ⚪ |
| `APP_BASE_URL` | URL pública base del frontend para enlaces de restablecimiento e invitación; si falta, se usa como fallback la inferencia desde headers reenviados de confianza | ⚪ |
| `KC__DEFAULT_ADMIN_EMAIL` | Correo del administrador por defecto | ⚪ |
| `KC__DEFAULT_ADMIN_PASSWORD` | Contraseña del administrador por defecto | ⚪ |

> 💡 Consejo: una vez que se hayan escrito datos en la base de datos, `KC__CRYPTO__SECRET_KEY` no debe cambiarse, o los datos históricos dejarán de poder descifrarse.

---

## API

### API compatible con OpenAI

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
# Ejemplo
curl -s http://192.168.100.100:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-329939d678d24433bc0277311c576481bc23b86ebc724354" \
  -d '{"model":"deepseek-chat","messages":[{"role":"user","content":"hello"}],"stream":true}'

data: {"id":"chatcmpl-7370f2606a6a4f5fa516fe54d9196c9d-kc","object":"chat.completion.chunk","created":1775231430,"model":"deepseek-chat","system_fingerprint":"fp_deepseek","choices":[{"index":0,"delta":{"role":"assistant","content":"¡Hola! 👋 ¡Encantado de conocerte!\n¿Cómo puedo ayudarte hoy?"},"finish_reason":null}]}
```

```bash
# Listar modelos
curl https://your-domain/v1/models \
  -H "Authorization: Bearer sk-xxx"
```

### API de administración

| Endpoint | Descripción |
|:---|:---|
| `POST /api/v1/auth/register` | Registro de usuario |
| `POST /api/v1/auth/login` | Inicio de sesión |
| `GET /api/v1/me` | Obtener el usuario actual |
| `GET /api/v1/keys` | Listar mis API keys |
| `POST /api/v1/keys` | Crear una API key |
| `GET /api/v1/usage` | Estadísticas de uso |
| `GET /api/v1/billing/records` | Registros de facturación |
| `POST /api/v1/payments/orders` | Crear una orden de pago |

---

## Guía de desarrollo

```bash
# Compilar
cargo build --workspace --exclude desktop --exclude mobile --verbose

# Ejecutar pruebas
cargo test --lib --workspace --exclude desktop --exclude mobile --verbose
cargo test --package client-api --tests --verbose
cargo test --package integration-tests --tests --verbose

# Verificaciones de código
cargo clippy --workspace --exclude desktop --exclude mobile --all-targets --all-features --future-incompat-report -- -D warnings
cargo fmt --all --check

# Habilitar el backend de Redis
cargo build -p keycompute-server --features redis
```

---

# Contribuciones

Damos la bienvenida a todo tipo de contribuciones. Consulta [CONTRIBUTING.md](CONTRIBUTING.md) para saber cómo participar.

- 🐛 [Reportar bugs](https://github.com/aiqubits/keycompute/issues/new?template=bug_report.yml)
- 💡 [Solicitar funcionalidades](https://github.com/aiqubits/keycompute/issues/new?template=feature_request.yml)
- 🔧 [Enviar código](CONTRIBUTING.md)

---

# Licencia

Este proyecto se distribuye bajo la licencia [MIT](LICENSE).

---

<div align="center">

### 💖 Gracias por usar KeyCompute

Si este proyecto te resulta útil, te agradeceremos una ⭐️.

**[Inicio rápido](#inicio-rápido)** • **[Reportar problemas](https://github.com/aiqubits/keycompute/issues)** • **[Últimas versiones](https://github.com/aiqubits/keycompute/releases)**

</div>
