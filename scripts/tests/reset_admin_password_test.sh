#!/usr/bin/env bash

set -euo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
source "${TEST_DIR}/../reset_admin_password.sh"

MOCK_RUNNING=""
MOCK_MAIN_ID=""
MOCK_REPLICA_ID=""
MOCK_LABEL_POSTGRES=""
MOCK_LABEL_PRIMARY=""

docker() {
    local command="${1:-}"
    shift || true

    case "${command}" in
        inspect)
            local target="${*: -1}"
            if [[ " ${MOCK_RUNNING} " == *" ${target} "* ]]; then
                printf 'true\n'
            else
                printf 'false\n'
            fi
            ;;
        compose)
            local compose_file=""
            local previous=""
            local argument
            for argument in "$@"; do
                if [ "${previous}" = "-f" ]; then
                    compose_file="${argument}"
                fi
                previous="${argument}"
            done
            if [[ "${compose_file}" == *"docker-compose.replicas.yml" ]]; then
                printf '%s\n' "${MOCK_REPLICA_ID}"
            else
                printf '%s\n' "${MOCK_MAIN_ID}"
            fi
            ;;
        ps)
            local service=""
            local argument
            for argument in "$@"; do
                case "${argument}" in
                    label=com.docker.compose.service=*)
                        service="${argument##*=}"
                        ;;
                esac
            done
            if [ "${service}" = "postgres-primary" ]; then
                printf '%s\n' "${MOCK_LABEL_PRIMARY}"
            else
                printf '%s\n' "${MOCK_LABEL_POSTGRES}"
            fi
            ;;
        *)
            printf 'unexpected docker command: %s\n' "${command}" >&2
            return 1
            ;;
    esac
}

assert_resolves_to() {
    local expected="$1"
    local actual
    actual="$(resolve_database_container)"
    if [ "${actual}" != "${expected}" ]; then
        printf 'expected container %s, got %s\n' "${expected}" "${actual}" >&2
        exit 1
    fi
}

reset_mocks() {
    DB_CONTAINER_OVERRIDE=""
    MOCK_RUNNING=""
    MOCK_MAIN_ID=""
    MOCK_REPLICA_ID=""
    MOCK_LABEL_POSTGRES=""
    MOCK_LABEL_PRIMARY=""
}

reset_mocks
DB_CONTAINER_OVERRIDE="custom-postgres"
MOCK_RUNNING="custom-postgres"
assert_resolves_to "custom-postgres"

reset_mocks
MOCK_MAIN_ID="main-compose-id"
MOCK_REPLICA_ID="replica-compose-id"
MOCK_RUNNING="main-compose-id replica-compose-id"
assert_resolves_to "main-compose-id"

reset_mocks
MOCK_REPLICA_ID="replica-compose-id"
MOCK_RUNNING="replica-compose-id"
assert_resolves_to "replica-compose-id"

reset_mocks
MOCK_LABEL_POSTGRES="labeled-main-id"
MOCK_RUNNING="labeled-main-id"
assert_resolves_to "labeled-main-id"

reset_mocks
MOCK_RUNNING="ains-postgres keycompute-postgres"
assert_resolves_to "keycompute-postgres"

reset_mocks
DB_CONTAINER_OVERRIDE="stopped-custom-postgres"
if resolve_database_container >/dev/null 2>&1; then
    printf 'a stopped explicit override must be rejected\n' >&2
    exit 1
fi

if [ "$(normalize_admin_email ' Admin@Example.COM ')" != "admin@example.com" ]; then
    printf 'administrator email normalization must trim and lowercase values\n' >&2
    exit 1
fi
if normalize_admin_email 'not-an-email' >/dev/null 2>&1; then
    printf 'invalid administrator email must be rejected before database changes\n' >&2
    exit 1
fi

SCRIPT_PATH="${TEST_DIR}/../reset_admin_password.sh"
if grep -En -- '(^|[[:space:]])read([[:space:]]|$)' "${SCRIPT_PATH}"; then
    printf 'reset script must not read a password interactively\n' >&2
    exit 1
fi
for required_text in \
    'DEFAULT_PASSWORD="12345"' \
    'KC_RESET_DB_PASSWORD' \
    'BEGIN;' \
    'COMMIT;' \
    'pg_advisory_xact_lock(5421647644090913945)' \
    'available_balance, frozen_balance' \
    'VALUES (v_system_tenant_id, v_admin_id, 10000, 0, 10000, 0)' \
    'SET available_balance = 10000' \
    'ON CONFLICT (user_id) DO UPDATE SET' \
    'token_version = token_version + 1' \
    'active balance reservations exceed frozen balance' \
    'system admin has a reservation attached to another tenant' \
    'cannot initialize balance while active reservations exist' \
    'cannot reset balance while active reservations exist' \
    'tenant_distribution_rules' \
    'argon2-cffi==23.1.0' \
    '"-X", "-w", "-qAt"'; do
    if ! grep -Fq -- "${required_text}" "${SCRIPT_PATH}"; then
        printf 'reset script is missing required invariant: %s\n' "${required_text}" >&2
        exit 1
    fi
done

if grep -Fn -- 'info "密码：${DEFAULT_PASSWORD}"' "${SCRIPT_PATH}"; then
    printf 'reset script must not print the default password to logs\n' >&2
    exit 1
fi

printf 'reset_admin_password helper tests: ok\n'
