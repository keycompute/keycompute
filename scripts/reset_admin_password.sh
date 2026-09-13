#!/usr/bin/env bash
# =============================================================================
# reset_admin_password.sh — KeyCompute 管理员密码一键重置脚本
#
# 使用方法：
#   chmod +x reset_admin_password.sh
#   sudo ./reset_admin_password.sh
#
# 前提条件：
#   - 通过本项目 Docker Compose 启动的 PostgreSQL 正在运行
#   - Python3 可用（用于生成 Argon2id 密码哈希）
#
# 可选覆盖（自定义 Compose 项目或容器时使用）：
#   KC_RESET_DB_CONTAINER=<container name or id>
#   KC_RESET_DB_USER=<postgres user>
#   KC_RESET_DB_NAME=<database name>
#   KC_RESET_DB_PASSWORD=<database password, optional; defaults to POSTGRES_PASSWORD in the container>
#   KC_RESET_ADMIN_EMAIL=<system account email>
#   KC_RESET_ADMIN_NAME=<system account display name>
# =============================================================================

set -euo pipefail

# ── 颜色定义 ────────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# ── 配置 ─────────────────────────────────────────────────────────────────────
DB_CONTAINER_OVERRIDE="${KC_RESET_DB_CONTAINER:-}"
DB_USER_OVERRIDE="${KC_RESET_DB_USER:-}"
DB_NAME_OVERRIDE="${KC_RESET_DB_NAME:-}"
DB_PASSWORD_OVERRIDE="${KC_RESET_DB_PASSWORD:-}"

# 默认管理员参数（用于脚本自动补齐）。密码按产品约定固定为 12345，脚本不读取 stdin。
DEFAULT_ADMIN_EMAIL="${KC_RESET_ADMIN_EMAIL:-${KC__DEFAULT_ADMIN_EMAIL:-admin@keycompute.local}}"
DEFAULT_ADMIN_NAME="${KC_RESET_ADMIN_NAME:-System Administrator}"
DEFAULT_PASSWORD="12345"
PYTHON_BIN="python3"
ARGON2_VENV=""

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
PROJECT_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd -P)"

# ── 工具函数 ─────────────────────────────────────────────────────────────────

info()  { echo -e "${BLUE}[INFO]${NC} $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*"; }
ok()    { echo -e "${GREEN}[OK]${NC} $*"; }

container_is_running() {
    [ "$(docker inspect --format '{{.State.Running}}' "$1" 2>/dev/null || true)" = "true" ]
}

container_display_name() {
    local name
    name="$(docker inspect --format '{{.Name}}' "$1" 2>/dev/null || true)"
    name="${name#/}"
    printf '%s\n' "${name:-$1}"
}

container_env_value() {
    local container="$1"
    local key="$2"

    docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "${container}" 2>/dev/null \
        | awk -v key="${key}" 'index($0, key "=") == 1 { sub("^[^=]*=", ""); print; exit }'
}

generate_password_hash() {
    KC_RESET_PASSWORD_INPUT="$1" "${PYTHON_BIN}" <<'PYEOF'
import os
import sys
from argon2 import PasswordHasher, Type

password = os.environ["KC_RESET_PASSWORD_INPUT"]
ph = PasswordHasher(
    time_cost=3,
    memory_cost=65536,
    parallelism=4,
    hash_len=32,
    type=Type.ID,
)

try:
    hash_str = ph.hash(password)
    ph.verify(hash_str, password)
except Exception as error:
    print(f"[ERROR] 密码哈希生成或验证失败: {error}", file=sys.stderr)
    sys.exit(1)

print(hash_str)
PYEOF
}

cleanup_argon2_venv() {
    if [ -n "${ARGON2_VENV}" ] && [ -d "${ARGON2_VENV}" ]; then
        rm -rf -- "${ARGON2_VENV}"
    fi
}

normalize_admin_email() {
    KC_RESET_ADMIN_EMAIL_INPUT="$1" "${PYTHON_BIN}" <<'PYEOF'
import os
import re
import sys

email = os.environ["KC_RESET_ADMIN_EMAIL_INPUT"].strip().lower()
if len(email) > 255 or not re.fullmatch(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+[.][a-zA-Z]{2,}", email):
    print("管理员邮箱格式无效（必须与应用登录校验一致）", file=sys.stderr)
    sys.exit(1)
print(email)
PYEOF
}

resolve_database_container() {
    local container_id

    # 显式覆盖始终优先，适用于非 Compose 或自定义容器名部署。
    if [ -n "${DB_CONTAINER_OVERRIDE}" ]; then
        if container_is_running "${DB_CONTAINER_OVERRIDE}"; then
            printf '%s\n' "${DB_CONTAINER_OVERRIDE}"
            return 0
        fi
        error "指定的数据库容器 ${DB_CONTAINER_OVERRIDE} 未运行" >&2
        return 1
    fi

    # 优先让 Compose 解析当前项目的实际容器 ID。普通编排的服务名为
    # postgres，主从编排的写库服务名为 postgres-primary。
    container_id="$(
        docker compose --project-directory "${PROJECT_ROOT}" \
            -f "${PROJECT_ROOT}/docker-compose.yml" \
            ps --status running -q postgres 2>/dev/null | head -n 1 || true
    )"
    if [ -n "${container_id}" ] && container_is_running "${container_id}"; then
        printf '%s\n' "${container_id}"
        return 0
    fi

    container_id="$(
        docker compose --project-directory "${PROJECT_ROOT}" \
            -f "${PROJECT_ROOT}/docker-compose.replicas.yml" \
            ps --status running -q postgres-primary 2>/dev/null | head -n 1 || true
    )"
    if [ -n "${container_id}" ] && container_is_running "${container_id}"; then
        printf '%s\n' "${container_id}"
        return 0
    fi

    # 支持 `docker compose -p <custom>`：通过 Compose 工作目录和服务标签限定
    # 当前项目，避免误选同机其他项目的 PostgreSQL。
    for service in postgres postgres-primary; do
        container_id="$(
            docker ps \
                --filter "label=com.docker.compose.project.working_dir=${PROJECT_ROOT}" \
                --filter "label=com.docker.compose.service=${service}" \
                --format '{{.ID}}' 2>/dev/null | head -n 1 || true
        )"
        if [ -n "${container_id}" ] && container_is_running "${container_id}"; then
            printf '%s\n' "${container_id}"
            return 0
        fi
    done

    # 兼容仓库历史上两个固定容器名。只接受精确名称，不做
    # `*postgres*` 模糊匹配，避免选中 ains-postgres 等无关数据库。
    for container_id in keycompute-postgres keycompute-postgres-primary; do
        if container_is_running "${container_id}"; then
            printf '%s\n' "${container_id}"
            return 0
        fi
    done

    return 1
}

# 被 shell 测试 source 时只导出上述辅助函数，不执行交互式重置。
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
    return 0
fi

# ── 前置检查 ────────────────────────────────────────────────────────────────

info "=== KeyCompute 管理员密码重置 ==="
echo ""

# 检查 Docker 是否可用
if ! command -v docker &>/dev/null; then
    error "Docker 未安装或不在 PATH 中"
    exit 1
fi

# 自动解析当前编排的写库容器
if ! DB_CONTAINER="$(resolve_database_container)"; then
    error "未找到当前 KeyCompute 项目正在运行的数据库容器！"
    info "可用的容器："
    docker ps --format '  {{.Names}}  ({{.Status}})'
    info "如使用自定义容器，可设置 KC_RESET_DB_CONTAINER=<name-or-id>"
    exit 1
fi
DB_CONTAINER_NAME="$(container_display_name "${DB_CONTAINER}")"
DB_USER="${DB_USER_OVERRIDE:-$(container_env_value "${DB_CONTAINER}" POSTGRES_USER)}"
DB_NAME="${DB_NAME_OVERRIDE:-$(container_env_value "${DB_CONTAINER}" POSTGRES_DB)}"
DB_PASSWORD="${DB_PASSWORD_OVERRIDE:-$(container_env_value "${DB_CONTAINER}" POSTGRES_PASSWORD)}"
DB_USER="${DB_USER:-keycompute}"
DB_NAME="${DB_NAME:-keycompute}"
ok "数据库容器 ${DB_CONTAINER_NAME} 运行正常"
info "数据库：${DB_NAME}（用户 ${DB_USER}）"

# 检查 Python3
if ! command -v python3 &>/dev/null; then
    error "Python3 未安装，请先安装：apt install python3 python3-pip"
    exit 1
fi
ok "Python3 可用"

if ! DEFAULT_ADMIN_EMAIL="$(normalize_admin_email "${DEFAULT_ADMIN_EMAIL}")"; then
    error "管理员邮箱配置无效，未执行任何数据库修改"
    exit 1
fi

# 检查 argon2-cffi；缺少时只在隔离的临时虚拟环境中安装固定版本，避免
# 修改系统 Python。安装需要网络和 python3-venv；若环境不具备则明确失败。
trap cleanup_argon2_venv EXIT
if ! "${PYTHON_BIN}" -c "import argon2" 2>/dev/null; then
    warn "argon2-cffi 未安装，正在临时虚拟环境中安装固定版本..."
    if ! "${PYTHON_BIN}" -m venv --clear "${ARGON2_VENV:=$(mktemp -d "${TMPDIR:-/tmp}/keycompute-argon2.XXXXXX")}"; then
        error "无法创建临时 Python 虚拟环境，请安装 python3-venv"
        exit 1
    fi
    if ! PIP_NO_INPUT=1 "${ARGON2_VENV}/bin/python" -m pip install \
        --disable-pip-version-check --no-input --quiet "argon2-cffi==23.1.0"; then
        error "安装固定版本 argon2-cffi 失败，请检查网络或预先安装该依赖"
        exit 1
    fi
    PYTHON_BIN="${ARGON2_VENV}/bin/python"
    if ! "${PYTHON_BIN}" -c "import argon2" 2>/dev/null; then
        error "临时虚拟环境中的 argon2-cffi 不可用"
        exit 1
    fi
    ok "argon2-cffi 已安装到临时虚拟环境"
else
    ok "argon2-cffi 已安装"
fi
echo ""

# ── 非交互式原子重置/补齐 ─────────────────────────────────────────────────────

# 账号、凭证、余额和账本流水必须在同一事务内完成。这样脚本可重复执行，
# 任一步失败都会整体回滚，不会留下“有用户但没有凭证/余额”的半初始化状态。
info "正在以非交互方式重置系统账号并设置余额..."

if ! PASSWORD_HASH="$(generate_password_hash "${DEFAULT_PASSWORD}")"; then
    error "默认管理员密码哈希生成失败！"
    exit 1
fi

if ! ADMIN_INFO="$(
    KC_RESET_PASSWORD_HASH="${PASSWORD_HASH}" \
    KC_RESET_ADMIN_EMAIL_VALUE="${DEFAULT_ADMIN_EMAIL}" \
    KC_RESET_ADMIN_NAME_VALUE="${DEFAULT_ADMIN_NAME}" \
    KC_RESET_DB_CONTAINER_ID="${DB_CONTAINER}" \
    KC_RESET_DB_USER_RESOLVED="${DB_USER}" \
    KC_RESET_DB_NAME_RESOLVED="${DB_NAME}" \
    KC_RESET_DB_PASSWORD_RESOLVED="${DB_PASSWORD}" \
    "${PYTHON_BIN}" <<'PYEOF'
import os
import subprocess
import sys


def sql_literal(value: str) -> str:
    """Return a safely quoted PostgreSQL string literal."""
    return "'" + value.replace("'", "''") + "'"


password_hash = sql_literal(os.environ["KC_RESET_PASSWORD_HASH"])
admin_email = os.environ["KC_RESET_ADMIN_EMAIL_VALUE"]
admin_name = os.environ["KC_RESET_ADMIN_NAME_VALUE"]
email_sql = sql_literal(admin_email)
name_sql = sql_literal(admin_name)

sql = f"""
BEGIN;
SELECT pg_advisory_xact_lock(5421647644090913945);

DO $bootstrap$
DECLARE
    v_system_tenant_id UUID;
    v_admin_id UUID;
    v_admin_tenant_id UUID;
    v_existing_id UUID;
    v_existing_role TEXT;
    v_existing_admin BOOLEAN := FALSE;
    v_balance user_balances%ROWTYPE;
    v_delta DECIMAL(20, 10);
    v_active_reserved DECIMAL(20, 10);
    v_expired_amount DECIMAL(20, 10);
    v_active_count BIGINT;
    v_level1_ratio NUMERIC;
    v_level2_ratio NUMERIC;
    v_effective_from TIMESTAMPTZ;
BEGIN
    INSERT INTO tenants (name, slug, description, status)
    VALUES ('System', 'system', 'System default tenant', 'active')
    ON CONFLICT (slug) DO NOTHING;

    SELECT id INTO v_system_tenant_id
    FROM tenants
    WHERE slug = 'system';

    IF v_system_tenant_id IS NULL THEN
        RAISE EXCEPTION 'system tenant is missing';
    END IF;

    SELECT id, tenant_id INTO v_admin_id, v_admin_tenant_id
    FROM users
    WHERE role = 'system'
    ORDER BY created_at ASC, id ASC
    LIMIT 1
    FOR UPDATE;

    IF v_admin_id IS NULL THEN
        SELECT id, role INTO v_existing_id, v_existing_role
        FROM users
        WHERE email = {email_sql}
        FOR UPDATE;

        IF v_existing_id IS NOT NULL THEN
            IF v_existing_role <> 'system' THEN
                RAISE EXCEPTION 'configured admin email is already used by a non-system account';
            END IF;
            v_admin_id := v_existing_id;
            SELECT tenant_id INTO v_admin_tenant_id
            FROM users
            WHERE id = v_admin_id
            FOR UPDATE;
            v_existing_admin := TRUE;
        ELSE
            INSERT INTO users (tenant_id, email, name, role)
            VALUES (v_system_tenant_id, {email_sql}, {name_sql}, 'system')
            RETURNING id, tenant_id INTO v_admin_id, v_admin_tenant_id;
        END IF;
    ELSE
        v_existing_admin := TRUE;
    END IF;

    IF v_admin_tenant_id IS DISTINCT FROM v_system_tenant_id THEN
        RAISE EXCEPTION 'system admin is not attached to the system tenant';
    END IF;

    IF v_existing_admin THEN
        UPDATE users
        SET token_version = token_version + 1,
            updated_at = NOW()
        WHERE id = v_admin_id;
    END IF;

    INSERT INTO user_credentials (
        user_id, password_hash, email_verified, email_verified_at,
        failed_login_attempts, locked_until, updated_at
    )
    VALUES (v_admin_id, {password_hash}, TRUE, NOW(), 0, NULL, NOW())
    ON CONFLICT (user_id) DO UPDATE SET
        password_hash = EXCLUDED.password_hash,
        email_verified = TRUE,
        email_verified_at = NOW(),
        failed_login_attempts = 0,
        locked_until = NULL,
        updated_at = NOW();

    SELECT * INTO v_balance
    FROM user_balances
    WHERE user_id = v_admin_id
    FOR UPDATE;

    IF NOT FOUND THEN
        SELECT COUNT(*)
        INTO v_active_count
        FROM balance_reservations
        WHERE user_id = v_admin_id
          AND status = 'active';

        IF v_active_count > 0 THEN
            RAISE EXCEPTION 'cannot initialize balance while active reservations exist';
        END IF;

        INSERT INTO user_balances (
            tenant_id, user_id, available_balance, frozen_balance,
            total_recharged, total_consumed
        )
        VALUES (v_system_tenant_id, v_admin_id, 10000, 0, 10000, 0);

        INSERT INTO balance_transactions (
            tenant_id, user_id, transaction_type, amount,
            balance_before, balance_after, description
        )
        VALUES (
            v_system_tenant_id, v_admin_id, 'recharge', 10000,
            0, 10000, 'reset_admin_password.sh 初始化系统账号余额'
        );
    ELSE
        IF v_balance.tenant_id IS DISTINCT FROM v_system_tenant_id THEN
            RAISE EXCEPTION 'system admin balance is not attached to the system tenant';
        END IF;

        SELECT COALESCE(SUM(amount), 0), COUNT(*)
        INTO v_active_reserved, v_active_count
        FROM balance_reservations
        WHERE user_id = v_admin_id
          AND status = 'active';

        IF EXISTS (
            SELECT 1
            FROM balance_reservations
            WHERE user_id = v_admin_id
              AND status = 'active'
              AND tenant_id IS DISTINCT FROM v_system_tenant_id
        ) THEN
            RAISE EXCEPTION 'system admin has a reservation attached to another tenant';
        END IF;

        IF v_active_reserved > v_balance.frozen_balance THEN
            RAISE EXCEPTION 'active balance reservations exceed frozen balance';
        END IF;

        WITH expired AS (
            UPDATE balance_reservations
            SET status = 'expired',
                updated_at = NOW()
            WHERE user_id = v_admin_id
              AND status = 'active'
              AND expires_at <= NOW()
            RETURNING amount
        )
        SELECT COALESCE(SUM(amount), 0)
        INTO v_expired_amount
        FROM expired;

        IF v_expired_amount > 0 THEN
            UPDATE user_balances
            SET available_balance = available_balance + v_expired_amount,
                frozen_balance = frozen_balance - v_expired_amount,
                updated_at = NOW()
            WHERE id = v_balance.id;

            SELECT * INTO v_balance
            FROM user_balances
            WHERE id = v_balance.id
            FOR UPDATE;
        END IF;

        SELECT COUNT(*)
        INTO v_active_count
        FROM balance_reservations
        WHERE user_id = v_admin_id
          AND status = 'active';

        IF v_active_count > 0 THEN
            RAISE EXCEPTION 'cannot reset balance while active reservations exist';
        END IF;

        IF v_balance.frozen_balance <> 0 THEN
            RAISE EXCEPTION 'cannot reset balance while frozen balance remains';
        END IF;

        v_delta := 10000 - v_balance.available_balance;
        IF v_delta <> 0 THEN
            UPDATE user_balances
            SET available_balance = 10000,
                total_recharged = CASE
                    WHEN v_delta > 0 THEN total_recharged + v_delta
                    ELSE total_recharged
                END,
                total_consumed = CASE
                    WHEN v_delta < 0 THEN total_consumed + (-v_delta)
                    ELSE total_consumed
                END,
                updated_at = NOW()
            WHERE id = v_balance.id;

            INSERT INTO balance_transactions (
                tenant_id, user_id, transaction_type, amount,
                balance_before, balance_after, description
            )
            VALUES (
                v_system_tenant_id,
                v_admin_id,
                CASE WHEN v_delta > 0 THEN 'recharge' ELSE 'consume' END,
                v_delta,
                v_balance.available_balance,
                10000,
                'reset_admin_password.sh 将系统账号余额调整为 10000'
            );
        END IF;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM tenant_distribution_rules
        WHERE tenant_id = v_system_tenant_id
    ) THEN
        SELECT CASE
            WHEN value ~ '^[0-9]+([.][0-9]+)?$' THEN value::numeric
            ELSE 0.03
        END
        INTO v_level1_ratio
        FROM system_settings
        WHERE key = 'distribution_level1_default_ratio';

        SELECT CASE
            WHEN value ~ '^[0-9]+([.][0-9]+)?$' THEN value::numeric
            ELSE 0.02
        END
        INTO v_level2_ratio
        FROM system_settings
        WHERE key = 'distribution_level2_default_ratio';

        IF v_level1_ratio IS NULL OR v_level1_ratio < 0 OR v_level1_ratio > 1 THEN
            v_level1_ratio := 0.03;
        END IF;
        IF v_level2_ratio IS NULL OR v_level2_ratio < 0 OR v_level2_ratio > 1 THEN
            v_level2_ratio := 0.02;
        END IF;

        v_effective_from := clock_timestamp();

        INSERT INTO tenant_distribution_rules (
            tenant_id, beneficiary_id, name, description, commission_rate,
            priority, effective_from
        )
        VALUES (
            v_system_tenant_id,
            '00000000-0000-0000-0000-000000000000',
            '一级分销规则',
            '默认一级分销规则，推荐人可获得指定比例的分销佣金',
            v_level1_ratio,
            10,
            v_effective_from
        );

        INSERT INTO tenant_distribution_rules (
            tenant_id, beneficiary_id, name, description, commission_rate,
            priority, effective_from
        )
        VALUES (
            v_system_tenant_id,
            '00000000-0000-0000-0000-000000000000',
            '二级分销规则',
            '默认二级分销规则，间接推荐人可获得指定比例的分销佣金',
            v_level2_ratio,
            5,
            v_effective_from + interval '1 microsecond'
        );
    END IF;
END $bootstrap$;

SELECT id || '|' || email
FROM users
WHERE role = 'system'
ORDER BY created_at ASC, id ASC
LIMIT 1;

COMMIT;
"""

cmd = [
    "docker", "exec", "-i",
]
db_password = os.environ.get("KC_RESET_DB_PASSWORD_RESOLVED", "")
if db_password:
    cmd.extend(["-e", f"PGPASSWORD={db_password}"])
cmd.extend([
    os.environ["KC_RESET_DB_CONTAINER_ID"],
    "psql", "-U", os.environ["KC_RESET_DB_USER_RESOLVED"],
    "-d", os.environ["KC_RESET_DB_NAME_RESOLVED"],
    "-v", "ON_ERROR_STOP=1", "-X", "-w", "-qAt",
])
result = subprocess.run(cmd, input=sql, capture_output=True, text=True)
if result.returncode != 0:
    print(f"[ERROR] 数据库原子重置失败: {result.stderr.strip()}", file=sys.stderr)
    sys.exit(1)

lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
if not lines:
    print("[ERROR] 事务已提交但未找到 system 账号", file=sys.stderr)
    sys.exit(1)
print(lines[-1])
PYEOF
)"; then
    error "系统账号重置失败，事务已回滚！"
    exit 1
fi

ADMIN_ID="${ADMIN_INFO%%|*}"
ADMIN_EMAIL_FOUND="${ADMIN_INFO#*|}"
ok "系统账号重置成功"
info "用户 ID：${ADMIN_ID}"
info "邮箱：${ADMIN_EMAIL_FOUND}"
info "密码：已重置"
info "余额：10000"
warn "请登录后立即修改默认密码。"
