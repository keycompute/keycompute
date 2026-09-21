-- KeyCompute 001_init：新库完整结构。
-- 仅用于空数据库初始化，不包含旧版本升级、数据回填或兼容迁移逻辑。

-- users: 用户表
CREATE TABLE IF NOT EXISTS users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid() CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    email VARCHAR(255) NOT NULL UNIQUE,
    name VARCHAR(255),
    platform_role VARCHAR(20) NOT NULL DEFAULT 'none'
        CONSTRAINT chk_users_platform_role_allowed CHECK (platform_role IN ('root', 'operator', 'none')),
    status VARCHAR(20) NOT NULL DEFAULT 'active'
        CONSTRAINT chk_users_status_allowed CHECK (status IN ('active', 'suspended')),
    -- 安全事件或权限变化时递增，使此前签发的 JWT 立即失效
    token_version INTEGER NOT NULL DEFAULT 0 CHECK (token_version >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_users_email ON users(email);
CREATE UNIQUE INDEX IF NOT EXISTS uq_users_email_normalized ON users(lower(btrim(email)));

CREATE INDEX IF NOT EXISTS idx_users_platform_role ON users(platform_role);

-- tenants: 租户/组织表
CREATE TABLE IF NOT EXISTS tenants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_user_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    name VARCHAR(255) NOT NULL,
    slug VARCHAR(100) NOT NULL UNIQUE,
    description TEXT,
    status VARCHAR(50) NOT NULL DEFAULT 'active',
    -- 租户配置
    default_rpm_limit INTEGER NOT NULL DEFAULT 60,
    default_tpm_limit INTEGER NOT NULL DEFAULT 100000,
    responses_idempotency_claim_count BIGINT NOT NULL DEFAULT 0,
    authz_version BIGINT NOT NULL DEFAULT 1 CHECK (authz_version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ck_tenants_responses_idempotency_claim_count CHECK (
        responses_idempotency_claim_count BETWEEN 0 AND 100000
    ),
    CONSTRAINT ck_tenants_status CHECK (status IN ('active', 'inactive')),
    CONSTRAINT ck_tenants_limits CHECK (default_rpm_limit >= 0 AND default_tpm_limit >= 0),
    CONSTRAINT ck_tenants_real_id CHECK (id <> '00000000-0000-0000-0000-000000000000')
);

CREATE INDEX IF NOT EXISTS idx_tenants_slug ON tenants(slug);
CREATE INDEX IF NOT EXISTS idx_tenants_status ON tenants(status);

-- Tenant membership is the sole source of tenant role and tenant ownership.
CREATE TABLE IF NOT EXISTS tenant_memberships (
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    role VARCHAR(20) NOT NULL DEFAULT 'member'
        CONSTRAINT chk_tenant_memberships_role CHECK (role IN ('admin', 'member')),
    status VARCHAR(20) NOT NULL DEFAULT 'active'
        CONSTRAINT chk_tenant_memberships_status CHECK (status IN ('active', 'suspended', 'revoked')),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_tenant_memberships_user ON tenant_memberships(user_id, status);
CREATE INDEX IF NOT EXISTS idx_tenant_memberships_tenant_role ON tenant_memberships(tenant_id, role, status);

CREATE TABLE IF NOT EXISTS tenant_invitations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    invited_by_user_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    email VARCHAR(255) NOT NULL,
    role VARCHAR(20) NOT NULL DEFAULT 'member'
        CONSTRAINT chk_tenant_invitations_role CHECK (role IN ('admin', 'member')),
    token_hash VARCHAR(64) NOT NULL UNIQUE CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    status VARCHAR(20) NOT NULL DEFAULT 'pending'
        CONSTRAINT chk_tenant_invitations_status CHECK (status IN ('pending', 'accepted', 'expired', 'revoked')),
    expires_at TIMESTAMPTZ NOT NULL,
    accepted_by_user_id UUID REFERENCES users(id) ON DELETE RESTRICT,
    accepted_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ck_invitation_email CHECK (email=lower(btrim(email)) AND position('@' in email)>1),
    CONSTRAINT ck_invitation_lifecycle CHECK (
        (status='accepted' AND accepted_at IS NOT NULL AND accepted_by_user_id IS NOT NULL AND revoked_at IS NULL) OR
        (status='revoked' AND accepted_at IS NULL AND accepted_by_user_id IS NULL AND revoked_at IS NOT NULL) OR
        (status IN ('pending','expired') AND accepted_at IS NULL AND accepted_by_user_id IS NULL AND revoked_at IS NULL)
    )
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_tenant_invitations_pending_email
    ON tenant_invitations(tenant_id, lower(email)) WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS idx_tenant_invitations_token ON tenant_invitations(token_hash);

CREATE TABLE IF NOT EXISTS tenant_audit_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope_type VARCHAR(20) NOT NULL DEFAULT 'tenant' CHECK (scope_type IN ('platform','tenant')),
    tenant_id UUID,
    actor_user_id UUID NOT NULL,
    action VARCHAR(100) NOT NULL CHECK (BTRIM(action) <> ''),
    resource_type VARCHAR(100) NOT NULL CHECK (BTRIM(resource_type) <> ''),
    resource_id TEXT,
    request_id UUID,
    credential_kind VARCHAR(32) NOT NULL DEFAULT 'jwt'
        CHECK (credential_kind IN ('jwt','api_key','node','system')),
    actor_platform_role VARCHAR(20) NOT NULL
        CHECK (actor_platform_role IN ('root','operator','none')),
    result VARCHAR(20) NOT NULL DEFAULT 'success' CHECK (result IN ('success','denied','failure')),
    actor_tenant_role VARCHAR(20)
        CHECK (actor_tenant_role IN ('admin','member')),
    details JSONB NOT NULL DEFAULT '{}'::jsonb
        CHECK (jsonb_typeof(details) = 'object'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ck_audit_scope_tenant CHECK ((scope_type='platform' AND tenant_id IS NULL) OR (scope_type='tenant' AND tenant_id IS NOT NULL)),
    CONSTRAINT ck_audit_ids_real CHECK (
        actor_user_id <> '00000000-0000-0000-0000-000000000000'
        AND (tenant_id IS NULL OR tenant_id <> '00000000-0000-0000-0000-000000000000')
    ),
    CONSTRAINT ck_audit_details_size CHECK (octet_length(details::text) <= 16384)
);
CREATE INDEX IF NOT EXISTS idx_tenant_audit_events_tenant_time
    ON tenant_audit_events(tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_tenant_audit_events_actor_time
    ON tenant_audit_events(actor_user_id, created_at DESC);

-- produce_ai_keys: Produce AI Key 表（用户访问系统的 API Key）
CREATE TABLE IF NOT EXISTS produce_ai_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 删除用户或租户时同步删除密钥，避免认证命中孤儿记录
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    user_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    produce_ai_key_hash VARCHAR(255) NOT NULL UNIQUE,
    produce_ai_key_preview VARCHAR(20) NOT NULL,
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    revoked_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    last_used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fk_produce_ai_keys_membership
        FOREIGN KEY (tenant_id, user_id)
        REFERENCES tenant_memberships(tenant_id, user_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_produce_ai_keys_tenant ON produce_ai_keys(tenant_id);
CREATE INDEX IF NOT EXISTS idx_produce_ai_keys_user ON produce_ai_keys(user_id);
CREATE INDEX IF NOT EXISTS idx_produce_ai_keys_hash ON produce_ai_keys(produce_ai_key_hash);
CREATE INDEX IF NOT EXISTS idx_produce_ai_keys_revoked ON produce_ai_keys(revoked) WHERE revoked = FALSE;
-- accounts: 上游 Provider 账号池
CREATE TABLE IF NOT EXISTS accounts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    provider VARCHAR(50) NOT NULL,
    name VARCHAR(255) NOT NULL,
    endpoint VARCHAR(500) NOT NULL,
    upstream_api_key_encrypted TEXT NOT NULL,
    upstream_api_key_preview VARCHAR(20) NOT NULL,
    rpm_limit INTEGER NOT NULL DEFAULT 60,
    tpm_limit INTEGER NOT NULL DEFAULT 100000,
    priority INTEGER NOT NULL DEFAULT 0,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    pool_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    models_supported TEXT[] NOT NULL DEFAULT '{}',
    api_capabilities TEXT[] NOT NULL,
    visibility VARCHAR(20) NOT NULL DEFAULT 'tenant',
    -- Account-level routing health. `enabled` remains the administrator's
    -- intent; health is an independent operational signal used by routing.
    health_status VARCHAR(20) NOT NULL DEFAULT 'unknown',
    health_reason VARCHAR(128),
    health_penalty INTEGER NOT NULL DEFAULT 0,
    health_consecutive_failures INTEGER NOT NULL DEFAULT 0,
    health_success_count BIGINT NOT NULL DEFAULT 0,
    health_failure_count BIGINT NOT NULL DEFAULT 0,
    health_avg_latency_ms BIGINT,
    health_last_success_at TIMESTAMPTZ,
    health_last_failure_at TIMESTAMPTZ,
    health_updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    health_generation BIGINT NOT NULL DEFAULT 0,
    last_probe_at TIMESTAMPTZ,
    last_probe_latency_ms BIGINT,
    last_probe_status VARCHAR(32),
    last_probe_error_code VARCHAR(128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    upstream_config_version TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ck_accounts_api_capabilities CHECK (
        cardinality(api_capabilities) > 0
        AND (
            (provider = 'openai' AND api_capabilities <@ ARRAY['chat_completions', 'responses']::TEXT[])
            OR
            (provider = 'anthropic' AND api_capabilities <@ ARRAY['messages']::TEXT[])
        )
    ),
    CONSTRAINT ck_accounts_probe_status
        CHECK (last_probe_status IS NULL OR last_probe_status IN ('succeeded', 'failed')),
    CONSTRAINT ck_accounts_priority
        CHECK (priority BETWEEN 0 AND 10),
    CONSTRAINT ck_accounts_health_status
        CHECK (health_status IN ('unknown', 'healthy', 'degraded', 'unhealthy')),
    CONSTRAINT ck_accounts_health_penalty
        CHECK (health_penalty BETWEEN 0 AND 100),
    CONSTRAINT ck_accounts_health_counters
        CHECK (health_consecutive_failures >= 0
            AND health_success_count >= 0
            AND health_failure_count >= 0)
);

CREATE INDEX IF NOT EXISTS idx_accounts_tenant_id ON accounts(tenant_id);
CREATE INDEX IF NOT EXISTS idx_accounts_provider ON accounts(provider);
CREATE INDEX IF NOT EXISTS idx_accounts_enabled ON accounts(enabled) WHERE enabled = TRUE;
CREATE INDEX IF NOT EXISTS idx_accounts_pool_enabled ON accounts(pool_enabled) WHERE pool_enabled = TRUE;
CREATE INDEX IF NOT EXISTS idx_accounts_visibility ON accounts(visibility) WHERE visibility = 'global';
CREATE INDEX IF NOT EXISTS idx_accounts_api_capabilities ON accounts USING GIN(api_capabilities);
CREATE INDEX IF NOT EXISTS idx_accounts_health_routing
    ON accounts(health_status, health_penalty, priority)
    WHERE enabled = TRUE;

-- passthrough_bindings are account grants.  Models are intentionally not
-- copied here: every declared account model is granted dynamically and the
-- account/tenant predicates below are authoritative at runtime.
CREATE TABLE IF NOT EXISTS passthrough_bindings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE RESTRICT,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    is_global BOOLEAN NOT NULL DEFAULT FALSE,
    pool_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    revision BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ck_passthrough_bindings_revision_positive CHECK (revision > 0),
    CONSTRAINT uk_passthrough_bindings_account_tenant UNIQUE (account_id, tenant_id)
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_passthrough_bindings_global_account
    ON passthrough_bindings(account_id) WHERE is_global = TRUE;
CREATE INDEX IF NOT EXISTS idx_passthrough_bindings_tenant
    ON passthrough_bindings(tenant_id, is_global, account_id);
CREATE INDEX IF NOT EXISTS idx_passthrough_bindings_account
    ON passthrough_bindings(account_id);

-- account_model_health: independent health snapshots for each account,
-- capability and model. Missing facts do not require per-model activation.
-- Known current failures are isolated; a configuration fence prevents stale
-- observations from reviving failures produced against a newer connection.
CREATE TABLE IF NOT EXISTS account_model_health (
    account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    api_capability VARCHAR(50) NOT NULL,
    model VARCHAR(255) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'unknown',
    reason_code VARCHAR(128),
    checked_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    -- Snapshot of accounts.upstream_config_version when observed.
    -- This timestamp is the account configuration version/fence.
    account_config_version TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    generation BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (account_id, api_capability, model),
    CONSTRAINT ck_account_model_health_capability
        CHECK (api_capability IN ('chat_completions', 'responses', 'messages')),
    CONSTRAINT ck_account_model_health_model_nonempty
        CHECK (BTRIM(model) <> '' AND model = BTRIM(model)),
    CONSTRAINT ck_account_model_health_status
        CHECK (status IN ('unknown', 'healthy', 'degraded', 'unhealthy')),
    CONSTRAINT ck_account_model_health_reason_safe
        CHECK (reason_code IS NULL OR (
            BTRIM(reason_code) = reason_code
            AND CHAR_LENGTH(reason_code) <= 128
            AND reason_code ~ '^[a-z0-9_.:-]+$'
        )),
    CONSTRAINT ck_account_model_health_expiry
        CHECK (expires_at IS NULL OR checked_at IS NOT NULL AND expires_at > checked_at),
    CONSTRAINT ck_account_model_health_generation_nonnegative
        CHECK (generation >= 0)
);

CREATE INDEX IF NOT EXISTS idx_account_model_health_expiry
    ON account_model_health(account_id, api_capability, expires_at);
CREATE INDEX IF NOT EXISTS idx_account_model_health_status
    ON account_model_health(status, expires_at);

-- responses_idempotency_claims: Responses Idempotency-Key 的永久身份绑定及短期结果缓存。
-- 只保留哈希后的绑定 ID；account_id 是历史执行归属，故意不引用可删除的
-- accounts 配置行。否则删除账号会重新开放已经使用过的幂等键。结果正文受
-- 应用层租户配额约束且仅在 replay 窗口内保留；永久身份数量也受应用层
-- 租户配额约束。过期后仍保留身份绑定，防止同一键被重新用于其他请求。
CREATE TABLE IF NOT EXISTS responses_idempotency_claims (
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    binding_id VARCHAR(128) NOT NULL,
    request_fingerprint VARCHAR(64) NOT NULL,
    billing_request_id UUID NOT NULL,
    user_id UUID NOT NULL,
    produce_ai_key_id UUID NOT NULL,
    provider VARCHAR(50) NOT NULL,
    model TEXT,
    account_id UUID NOT NULL,
    execution_state VARCHAR(32) NOT NULL DEFAULT 'in_progress',
    execution_token UUID NOT NULL DEFAULT gen_random_uuid(),
    lease_expires_at TIMESTAMPTZ NOT NULL DEFAULT (NOW() + INTERVAL '5 minutes'),
    -- Set immediately before handing the paid POST to the gateway. Once set,
    -- an expired lease is intentionally not reclaimable because the upstream
    -- outcome may be ambiguous after a process crash or response-read failure.
    upstream_dispatched_at TIMESTAMPTZ,
    response_status SMALLINT,
    response_headers JSONB,
    response_body TEXT,
    response_body_bytes BIGINT,
    response_expires_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, binding_id),
    CONSTRAINT uk_responses_idempotency_claims_billing UNIQUE (billing_request_id),
    CONSTRAINT ck_responses_idempotency_claims_state CHECK (
        (
            execution_state = 'in_progress'
            AND response_status IS NULL
            AND response_headers IS NULL
            AND response_body IS NULL
            AND response_body_bytes IS NULL
            AND response_expires_at IS NULL
            AND completed_at IS NULL
        )
        OR (
            execution_state = 'completed'
            AND response_status BETWEEN 100 AND 599
            AND response_headers IS NOT NULL
            AND response_body IS NOT NULL
            AND response_body_bytes BETWEEN 0 AND 100663296
            AND response_body_bytes = octet_length(response_body)
            AND response_expires_at IS NOT NULL
            AND upstream_dispatched_at IS NOT NULL
            AND completed_at IS NOT NULL
        )
        OR (
            execution_state = 'expired'
            AND response_status IS NULL
            AND response_headers IS NULL
            AND response_body IS NULL
            AND response_body_bytes IS NULL
            AND response_expires_at IS NULL
            AND upstream_dispatched_at IS NOT NULL
            AND completed_at IS NOT NULL
        )
    ),
    CONSTRAINT fk_responses_idempotency_claims_membership FOREIGN KEY (tenant_id,user_id) REFERENCES tenant_memberships(tenant_id,user_id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_responses_idempotency_claims_response_expiry
    ON responses_idempotency_claims(response_expires_at)
    WHERE execution_state = 'completed';

CREATE INDEX IF NOT EXISTS idx_responses_idempotency_claims_tenant_replay
    ON responses_idempotency_claims(tenant_id, completed_at DESC, binding_id DESC)
    WHERE execution_state = 'completed';

-- response_affinities: OpenAI resp_*/conv_* 资源到创建账号的租户+用户绑定。
-- 后续资源操作及 conversation 请求必须继续命中同一上游账号和调用者。
CREATE TABLE IF NOT EXISTS response_affinities (
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    -- Immutable caller identity snapshot, intentionally not a cascading user FK:
    -- accepted requests must still persist settlement after a user is removed.
    -- NULL is for internal/unowned rows; public paths require an exact user ID.
    user_id UUID,
    -- Also stores official conv_* IDs; the namespaces are disjoint.
    -- OpenAI resource IDs are opaque. The 2048-byte application limit keeps
    -- this primary-key component below PostgreSQL's B-tree entry limit.
    response_id VARCHAR(2048) NOT NULL,
    provider VARCHAR(50) NOT NULL,
    -- Actual upstream model, used when a continuation omits `model` as the
    -- official Responses contract permits.
    model TEXT,
    -- Upstream resources, chained warmups, reservations and settlements retain
    -- their owning account. Root local warmups and terminal node-settlement
    -- outboxes have no upstream account owner and leave this NULL.
    account_id UUID REFERENCES accounts(id) ON DELETE RESTRICT,
    -- Short-lived route reservation created before dispatch. It closes the
    -- interval in which account deletion could otherwise win before the real
    -- resp_* affinity is known.
    is_reservation BOOLEAN NOT NULL DEFAULT FALSE,
    -- KeyCompute-local WebSocket warmups (`generate:false,store:true`).
    -- They retain chain context without inventing an upstream resource.
    local_response JSONB,
    local_context JSONB,
    -- Conservative application-memory estimate used to admit the row before
    -- PostgreSQL transfers and serde materializes its JSON values.
    local_context_bytes BIGINT CHECK (local_context_bytes IS NULL OR local_context_bytes >= 0),
    CONSTRAINT ck_response_affinities_local_context_size CHECK (
        local_response IS NULL OR local_context_bytes IS NOT NULL
    ),
    -- Durable billing/TPM hand-off. Background Responses resources are polled
    -- until terminal; already-terminal generation requests are replayed directly.
    settlement JSONB,
    settlement_next_poll_at TIMESTAMPTZ,
    settlement_lease_until TIMESTAMPTZ,
    -- Defensive tombstone for internal cleanup paths. Public resource deletion
    -- is rejected while settlement is pending so the worker can keep polling.
    deleted_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, response_id),
    CONSTRAINT ck_response_affinities_account_owner CHECK (
        account_id IS NOT NULL OR (
            NOT is_reservation
            AND (
                (
                    settlement IS NULL
                    AND deleted_at IS NULL
                    AND local_response IS NOT NULL
                    AND local_context IS NOT NULL
                    AND local_context ? 'upstream_previous_response_id'
                    AND local_context->'upstream_previous_response_id' = 'null'::JSONB
                )
                OR (
                    response_id LIKE 'resp_kc_settlement_%'
                    AND settlement IS NOT NULL
                    AND settlement->>'terminal_status' IS NOT NULL
                    AND settlement->>'account_id' = '00000000-0000-0000-0000-000000000000'
                    AND settlement_next_poll_at IS NOT NULL
                    AND deleted_at IS NOT NULL
                    AND local_response IS NULL
                    AND local_context IS NULL
                )
            )
        )
    ),
    CONSTRAINT fk_response_affinities_membership FOREIGN KEY (tenant_id,user_id) REFERENCES tenant_memberships(tenant_id,user_id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_response_affinities_account
    ON response_affinities(account_id);
CREATE INDEX IF NOT EXISTS idx_response_affinities_user
    ON response_affinities(tenant_id,user_id,response_id);
CREATE INDEX IF NOT EXISTS idx_response_affinities_expires
    ON response_affinities(expires_at);
CREATE INDEX IF NOT EXISTS idx_response_affinities_local_warmups
    ON response_affinities(tenant_id)
    WHERE local_response IS NOT NULL AND NOT is_reservation AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_response_affinities_settlement_due
    ON response_affinities(settlement_next_poll_at, tenant_id, response_id COLLATE "C")
    WHERE settlement IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_response_affinities_settlement_recovery
    ON response_affinities(tenant_id, response_id)
    WHERE settlement IS NOT NULL;

-- pricing_models: 模型定价表
CREATE TABLE IF NOT EXISTS pricing_models (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope_type VARCHAR(20) NOT NULL DEFAULT 'tenant',
    tenant_id UUID REFERENCES tenants(id) ON DELETE RESTRICT,
    model_name VARCHAR(100) NOT NULL,
    billing_dimension VARCHAR(50) NOT NULL,
    currency VARCHAR(10) NOT NULL DEFAULT 'CNY',
    input_price_per_1k DECIMAL(20, 10) NOT NULL,
    output_price_per_1k DECIMAL(20, 10) NOT NULL,
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    version BIGINT NOT NULL DEFAULT 1,
    effective_from TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    effective_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE NULLS NOT DISTINCT (tenant_id, model_name, billing_dimension),
    CONSTRAINT ck_pricing_models_scope CHECK (
        (scope_type = 'platform' AND tenant_id IS NULL)
        OR (scope_type = 'tenant' AND tenant_id IS NOT NULL)
    ),
    CONSTRAINT ck_pricing_models_billing_dimension CHECK (
        billing_dimension IN ('node', 'provideraccount')
    ),
    CONSTRAINT ck_pricing_models_model_name_nonempty CHECK (BTRIM(model_name) <> ''),
    CONSTRAINT ck_pricing_models_currency_nonempty CHECK (BTRIM(currency) <> ''),
    CONSTRAINT ck_pricing_models_input_price_nonnegative CHECK (input_price_per_1k >= 0),
    CONSTRAINT ck_pricing_models_output_price_nonnegative CHECK (output_price_per_1k >= 0),
    CONSTRAINT ck_pricing_models_version_positive CHECK (version > 0),
    CONSTRAINT ck_pricing_models_effective_window CHECK (
        effective_until IS NULL OR effective_until > effective_from
    )
);

CREATE INDEX IF NOT EXISTS idx_pricing_models_tenant_id ON pricing_models(tenant_id);
CREATE INDEX IF NOT EXISTS idx_pricing_models_model ON pricing_models(model_name);
CREATE INDEX IF NOT EXISTS idx_pricing_models_billing_dimension ON pricing_models(billing_dimension);
CREATE INDEX IF NOT EXISTS idx_pricing_models_default ON pricing_models(is_default) WHERE is_default = TRUE;
CREATE INDEX IF NOT EXISTS idx_pricing_models_lookup
    ON pricing_models(model_name, billing_dimension, tenant_id, effective_from, effective_until);
CREATE UNIQUE INDEX IF NOT EXISTS uk_pricing_models_default_scope
    ON pricing_models(model_name, billing_dimension, tenant_id)
    WHERE is_default = TRUE;

COMMENT ON COLUMN pricing_models.billing_dimension IS '计费维度: node 或 provideraccount';
COMMENT ON COLUMN pricing_models.scope_type IS '定价范围：platform 或 tenant';
COMMENT ON COLUMN pricing_models.version IS '管理端乐观并发版本号';

-- 定价管理审计事件。事件表不引用业务行，避免删除定价后丢失变更证据。
CREATE TABLE IF NOT EXISTS pricing_audit_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    actor_user_id UUID NOT NULL,
    action VARCHAR(32) NOT NULL,
    pricing_id UUID NOT NULL,
    scope_type VARCHAR(20) NOT NULL DEFAULT 'tenant',
    tenant_id UUID,
    model_name VARCHAR(100) NOT NULL,
    billing_dimension VARCHAR(50) NOT NULL,
    before_state JSONB,
    after_state JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ck_pricing_audit_scope CHECK (
        (scope_type = 'platform' AND tenant_id IS NULL)
        OR (scope_type = 'tenant' AND tenant_id IS NOT NULL)
    ),
    CONSTRAINT ck_pricing_audit_action CHECK (
        action IN ('create', 'update', 'delete', 'make_default')
    )
);

CREATE INDEX IF NOT EXISTS idx_pricing_audit_pricing_created
    ON pricing_audit_events(pricing_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_pricing_audit_tenant_created
    ON pricing_audit_events(tenant_id, created_at DESC);
-- usage_logs: 计费主账本，不可变
CREATE TABLE IF NOT EXISTS usage_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    request_id UUID NOT NULL UNIQUE,
    -- Stable logical identity for an end-to-end idempotent request. The
    -- concrete request_id remains the first HTTP attempt's trace identity.
    idempotency_id UUID UNIQUE,
    tenant_id UUID NOT NULL,
    user_id UUID NOT NULL,
    produce_ai_key_id UUID NOT NULL,
    model_name VARCHAR(100) NOT NULL,
    provider_name VARCHAR(50) NOT NULL,
    account_id UUID NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    total_tokens INTEGER NOT NULL,
    input_unit_price_snapshot DECIMAL(20, 10) NOT NULL,
    output_unit_price_snapshot DECIMAL(20, 10) NOT NULL,
    user_amount DECIMAL(20, 10) NOT NULL,
    currency VARCHAR(10) NOT NULL DEFAULT 'CNY',
    usage_source VARCHAR(20) NOT NULL,
    status VARCHAR(20) NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    finished_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Distribution records carry the tenant explicitly.  Keep a matching
    -- unique key so PostgreSQL can enforce that they reference a usage row in
    -- the same tenant rather than merely an existing global UUID.
    CONSTRAINT uk_usage_logs_tenant_id_id UNIQUE (tenant_id, id),
    CONSTRAINT fk_usage_logs_membership FOREIGN KEY (tenant_id,user_id) REFERENCES tenant_memberships(tenant_id,user_id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_usage_logs_tenant ON usage_logs(tenant_id);
CREATE INDEX IF NOT EXISTS idx_usage_logs_user ON usage_logs(user_id);
CREATE INDEX IF NOT EXISTS idx_usage_logs_user_tenant_created
    ON usage_logs(tenant_id,user_id,created_at DESC,id DESC);
CREATE INDEX IF NOT EXISTS idx_usage_logs_produce_ai_key ON usage_logs(produce_ai_key_id);
CREATE INDEX IF NOT EXISTS idx_usage_logs_created ON usage_logs(created_at);
CREATE INDEX IF NOT EXISTS idx_usage_logs_request ON usage_logs(request_id);

-- ============================================================================
-- Gateway 请求监控追踪
-- ============================================================================

CREATE TABLE IF NOT EXISTS gateway_requests (
    request_id UUID PRIMARY KEY,
    client_request_id VARCHAR(128),
    tenant_id UUID NOT NULL,
    user_id UUID NOT NULL,
    produce_ai_key_id UUID NOT NULL,
    protocol VARCHAR(32) NOT NULL,
    request_path VARCHAR(255) NOT NULL,
    requested_model VARCHAR(255) NOT NULL,
    is_stream BOOLEAN NOT NULL,
    route_type VARCHAR(32),
    status VARCHAR(32) NOT NULL,
    error_origin VARCHAR(32),
    error_category VARCHAR(32),
    error_code VARCHAR(128),
    received_at TIMESTAMPTZ NOT NULL,
    client_first_content_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    billing_status VARCHAR(32) NOT NULL,
    trace_quality VARCHAR(16) NOT NULL DEFAULT 'actual',
    trace_version INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ck_gateway_requests_protocol CHECK (protocol IN ('openai', 'anthropic')),
    CONSTRAINT ck_gateway_requests_route_type CHECK (route_type IS NULL OR route_type IN ('provider_account', 'passthrough_binding', 'node')),
    CONSTRAINT ck_gateway_requests_status CHECK (status IN ('received', 'routing', 'queued', 'running', 'succeeded', 'failed', 'timed_out', 'cancelled')),
    CONSTRAINT ck_gateway_requests_error_origin CHECK (error_origin IS NULL OR error_origin IN ('client', 'gateway', 'upstream', 'node')),
    CONSTRAINT ck_gateway_requests_error_category CHECK (error_category IS NULL OR error_category IN ('authorization', 'invalid_request', 'balance', 'rate_limit', 'transport', 'timeout', 'upstream_4xx', 'upstream_5xx', 'protocol', 'client_disconnect', 'node_expired', 'node_failed', 'internal')),
    CONSTRAINT ck_gateway_requests_billing_status CHECK (billing_status IN ('pending', 'succeeded', 'failed', 'not_applicable')),
    CONSTRAINT ck_gateway_requests_trace_quality CHECK (trace_quality IN ('actual', 'derived', 'partial')),
    CONSTRAINT ck_gateway_requests_terminal_time CHECK ((status IN ('succeeded', 'failed', 'timed_out', 'cancelled')) = (finished_at IS NOT NULL)),
    CONSTRAINT ck_gateway_requests_first_content_time CHECK (client_first_content_at IS NULL OR client_first_content_at >= received_at),
    CONSTRAINT ck_gateway_requests_finished_time CHECK (finished_at IS NULL OR finished_at >= received_at),
    CONSTRAINT fk_gateway_requests_membership FOREIGN KEY (tenant_id,user_id) REFERENCES tenant_memberships(tenant_id,user_id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS gateway_request_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    request_id UUID NOT NULL REFERENCES gateway_requests(request_id) ON DELETE CASCADE,
    attempt_no INTEGER NOT NULL,
    attempt_kind VARCHAR(16) NOT NULL,
    route_type VARCHAR(32) NOT NULL,
    model VARCHAR(255) NOT NULL,
    status VARCHAR(32) NOT NULL,
    is_final BOOLEAN NOT NULL DEFAULT FALSE,
    provider_name VARCHAR(64),
    account_id UUID,
    node_task_id UUID,
    node_id UUID,
    session_id UUID,
    lease_id UUID,
    upstream_request_id VARCHAR(128),
    http_status INTEGER,
    retryable BOOLEAN,
    error_origin VARCHAR(32),
    error_category VARCHAR(32),
    error_code VARCHAR(128),
    error_summary VARCHAR(512),
    started_at TIMESTAMPTZ NOT NULL,
    headers_received_at TIMESTAMPTZ,
    first_content_at TIMESTAMPTZ,
    stream_end_reason VARCHAR(32),
    stream_error_count INTEGER,
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uk_gateway_request_attempt_no UNIQUE (request_id, attempt_no),
    CONSTRAINT ck_gateway_attempt_no CHECK (attempt_no > 0),
    CONSTRAINT ck_gateway_attempt_kind CHECK (attempt_kind IN ('primary', 'fallback', 'retry', 'reclaim')),
    CONSTRAINT ck_gateway_attempt_route_type CHECK (route_type IN ('provider_account', 'passthrough_binding', 'node')),
    CONSTRAINT ck_gateway_attempt_status CHECK (status IN ('running', 'succeeded', 'failed', 'timed_out', 'cancelled', 'expired')),
    CONSTRAINT ck_gateway_attempt_error_origin CHECK (error_origin IS NULL OR error_origin IN ('upstream', 'gateway', 'node')),
    CONSTRAINT ck_gateway_attempt_error_category CHECK (error_category IS NULL OR error_category IN ('authorization', 'invalid_request', 'balance', 'rate_limit', 'transport', 'timeout', 'upstream_4xx', 'upstream_5xx', 'protocol', 'client_disconnect', 'node_expired', 'node_failed', 'internal')),
    CONSTRAINT ck_gateway_attempt_stream_end CHECK (stream_end_reason IS NULL OR stream_end_reason IN ('completed', 'upstream_error', 'protocol_error', 'client_disconnect', 'cancelled', 'timeout', 'truncated')),
    CONSTRAINT ck_gateway_attempt_stream_errors CHECK (stream_error_count IS NULL OR stream_error_count >= 0),
    CONSTRAINT ck_gateway_attempt_headers_time CHECK (headers_received_at IS NULL OR headers_received_at >= started_at),
    CONSTRAINT ck_gateway_attempt_first_time CHECK (first_content_at IS NULL OR first_content_at >= started_at),
    CONSTRAINT ck_gateway_attempt_finished_time CHECK (finished_at IS NULL OR finished_at >= started_at),
    CONSTRAINT ck_gateway_attempt_content_finished CHECK (first_content_at IS NULL OR finished_at IS NULL OR first_content_at <= finished_at),
    CONSTRAINT ck_gateway_attempt_terminal_time CHECK ((status = 'running') = (finished_at IS NULL)),
    CONSTRAINT ck_gateway_attempt_target CHECK (
        (route_type IN ('provider_account', 'passthrough_binding') AND provider_name IS NOT NULL AND account_id IS NOT NULL AND node_task_id IS NULL AND node_id IS NULL AND session_id IS NULL AND lease_id IS NULL)
        OR
        (route_type = 'node' AND provider_name IS NULL AND account_id IS NULL AND node_task_id IS NOT NULL AND node_id IS NOT NULL AND session_id IS NOT NULL AND lease_id IS NOT NULL)
    )
);

CREATE INDEX IF NOT EXISTS idx_gateway_requests_received ON gateway_requests(received_at DESC, request_id DESC);
CREATE INDEX IF NOT EXISTS idx_gateway_requests_status_received ON gateway_requests(status, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_gateway_requests_route_received ON gateway_requests(route_type, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_gateway_requests_tenant_received ON gateway_requests(tenant_id, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_gateway_requests_user_received ON gateway_requests(user_id, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_gateway_requests_key_received ON gateway_requests(produce_ai_key_id, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_gateway_requests_model_received ON gateway_requests(requested_model, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_gateway_requests_client_id ON gateway_requests(client_request_id) WHERE client_request_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_gateway_requests_pending_billing_finished ON gateway_requests(finished_at)
    WHERE billing_status = 'pending' AND finished_at IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uk_gateway_request_final_attempt ON gateway_request_attempts(request_id) WHERE is_final;
CREATE UNIQUE INDEX IF NOT EXISTS uk_gateway_node_attempt_lease ON gateway_request_attempts(node_task_id, lease_id) WHERE route_type = 'node';
CREATE INDEX IF NOT EXISTS idx_gateway_attempt_account_started ON gateway_request_attempts(account_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_gateway_attempt_provider_started ON gateway_request_attempts(provider_name, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_gateway_attempt_node_started ON gateway_request_attempts(node_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_gateway_attempt_upstream_id ON gateway_request_attempts(upstream_request_id) WHERE upstream_request_id IS NOT NULL;

-- distribution_records: 二级分销记录
CREATE TABLE IF NOT EXISTS distribution_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    usage_log_id UUID NOT NULL REFERENCES usage_logs(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    beneficiary_scope VARCHAR(20) NOT NULL DEFAULT 'tenant_member',
    beneficiary_id UUID,
    share_amount DECIMAL(20, 10) NOT NULL,
    share_ratio DECIMAL(5, 4) NOT NULL,
    -- 分销层级；参与唯一约束以提供写入幂等性
    level VARCHAR(20) NOT NULL DEFAULT 'level1',
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    settled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uk_distribution_records_unique
        UNIQUE NULLS NOT DISTINCT (usage_log_id, beneficiary_scope, beneficiary_id, level),
    CONSTRAINT ck_distribution_records_beneficiary CHECK (
        (beneficiary_scope = 'everyone' AND beneficiary_id IS NULL)
        OR (beneficiary_scope = 'tenant_member' AND beneficiary_id IS NOT NULL)
    ),
    CONSTRAINT fk_distribution_records_beneficiary_membership
        FOREIGN KEY (tenant_id, beneficiary_id)
        REFERENCES tenant_memberships(tenant_id, user_id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_distribution_records_usage_tenant
        FOREIGN KEY (tenant_id, usage_log_id)
        REFERENCES usage_logs(tenant_id, id)
        ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_distribution_records_tenant_id ON distribution_records(tenant_id);
CREATE INDEX IF NOT EXISTS idx_distribution_records_usage_log_id ON distribution_records(usage_log_id);
CREATE INDEX IF NOT EXISTS idx_distribution_records_beneficiary_id ON distribution_records(beneficiary_id);
CREATE INDEX IF NOT EXISTS idx_distribution_records_status ON distribution_records(status);
CREATE INDEX IF NOT EXISTS idx_distribution_records_level ON distribution_records(level);
COMMENT ON CONSTRAINT uk_distribution_records_unique ON distribution_records IS
'幂等性保护：防止同一 usage_log 对同一受益人的重复分销记录';
-- tenant_distribution_rules: 租户分销规则
CREATE TABLE IF NOT EXISTS tenant_distribution_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    beneficiary_scope VARCHAR(20) NOT NULL DEFAULT 'tenant_member',
    beneficiary_id UUID,
    name VARCHAR(255) NOT NULL DEFAULT '默认分销规则',
    description TEXT,
    commission_rate DECIMAL(5, 4) NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    effective_from TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    effective_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE NULLS NOT DISTINCT (tenant_id, beneficiary_scope, beneficiary_id, effective_from),
    CONSTRAINT ck_tenant_distribution_rules_beneficiary CHECK (
        (beneficiary_scope = 'everyone' AND beneficiary_id IS NULL)
        OR (beneficiary_scope = 'tenant_member' AND beneficiary_id IS NOT NULL)
    ),
    CONSTRAINT fk_tenant_distribution_rules_beneficiary_membership
        FOREIGN KEY (tenant_id, beneficiary_id)
        REFERENCES tenant_memberships(tenant_id, user_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_tenant_distribution_rules_tenant ON tenant_distribution_rules(tenant_id);
CREATE INDEX IF NOT EXISTS idx_tenant_distribution_rules_active ON tenant_distribution_rules(is_active) WHERE is_active = TRUE;
-- pending_registrations: 待完成注册表
-- 用于邮箱验证码注册流程，在验证码验证成功前暂存注册占位状态

CREATE TABLE IF NOT EXISTS pending_registrations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email VARCHAR(255) NOT NULL UNIQUE,
    -- 首次触达时锁定的推荐码（可选）
    referral_code UUID REFERENCES users(id) ON DELETE SET NULL,
    -- Argon2 哈希后的 6 位验证码
    verification_code_hash VARCHAR(255) NOT NULL,
    -- 验证码过期时间（默认 10 分钟）
    expires_at TIMESTAMPTZ NOT NULL,
    -- 已尝试验证次数
    verify_attempts INTEGER NOT NULL DEFAULT 0,
    -- 验证码发送次数
    resend_count INTEGER NOT NULL DEFAULT 1,
    -- 最近一次发送时间
    last_sent_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- 发起请求的客户端 IP（可选）
    requested_from_ip TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_pending_registrations_email ON pending_registrations(email);
CREATE INDEX IF NOT EXISTS idx_pending_registrations_expires ON pending_registrations(expires_at);
CREATE INDEX IF NOT EXISTS idx_pending_registrations_referral_code ON pending_registrations(referral_code);
-- user_credentials: 用户密码凭证表
-- 存储用户密码哈希和登录安全相关信息

CREATE TABLE IF NOT EXISTS user_credentials (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- 密码哈希 (argon2id)
    password_hash VARCHAR(255) NOT NULL,
    -- 邮箱验证状态
    email_verified BOOLEAN NOT NULL DEFAULT FALSE,
    email_verified_at TIMESTAMPTZ,
    -- 登录失败计数（用于防护暴力破解）
    failed_login_attempts INTEGER NOT NULL DEFAULT 0,
    locked_until TIMESTAMPTZ,
    -- 最后登录信息
    last_login_at TIMESTAMPTZ,
    last_login_ip TEXT,
    -- 时间戳
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- 唯一约束：一个用户只有一个凭证记录
    UNIQUE(user_id)
);

-- 索引
CREATE INDEX IF NOT EXISTS idx_user_credentials_user ON user_credentials(user_id);
CREATE INDEX IF NOT EXISTS idx_user_credentials_locked ON user_credentials(locked_until) 
    WHERE locked_until IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_user_credentials_verified ON user_credentials(email_verified) 
    WHERE email_verified = FALSE;
-- password_resets: 密码重置令牌表
-- 管理用户密码重置流程

CREATE TABLE IF NOT EXISTS password_resets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- 重置令牌
    token VARCHAR(255) NOT NULL UNIQUE,
    -- 令牌过期时间（短时效，如 1 小时）
    expires_at TIMESTAMPTZ NOT NULL,
    -- 是否已使用
    used BOOLEAN NOT NULL DEFAULT FALSE,
    used_at TIMESTAMPTZ,
    -- 请求来源 IP
    requested_from_ip INET,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 索引
CREATE INDEX IF NOT EXISTS idx_password_resets_token ON password_resets(token);
CREATE INDEX IF NOT EXISTS idx_password_resets_expires ON password_resets(expires_at) 
    WHERE used = FALSE;
CREATE INDEX IF NOT EXISTS idx_password_resets_user ON password_resets(user_id);
-- user_referrals: 用户推荐关系表
-- 用于存储谁推荐了谁，支持二级分销

CREATE TABLE IF NOT EXISTS user_referrals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 被推荐人（新用户）
    user_id UUID NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    -- 一级推荐人
    level1_referrer_id UUID REFERENCES users(id) ON DELETE SET NULL,
    -- 二级推荐人（推荐人的推荐人）
    level2_referrer_id UUID REFERENCES users(id) ON DELETE SET NULL,
    -- 推荐来源（可选，如推荐码、链接等）
    source VARCHAR(255),
    -- 推荐状态: pending, active, expired
    status VARCHAR(50) NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 索引
CREATE INDEX IF NOT EXISTS idx_user_referrals_user ON user_referrals(user_id);
CREATE INDEX IF NOT EXISTS idx_user_referrals_level1 ON user_referrals(level1_referrer_id);
CREATE INDEX IF NOT EXISTS idx_user_referrals_level2 ON user_referrals(level2_referrer_id);
CREATE INDEX IF NOT EXISTS idx_user_referrals_status ON user_referrals(status);
-- 支付订单表
-- 用于存储用户充值订单记录

CREATE TABLE IF NOT EXISTS payment_orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 租户ID
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    -- 用户ID
    user_id UUID NOT NULL,
    -- 商户订单号（外部订单号）
    out_trade_no VARCHAR(64) NOT NULL UNIQUE,
    -- 通用渠道交易号展示字段
    trade_no VARCHAR(64),
    -- 支付渠道交易号
    provider_trade_no VARCHAR(64),
    -- 订单金额（单位：元）
    amount DECIMAL(12, 2) NOT NULL,
    -- 币种（默认CNY）
    currency VARCHAR(8) NOT NULL DEFAULT 'CNY',
    -- 订单状态: pending/paid/failed/closed
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    -- 支付方式: alipay/wechatpay
    payment_method VARCHAR(20) NOT NULL DEFAULT 'alipay',
    -- 支付场景: page/wap/qr/native
    payment_scene VARCHAR(20) NOT NULL DEFAULT 'page',
    -- 商品标题
    subject VARCHAR(256) NOT NULL,
    -- 商品描述
    body TEXT,
    -- 支付时间
    paid_at TIMESTAMPTZ,
    -- 关闭时间
    closed_at TIMESTAMPTZ,
    -- 过期时间
    expired_at TIMESTAMPTZ NOT NULL,
    -- 支付URL（用于前端跳转）
    pay_url TEXT,
    -- 回调通知原始数据
    notify_data JSONB,
    -- 支付渠道返回的非敏感展示数据
    provider_payload JSONB,
    -- 最近一次渠道错误码
    last_error_code VARCHAR(64),
    -- 最近一次渠道错误信息
    last_error_message TEXT,
    -- 最近一次主动同步时间
    last_synced_at TIMESTAMPTZ,
    -- 备注信息
    remarks TEXT,
    -- 创建时间
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- 更新时间
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fk_payment_orders_membership FOREIGN KEY (tenant_id,user_id) REFERENCES tenant_memberships(tenant_id,user_id) ON DELETE RESTRICT
);

-- 创建索引
CREATE INDEX IF NOT EXISTS idx_payment_orders_tenant_id ON payment_orders(tenant_id);
CREATE INDEX IF NOT EXISTS idx_payment_orders_user_id ON payment_orders(user_id);
CREATE INDEX IF NOT EXISTS idx_payment_orders_out_trade_no ON payment_orders(out_trade_no);
CREATE INDEX IF NOT EXISTS idx_payment_orders_trade_no ON payment_orders(trade_no);
CREATE INDEX IF NOT EXISTS idx_payment_orders_status ON payment_orders(status);
CREATE INDEX IF NOT EXISTS idx_payment_orders_created_at ON payment_orders(created_at);
CREATE UNIQUE INDEX IF NOT EXISTS uk_payment_orders_provider_trade_no
    ON payment_orders(payment_method, provider_trade_no)
    WHERE provider_trade_no IS NOT NULL;

-- 添加注释
COMMENT ON TABLE payment_orders IS '支付订单表';
COMMENT ON COLUMN payment_orders.id IS '订单ID';
COMMENT ON COLUMN payment_orders.tenant_id IS '租户ID';
COMMENT ON COLUMN payment_orders.user_id IS '用户ID';
COMMENT ON COLUMN payment_orders.out_trade_no IS '商户订单号（外部订单号）';
COMMENT ON COLUMN payment_orders.trade_no IS '通用渠道交易号展示字段';
COMMENT ON COLUMN payment_orders.provider_trade_no IS '支付渠道交易号';
COMMENT ON COLUMN payment_orders.amount IS '订单金额（单位：元）';
COMMENT ON COLUMN payment_orders.currency IS '币种';
COMMENT ON COLUMN payment_orders.status IS '订单状态: pending/paid/failed/closed';
COMMENT ON COLUMN payment_orders.payment_method IS '支付方式';
COMMENT ON COLUMN payment_orders.payment_scene IS '支付场景: page/wap/qr/native';
COMMENT ON COLUMN payment_orders.subject IS '商品标题';
COMMENT ON COLUMN payment_orders.body IS '商品描述';
COMMENT ON COLUMN payment_orders.paid_at IS '支付时间';
COMMENT ON COLUMN payment_orders.closed_at IS '关闭时间';
COMMENT ON COLUMN payment_orders.expired_at IS '过期时间';
COMMENT ON COLUMN payment_orders.pay_url IS '支付URL';
COMMENT ON COLUMN payment_orders.notify_data IS '回调通知原始数据';
COMMENT ON COLUMN payment_orders.provider_payload IS '支付渠道返回的非敏感展示数据';
COMMENT ON COLUMN payment_orders.last_error_code IS '最近一次渠道错误码';
COMMENT ON COLUMN payment_orders.last_error_message IS '最近一次渠道错误信息';
COMMENT ON COLUMN payment_orders.last_synced_at IS '最近一次主动同步时间';
COMMENT ON COLUMN payment_orders.remarks IS '备注信息';

-- 支付通知去重与处理状态
CREATE TABLE IF NOT EXISTS payment_notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    payment_method VARCHAR(20) NOT NULL,
    provider_event_id VARCHAR(128) NOT NULL,
    order_id UUID REFERENCES payment_orders(id) ON DELETE SET NULL,
    out_trade_no VARCHAR(64) NOT NULL,
    processing_status VARCHAR(24) NOT NULL DEFAULT 'received',
    payload_digest VARCHAR(64) NOT NULL,
    failure_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    processed_at TIMESTAMPTZ,
    UNIQUE(payment_method, provider_event_id)
);

CREATE INDEX IF NOT EXISTS idx_payment_notifications_order
    ON payment_notifications(order_id);
CREATE INDEX IF NOT EXISTS idx_payment_notifications_status
    ON payment_notifications(processing_status, created_at DESC);

-- 支付回调安全拒绝事件
CREATE TABLE IF NOT EXISTS payment_security_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    payment_method VARCHAR(20) NOT NULL,
    event_type VARCHAR(40) NOT NULL,
    request_id VARCHAR(128),
    source_ip VARCHAR(64),
    payload_digest VARCHAR(64) NOT NULL,
    detail TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_payment_security_events_created
    ON payment_security_events(created_at DESC);

-- 支付渠道配置验证与集群运行状态
CREATE TABLE IF NOT EXISTS payment_provider_states (
    payment_method VARCHAR(20) PRIMARY KEY,
    config_version BIGINT NOT NULL DEFAULT 1,
    config_fingerprint VARCHAR(64),
    verified_config_fingerprint VARCHAR(64),
    verified_at TIMESTAMPTZ,
    circuit_state VARCHAR(20) NOT NULL DEFAULT 'available',
    last_error_code VARCHAR(64),
    last_error_message TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_payment_provider_states_circuit
        CHECK (circuit_state IN ('available', 'degraded', 'unavailable'))
);

INSERT INTO payment_provider_states(payment_method)
VALUES ('alipay'), ('wechatpay')
ON CONFLICT (payment_method) DO NOTHING;

-- 用户余额表
CREATE TABLE IF NOT EXISTS user_balances (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 租户ID
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    -- 用户ID
    user_id UUID NOT NULL,
    -- 可用余额（单位：元，10 位小数与计费精度对齐）
    available_balance DECIMAL(20, 10) NOT NULL DEFAULT 0,
    -- 冻结余额（单位：元）
    frozen_balance DECIMAL(20, 10) NOT NULL DEFAULT 0,
    -- 累计充值金额
    total_recharged DECIMAL(20, 10) NOT NULL DEFAULT 0,
    -- 累计消费金额
    total_consumed DECIMAL(20, 10) NOT NULL DEFAULT 0,
    -- 创建时间
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- 更新时间
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- available_balance may be negative when a ledger-backed late settlement
    -- records auditable debt. Frozen funds and cumulative counters may not.
    CONSTRAINT ck_user_balances_frozen_nonnegative CHECK (frozen_balance >= 0),
    CONSTRAINT ck_user_balances_total_recharged_nonnegative CHECK (total_recharged >= 0),
    CONSTRAINT ck_user_balances_total_consumed_nonnegative CHECK (total_consumed >= 0),
    CONSTRAINT fk_user_balances_membership FOREIGN KEY (tenant_id,user_id) REFERENCES tenant_memberships(tenant_id,user_id) ON DELETE RESTRICT
);

-- 创建索引
CREATE INDEX IF NOT EXISTS idx_user_balances_tenant_id ON user_balances(tenant_id);
CREATE INDEX IF NOT EXISTS idx_user_balances_user_id ON user_balances(user_id);
CREATE UNIQUE INDEX IF NOT EXISTS uq_user_balances_tenant_user ON user_balances(tenant_id,user_id);

-- 添加注释
COMMENT ON TABLE user_balances IS '用户余额表';
COMMENT ON COLUMN user_balances.available_balance IS '可用余额（单位：元）';
COMMENT ON COLUMN user_balances.frozen_balance IS '冻结余额（单位：元）';
COMMENT ON COLUMN user_balances.total_recharged IS '累计充值金额';
COMMENT ON COLUMN user_balances.total_consumed IS '累计消费金额';

-- API 请求余额预留。预留与 billing_request_id 一一对应，确保并发请求
-- 不能共同消费同一份可用余额；终态结算与 usage_logs 绑定并可安全重放。
CREATE TABLE IF NOT EXISTS balance_reservations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    request_id UUID NOT NULL UNIQUE,
    -- 每次处理器取得同一逻辑请求的预留所有权时轮换。旧处理器只能用
    -- 自己持有的 token 释放，不能误释放幂等重试重新接管的预留。
    owner_token UUID NOT NULL DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    user_id UUID NOT NULL,
    amount DECIMAL(20, 10) NOT NULL CHECK (amount >= 0),
    status VARCHAR(20) NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'settled', 'released', 'expired')),
    usage_log_id UUID REFERENCES usage_logs(id),
    expires_at TIMESTAMPTZ NOT NULL,
    settled_at TIMESTAMPTZ,
    released_at TIMESTAMPTZ,
    release_kind VARCHAR(20),
    release_reason TEXT,
    released_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ck_balance_reservations_release_audit CHECK (
        (status = 'released' AND released_at IS NOT NULL
            AND release_kind IN ('automatic', 'administrative')
            AND release_reason IS NOT NULL AND BTRIM(release_reason) <> ''
            AND CHAR_LENGTH(BTRIM(release_reason)) <= 1000)
        OR
        (status <> 'released' AND released_at IS NULL
            AND release_kind IS NULL AND release_reason IS NULL
            AND released_by IS NULL)
    ),
    CONSTRAINT ck_balance_reservations_settlement_audit CHECK (
        (status = 'settled' AND usage_log_id IS NOT NULL AND settled_at IS NOT NULL)
        OR
        (status <> 'settled' AND usage_log_id IS NULL AND settled_at IS NULL)
    ),
    CONSTRAINT fk_balance_reservations_membership FOREIGN KEY (tenant_id,user_id) REFERENCES tenant_memberships(tenant_id,user_id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_balance_reservations_user_id
    ON balance_reservations(user_id);
CREATE INDEX IF NOT EXISTS idx_balance_reservations_active_user_created
    ON balance_reservations(user_id, created_at DESC, id DESC)
    INCLUDE (amount)
    WHERE status = 'active';
CREATE INDEX IF NOT EXISTS idx_balance_reservations_active_user_expiry
    ON balance_reservations(user_id, expires_at, id)
    INCLUDE (amount)
    WHERE status = 'active';
CREATE INDEX IF NOT EXISTS idx_balance_reservations_active_expiry
    ON balance_reservations(expires_at)
    WHERE status = 'active';
CREATE UNIQUE INDEX IF NOT EXISTS uk_balance_reservations_usage_log
    ON balance_reservations(usage_log_id)
    WHERE usage_log_id IS NOT NULL;

COMMENT ON TABLE balance_reservations IS 'API 请求预付费余额预留';
COMMENT ON COLUMN balance_reservations.request_id IS '稳定 billing_request_id';
COMMENT ON COLUMN balance_reservations.owner_token IS '当前处理器持有的预留所有权 token';
COMMENT ON COLUMN balance_reservations.amount IS '从可用余额转入冻结余额的最大预留金额';
COMMENT ON COLUMN balance_reservations.released_at IS '主动释放预留的时间；过期回收不使用此字段';
COMMENT ON COLUMN balance_reservations.release_kind IS '释放类型：automatic 自动补偿；administrative 管理员强制释放';
COMMENT ON COLUMN balance_reservations.release_reason IS '主动释放原因，包括自动补偿与管理员操作';
COMMENT ON COLUMN balance_reservations.released_by IS '管理员释放时的操作用户；自动补偿为空';

-- Append-only snapshots of reservation ownership and terminal transitions.
-- The live reservation row may be reused after an automatic release or
-- expiry, so audit evidence must not depend on fields retained in that row.
CREATE TABLE IF NOT EXISTS balance_reservation_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Sequence is the authoritative event order. NOW() is transaction-stable
    -- and UUIDv4 is unordered, so neither can prove the order of multiple
    -- transitions recorded by one transaction.
    event_sequence BIGINT GENERATED ALWAYS AS IDENTITY NOT NULL UNIQUE,
    -- Intentionally no foreign key: reservation/user/tenant deletion must not
    -- erase immutable financial audit evidence.
    reservation_id UUID NOT NULL,
    request_id UUID NOT NULL,
    owner_token UUID NOT NULL,
    tenant_id UUID NOT NULL,
    user_id UUID NOT NULL,
    event_type VARCHAR(20) NOT NULL
        CHECK (event_type IN ('reserved', 'reowned', 'resized', 'settled', 'released', 'expired', 'updated')),
    amount DECIMAL(20, 10) NOT NULL CHECK (amount >= 0),
    status VARCHAR(20) NOT NULL
        CHECK (status IN ('active', 'settled', 'released', 'expired')),
    usage_log_id UUID,
    expires_at TIMESTAMPTZ NOT NULL,
    settled_at TIMESTAMPTZ,
    released_at TIMESTAMPTZ,
    release_kind VARCHAR(20),
    release_reason TEXT,
    released_by UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_balance_reservation_events_request_sequence
    ON balance_reservation_events(request_id, event_sequence);
CREATE INDEX IF NOT EXISTS idx_balance_reservation_events_reservation_sequence
    ON balance_reservation_events(reservation_id, event_sequence);

CREATE OR REPLACE FUNCTION record_balance_reservation_event()
RETURNS TRIGGER AS $$
DECLARE
    reservation_event_type VARCHAR(20);
BEGIN
    IF TG_OP = 'INSERT' THEN
        reservation_event_type := 'reserved';
    ELSIF NEW.owner_token IS DISTINCT FROM OLD.owner_token THEN
        reservation_event_type := 'reowned';
    ELSIF NEW.status IS DISTINCT FROM OLD.status THEN
        reservation_event_type := CASE NEW.status
            WHEN 'settled' THEN 'settled'
            WHEN 'released' THEN 'released'
            WHEN 'expired' THEN 'expired'
            ELSE 'updated'
        END;
    ELSIF NEW.amount IS DISTINCT FROM OLD.amount THEN
        reservation_event_type := 'resized';
    ELSE
        reservation_event_type := 'updated';
    END IF;

    INSERT INTO balance_reservation_events (
        reservation_id, request_id, owner_token, tenant_id, user_id,
        event_type, amount, status, usage_log_id, expires_at, settled_at,
        released_at, release_kind, release_reason, released_by
    ) VALUES (
        NEW.id, NEW.request_id, NEW.owner_token, NEW.tenant_id, NEW.user_id,
        reservation_event_type, NEW.amount, NEW.status, NEW.usage_log_id,
        NEW.expires_at, NEW.settled_at, NEW.released_at, NEW.release_kind,
        NEW.release_reason, NEW.released_by
    );
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_record_balance_reservation_event ON balance_reservations;
CREATE TRIGGER trg_record_balance_reservation_event
    AFTER INSERT OR UPDATE OF owner_token, amount, status, usage_log_id,
        expires_at, settled_at, released_at, release_kind, release_reason, released_by
    ON balance_reservations
    FOR EACH ROW
    EXECUTE FUNCTION record_balance_reservation_event();

CREATE OR REPLACE FUNCTION reject_balance_reservation_event_mutation()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'balance_reservation_events is append-only';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_reject_balance_reservation_event_mutation ON balance_reservation_events;
CREATE TRIGGER trg_reject_balance_reservation_event_mutation
    BEFORE UPDATE OR DELETE ON balance_reservation_events
    FOR EACH ROW
    EXECUTE FUNCTION reject_balance_reservation_event_mutation();

COMMENT ON TABLE balance_reservation_events IS '余额预留所有权和状态变更的不可变审计快照';
COMMENT ON COLUMN balance_reservation_events.event_sequence IS '不可变且单调递增的审计事件顺序';
COMMENT ON COLUMN balance_reservation_events.reservation_id IS '原预留 UUID 快照；故意不设外键以保留删除后的审计证据';

-- 余额变动记录表
CREATE TABLE IF NOT EXISTS balance_transactions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 租户ID
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    -- 用户ID
    user_id UUID NOT NULL,
    -- 关联订单ID（可选）
    order_id UUID REFERENCES payment_orders(id),
    -- 关联使用日志ID（可选）
    usage_log_id UUID REFERENCES usage_logs(id),
    -- 交易类型: recharge/consume/freeze/unfreeze
    transaction_type VARCHAR(20) NOT NULL,
    -- 变动金额（正数为增加，负数为减少，10 位小数与计费精度对齐）
    amount DECIMAL(20, 10) NOT NULL,
    -- 变动前余额
    balance_before DECIMAL(20, 10) NOT NULL,
    -- 变动后余额
    balance_after DECIMAL(20, 10) NOT NULL,
    -- 币种
    currency VARCHAR(8) NOT NULL DEFAULT 'CNY',
    -- 备注
    description TEXT,
    -- 创建时间
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fk_balance_transactions_membership FOREIGN KEY (tenant_id,user_id) REFERENCES tenant_memberships(tenant_id,user_id) ON DELETE RESTRICT
);

-- 创建索引
CREATE INDEX IF NOT EXISTS idx_balance_transactions_tenant_id ON balance_transactions(tenant_id);
CREATE INDEX IF NOT EXISTS idx_balance_transactions_user_id ON balance_transactions(user_id);
CREATE INDEX IF NOT EXISTS idx_balance_transactions_order_id ON balance_transactions(order_id);
CREATE INDEX IF NOT EXISTS idx_balance_transactions_usage_log_id ON balance_transactions(usage_log_id);
CREATE INDEX IF NOT EXISTS idx_balance_transactions_type ON balance_transactions(transaction_type);
CREATE INDEX IF NOT EXISTS idx_balance_transactions_created_at ON balance_transactions(created_at);
-- 同一支付订单只能产生一笔充值流水
CREATE UNIQUE INDEX IF NOT EXISTS uk_balance_transactions_recharge_order
    ON balance_transactions(order_id)
    WHERE transaction_type = 'recharge' AND order_id IS NOT NULL;
-- 同一用量主账本只能产生一笔消费流水，供崩溃恢复安全重放后置结算。
CREATE UNIQUE INDEX IF NOT EXISTS uk_balance_transactions_consume_usage_log
    ON balance_transactions(usage_log_id)
    WHERE transaction_type = 'consume' AND usage_log_id IS NOT NULL;

-- 管理员手工充值、扣款、冻结和解冻操作的持久化幂等账本。Idempotency-Key 只保存
-- SHA-256 摘要；全局唯一约束使同一 key 即使换租户、目标用户或操作者也会
-- 命中同一行并由请求指纹判定为冲突。结果快照与余额变更在同一事务完成，
-- 因而可在响应丢失后跨进程、跨副本返回完全相同的操作结果。
CREATE TABLE IF NOT EXISTS admin_balance_operations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    idempotency_key_hash VARCHAR(64) NOT NULL UNIQUE,
    request_fingerprint VARCHAR(64) NOT NULL,
    operation_type VARCHAR(20) NOT NULL
        CHECK (operation_type IN ('recharge', 'consume', 'freeze', 'unfreeze')),
    tenant_id UUID NOT NULL,
    user_id UUID NOT NULL,
    actor_user_id UUID NOT NULL,
    amount DECIMAL(20, 10) NOT NULL CHECK (amount > 0),
    reason TEXT NOT NULL CHECK (
        BTRIM(reason) <> '' AND reason = BTRIM(reason)
        AND CHAR_LENGTH(reason) <= 1000
    ),
    balance_transaction_id UUID,
    balance_before DECIMAL(20, 10),
    balance_after DECIMAL(20, 10),
    frozen_balance_after DECIMAL(20, 10),
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ck_admin_balance_operations_key_hash
        CHECK (idempotency_key_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_admin_balance_operations_request_fingerprint
        CHECK (request_fingerprint ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_admin_balance_operations_completion CHECK (
        (completed_at IS NULL AND balance_transaction_id IS NULL
            AND balance_before IS NULL AND balance_after IS NULL
            AND frozen_balance_after IS NULL)
        OR
        (completed_at IS NOT NULL AND balance_transaction_id IS NOT NULL
            AND balance_before IS NOT NULL AND balance_after IS NOT NULL
            AND frozen_balance_after IS NOT NULL AND frozen_balance_after >= 0)
    ),
    CONSTRAINT fk_admin_balance_operations_membership FOREIGN KEY (tenant_id,user_id) REFERENCES tenant_memberships(tenant_id,user_id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_admin_balance_operations_user_created
    ON admin_balance_operations(user_id, created_at DESC, id DESC);

COMMENT ON TABLE admin_balance_operations IS '管理员手工充值、扣款、冻结和解冻操作的持久化幂等账本';
COMMENT ON COLUMN admin_balance_operations.idempotency_key_hash IS 'Idempotency-Key 的 SHA-256 摘要，永不保存明文 key';
COMMENT ON COLUMN admin_balance_operations.request_fingerprint IS '规范化操作类型、租户、目标用户、操作者、金额和原因的 SHA-256';

-- 添加注释
COMMENT ON TABLE balance_transactions IS '余额变动记录表';
COMMENT ON COLUMN balance_transactions.transaction_type IS '交易类型: recharge/consume/freeze/unfreeze/tip_credit';
COMMENT ON COLUMN balance_transactions.amount IS '变动金额（正数为增加，负数为减少）';
COMMENT ON COLUMN balance_transactions.balance_before IS '变动前余额';
COMMENT ON COLUMN balance_transactions.balance_after IS '变动后余额';
-- 系统设置表
-- 存储全局系统配置，支持运行时修改

CREATE TABLE IF NOT EXISTS system_settings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 设置键名（唯一）
    key VARCHAR(100) UNIQUE NOT NULL,
    -- 设置值（以字符串形式存储）
    value TEXT NOT NULL,
    -- 值类型：string, bool, int, decimal, json
    value_type VARCHAR(20) NOT NULL DEFAULT 'string',
    -- 设置描述
    description VARCHAR(255),
    -- 是否为敏感设置（敏感设置不在日志中显示）
    is_sensitive BOOLEAN DEFAULT FALSE,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- 创建索引
-- key 上有 UNIQUE 约束，PG 自动创建唯一索引，无需额外 B-tree 索引

-- 插入默认系统设置
INSERT INTO system_settings (key, value, value_type, description) VALUES
    -- 站点设置
    ('site_name', 'KeyCompute', 'string', '站点名称'),
    ('site_description', 'Next-generation high-performance AI token compute service platform', 'string', '站点描述'),
    ('site_logo_url', '', 'string', '站点 Logo URL'),
    ('site_favicon_url', '', 'string', '站点 Favicon URL'),
    
    -- 注册设置
    ('default_user_quota', '10.00', 'decimal', '新用户默认配额（元）'),
    
    -- 限流设置
    ('default_rpm_limit', '60', 'int', '默认 RPM 限制'),
    ('default_tpm_limit', '100000', 'int', '默认 TPM 限制'),
    
    -- 系统状态
    ('maintenance_mode', 'false', 'bool', '维护模式（开启后禁止所有 API 访问）'),
    ('maintenance_message', '', 'string', '维护模式提示信息'),
    
    -- 分销设置
    ('distribution_enabled', 'true', 'bool', '是否启用分销系统'),
    ('distribution_level1_default_ratio', '0.03', 'decimal', '一级分销默认分成比例'),
    ('distribution_level2_default_ratio', '0.02', 'decimal', '二级分销默认分成比例'),
    ('distribution_min_withdraw', '10.00', 'decimal', '最低提现金额'),
    
    -- 支付设置
    ('alipay_enabled', 'false', 'bool', '是否启用支付宝支付'),
    ('wechatpay_enabled', 'false', 'bool', '是否启用微信支付'),
    ('min_recharge_amount', '1.00', 'decimal', '最小充值金额'),
    ('max_recharge_amount', '100000.00', 'decimal', '最大充值金额'),
    
    -- 安全设置
    ('login_failed_limit', '5', 'int', '登录失败次数限制'),
    ('login_lockout_minutes', '30', 'int', '登录锁定时长（分钟）'),
    -- 密码策略使用硬编码，参见 keycompute-auth/src/password/validator.rs
    -- ('password_min_length', '8', 'int', '密码最小长度'),
    -- ('password_require_uppercase', 'true', 'bool', '密码是否需要大写字母'),
    -- ('password_require_lowercase', 'true', 'bool', '密码是否需要小写字母'),
    -- ('password_require_number', 'true', 'bool', '密码是否需要数字'),
    -- ('password_require_special', 'false', 'bool', '密码是否需要特殊字符'),
    
    -- 公告设置
    ('system_notice', '', 'string', '系统公告内容'),
    ('system_notice_enabled', 'false', 'bool', '是否显示系统公告'),
    
    -- 节点租赁小费设置
    ('node_tip_ratio', '0.90', 'decimal', '节点租赁小费比例（计费金额 * 比例 = 小费）'),
    
    -- 其他设置
    ('footer_content', '', 'string', '页脚自定义内容'),
    ('about_content', '', 'string', '关于页面内容'),
    ('terms_of_service_url', '', 'string', '服务条款 URL'),
    ('privacy_policy_url', '', 'string', '隐私政策 URL')
ON CONFLICT (key) DO NOTHING;

-- 创建更新时间触发器
CREATE OR REPLACE FUNCTION update_system_settings_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trigger_update_system_settings_updated_at ON system_settings;
CREATE TRIGGER trigger_update_system_settings_updated_at
    BEFORE UPDATE ON system_settings
    FOR EACH ROW
    EXECUTE FUNCTION update_system_settings_updated_at();

-- ============================================================================
-- Node Gateway 节点相关表 (MVP)
-- ============================================================================

-- nodes: 节点注册信息表
CREATE TABLE IF NOT EXISTS nodes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    owner_user_id UUID NOT NULL,
    client_instance_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    status TEXT NOT NULL,
    capabilities_json JSONB NOT NULL,
    consecutive_failure_count INTEGER NOT NULL DEFAULT 0,
    failure_threshold INTEGER NOT NULL DEFAULT 3,
    last_heartbeat_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fk_nodes_owner_membership
        FOREIGN KEY (tenant_id, owner_user_id)
        REFERENCES tenant_memberships(tenant_id, user_id)
        ON DELETE CASCADE,
    UNIQUE (tenant_id, owner_user_id, client_instance_id)
);

CREATE INDEX IF NOT EXISTS idx_nodes_status ON nodes(status);
CREATE INDEX IF NOT EXISTS idx_nodes_last_heartbeat_at ON nodes(last_heartbeat_at);
-- 管理后台默认按创建时间倒序分页，唯一 ID 作为稳定排序兜底
CREATE INDEX IF NOT EXISTS idx_nodes_created_at_desc
    ON nodes(created_at DESC, id DESC);

-- node_sessions: 节点会话管理表
CREATE TABLE IF NOT EXISTS node_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    node_id UUID NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    session_token_hash TEXT NOT NULL UNIQUE,
    accepted_models_json JSONB NOT NULL DEFAULT '[]'::jsonb,
    registered_models_json JSONB NOT NULL DEFAULT '[]'::jsonb,
    native_operations_json JSONB NOT NULL DEFAULT '[]'::jsonb,
    native_profiles_json JSONB NOT NULL DEFAULT '[]'::jsonb,
    issued_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    accepting_tasks BOOLEAN NOT NULL DEFAULT TRUE,
    revoked_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_node_sessions_node_id_expires_at ON node_sessions(node_id, expires_at);
CREATE INDEX IF NOT EXISTS idx_node_sessions_accepted_models ON node_sessions USING GIN (accepted_models_json);
CREATE INDEX IF NOT EXISTS idx_node_sessions_native_operations ON node_sessions USING GIN (native_operations_json);

-- node_tasks: 节点任务生命周期表
CREATE TABLE IF NOT EXISTS node_tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    request_id UUID NOT NULL UNIQUE,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id UUID NOT NULL,
    model TEXT NOT NULL,
    payload_json JSONB NOT NULL,
    native_requirements_json JSONB,
    status TEXT NOT NULL,
    assigned_node_id UUID REFERENCES nodes(id) ON DELETE SET NULL,
    assigned_session_id UUID REFERENCES node_sessions(id) ON DELETE SET NULL,
    lease_id UUID,
    failure_count INTEGER NOT NULL DEFAULT 0,
    failure_threshold INTEGER NOT NULL DEFAULT 3,
    result_json JSONB,
    error_json JSONB,
    queued_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    claimed_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    deadline_at TIMESTAMPTZ NOT NULL,
    complete_grace_until TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fk_node_tasks_caller_membership
        FOREIGN KEY (tenant_id, user_id)
        REFERENCES tenant_memberships(tenant_id, user_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_node_tasks_status_model_deadline ON node_tasks(status, model, deadline_at);
CREATE INDEX IF NOT EXISTS idx_node_tasks_status_created_at_desc ON node_tasks(status, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_node_tasks_assigned_node_status ON node_tasks(assigned_node_id, status);
CREATE INDEX IF NOT EXISTS idx_node_tasks_assigned_session_lease ON node_tasks(assigned_session_id, lease_id);

-- node_task_submissions: 节点任务提交结果表 (幂等控制)
CREATE TABLE IF NOT EXISTS node_task_submissions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES node_tasks(id) ON DELETE CASCADE,
    lease_id UUID NOT NULL,
    node_id UUID NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    session_id UUID NOT NULL REFERENCES node_sessions(id) ON DELETE CASCADE,
    result_kind TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    action TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (task_id, lease_id)
);

CREATE INDEX IF NOT EXISTS idx_node_task_submissions_task_lease ON node_task_submissions(task_id, lease_id);

-- ============================================================================
-- 管理端监控查询性能优化索引
-- ============================================================================

-- node_tasks 监控追踪查询优化索引
-- 用于 admin_monitoring.rs 中的 traces 查询（ORDER BY created_at DESC LIMIT 50）
CREATE INDEX IF NOT EXISTS idx_node_tasks_created_at_desc ON node_tasks(created_at DESC);

-- node_tasks 完成时间统计优化索引（部分索引）
-- 用于 admin_monitoring.rs 中的 avg_node_latency_ms 统计（WHERE finished_at IS NOT NULL）
CREATE INDEX IF NOT EXISTS idx_node_tasks_finished_at ON node_tasks(finished_at) WHERE finished_at IS NOT NULL;

-- node_task_submissions 监控查询优化索引
-- 用于 admin_monitoring.rs 中的 LEFT JOIN LATERAL 子查询（WHERE task_id = nt.id ORDER BY created_at DESC）
CREATE INDEX IF NOT EXISTS idx_node_task_submissions_task_id_created_at ON node_task_submissions(task_id, created_at DESC);

-- Native SSE delivery is immutable per task lease.  The state row is the
-- bounded cursor/window; event rows make retries and contiguous sequence
-- checks durable without holding a transaction across HTTP streaming.
CREATE TABLE IF NOT EXISTS node_native_streams (
    task_id UUID NOT NULL REFERENCES node_tasks(id) ON DELETE CASCADE,
    lease_id UUID NOT NULL,
    node_id UUID NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    session_id UUID NOT NULL REFERENCES node_sessions(id) ON DELETE CASCADE,
    state TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open','terminal','failed','canceled')),
    next_seq BIGINT NOT NULL DEFAULT 0 CHECK (next_seq BETWEEN 0 AND 4100),
    unread_frames INTEGER NOT NULL DEFAULT 0 CHECK (unread_frames >= 0),
    unread_bytes BIGINT NOT NULL DEFAULT 0 CHECK (unread_bytes >= 0),
    total_bytes BIGINT NOT NULL DEFAULT 0 CHECK (total_bytes >= 0),
    inspector_json JSONB NOT NULL,
    head_status INTEGER NOT NULL CHECK (head_status=200),
    head_headers JSONB NOT NULL,
    summary_json JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (task_id,lease_id)
);
CREATE TABLE IF NOT EXISTS node_native_stream_events (
    task_id UUID NOT NULL,
    lease_id UUID NOT NULL,
    seq BIGINT NOT NULL CHECK (seq >= 0),
    event_json JSONB,
    event_hash TEXT NOT NULL CHECK (event_hash ~ '^[0-9a-f]{64}$'),
    event_bytes INTEGER NOT NULL CHECK (event_bytes > 0),
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (task_id,lease_id,seq),
    FOREIGN KEY (task_id,lease_id) REFERENCES node_native_streams(task_id,lease_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_node_native_stream_events_unread
    ON node_native_stream_events(task_id,lease_id,seq) WHERE consumed_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_node_native_streams_updated
    ON node_native_streams(updated_at,task_id);

-- node_sessions 监控查询优化索引
-- 用于 admin_monitoring.rs 和 admin_node_gateway.rs 中的 LEFT JOIN LATERAL 子查询
-- （WHERE node_id = n.id ORDER BY last_seen_at DESC LIMIT 1）
CREATE INDEX IF NOT EXISTS idx_node_sessions_node_id_last_seen_at ON node_sessions(node_id, last_seen_at DESC);

-- ============================================================================
-- user_node_gateway_tokens: 用户节点网关注册令牌表
--
-- 审批流程：
--   1. 用户申请 → status='pending'
--   2. Admin 审批 → status='approved'，token 可被 GET 返回明文（始终可重建）
--   3. 用户注册节点 → status='consumed'（一次性使用）
--
-- Token 格式: kcng-{token_id}-{signature}
--   - token_id = UUID v4 去连字符（同时也是本表的 id）
--   - signature = HMAC-SHA256(secret, token_id) 后 32 个十六进制字符（取 HMAC 后 16 字节）
-- ============================================================================

CREATE TABLE IF NOT EXISTS user_node_gateway_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    user_id UUID NOT NULL,
    -- token 的 SHA-256 hash（冗余存储，用于额外校验）
    token_hash TEXT NOT NULL UNIQUE,
    -- token 预览（前 16 位，用于 UI 展示，例如 "kcng-a1b2c3d4e5f6"）
    token_preview TEXT NOT NULL,
    -- 状态：pending(待审批) / approved(已审批) / rejected(已拒绝) / consumed(已使用)
    status TEXT NOT NULL DEFAULT 'pending',
    -- token 是否已被用户查看过明文（标记已查看，用于安全提醒）
    is_revealed BOOLEAN NOT NULL DEFAULT FALSE,
    -- 审批人 ID
    approved_by UUID REFERENCES users(id),
    -- 管理员操作时间（审批通过/拒绝时均会更新）
    actioned_at TIMESTAMPTZ,
    -- 消费时间（节点注册时设置）
    consumed_at TIMESTAMPTZ,
    -- 消费该 token 注册的节点 ID
    consumed_node_id UUID REFERENCES nodes(id) ON DELETE SET NULL,
    -- 吊销原因（Admin 吊销令牌时填写）
    revoke_reason TEXT,
    -- 签发时间（用户申请时间）
    issued_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fk_node_registration_membership FOREIGN KEY (tenant_id,user_id)
        REFERENCES tenant_memberships(tenant_id,user_id) ON DELETE RESTRICT
);

-- 索引
CREATE INDEX IF NOT EXISTS idx_user_node_gateway_tokens_user_id ON user_node_gateway_tokens(user_id);
-- 待审批列表查询
CREATE INDEX IF NOT EXISTS idx_user_node_gateway_tokens_pending_issued
    ON user_node_gateway_tokens(issued_at ASC, id ASC)
    WHERE status = 'pending';
-- 已审批且未被消费的 token 查询（注册时使用）
CREATE INDEX IF NOT EXISTS idx_user_node_gateway_tokens_approved ON user_node_gateway_tokens(status) WHERE status = 'approved';
-- 管理后台节点列表按节点查找最近消费的 token
CREATE INDEX IF NOT EXISTS idx_user_node_gateway_tokens_consumed_node_issued
    ON user_node_gateway_tokens(consumed_node_id, issued_at DESC, id DESC)
    WHERE consumed_node_id IS NOT NULL;
-- token_hash 上有 UNIQUE 约束，已自动创建唯一索引，无需额外 B-tree 索引
-- 确保每用户同一时间仅有一个活跃 token（pending 或 approved），防止并发 POST 创建多个
CREATE UNIQUE INDEX IF NOT EXISTS idx_user_node_gateway_tokens_one_active ON user_node_gateway_tokens(tenant_id,user_id) WHERE status IN ('pending', 'approved');

-- 注释
COMMENT ON TABLE user_node_gateway_tokens IS '用户节点网关注册令牌表（审批制 + HMAC 签名 + 一次性使用）';
COMMENT ON COLUMN user_node_gateway_tokens.id IS '令牌记录 ID（同时也是 token 中的 token_id）';
COMMENT ON COLUMN user_node_gateway_tokens.user_id IS '所属用户 ID';
COMMENT ON COLUMN user_node_gateway_tokens.token_hash IS '令牌 SHA-256 hash';
COMMENT ON COLUMN user_node_gateway_tokens.token_preview IS '令牌预览（前 16 位）';
COMMENT ON COLUMN user_node_gateway_tokens.status IS '状态：pending/approved/rejected/consumed';
COMMENT ON COLUMN user_node_gateway_tokens.is_revealed IS 'token 明文是否已被用户查看';
COMMENT ON COLUMN user_node_gateway_tokens.approved_by IS '审批人 ID';
COMMENT ON COLUMN user_node_gateway_tokens.actioned_at IS '管理员操作时间（审批通过/拒绝时均会更新）';
COMMENT ON COLUMN user_node_gateway_tokens.consumed_at IS 'token 使用时间';
COMMENT ON COLUMN user_node_gateway_tokens.consumed_node_id IS '使用 token 注册的节点 ID';
COMMENT ON COLUMN user_node_gateway_tokens.issued_at IS '签发时间（用户申请时间）';
COMMENT ON COLUMN user_node_gateway_tokens.updated_at IS '最后更新时间';

-- ============================================================================
-- node_tips: 节点租赁小费表
--
-- 当用户通过 node gateway 发起会话并完成计费后，节点提供者（owner）获得小费
-- tips = usage_log.user_amount * node_tip_ratio
-- ============================================================================

CREATE TABLE IF NOT EXISTS node_tips (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 关联的计费记录
    usage_log_id UUID NOT NULL
        CONSTRAINT uk_node_tips_usage_log_id UNIQUE
        REFERENCES usage_logs(id) ON DELETE RESTRICT,
    -- 提供服务的节点 ID
    node_id UUID NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    -- 节点所有者（同时也是 tips 受益人）
    owner_user_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    -- 消费该服务的用户（付费方）
    consumer_user_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    -- 小费金额（10 位小数，与 usage_logs.user_amount DECIMAL(20,10) 对齐）
    tip_amount DECIMAL(20, 10) NOT NULL,
    -- 币种
    currency VARCHAR(8) NOT NULL DEFAULT 'CNY',
    -- 计算比例（快照，如 0.9000）
    tip_ratio DECIMAL(5, 4) NOT NULL,
    -- 原始计费金额（快照，审计用，10 位小数与 usage_logs.user_amount DECIMAL(20,10) 对齐）
    bill_amount DECIMAL(20, 10) NOT NULL,
    -- 创建时间
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- 最后更新时间
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_node_tips_owner_user_id ON node_tips(owner_user_id);
-- usage_log_id 上有 UNIQUE 约束，已自动创建唯一索引，无需额外 B-tree 索引
-- 按用户查询历史记录的复合索引（覆盖 list_by_user 的 ORDER BY created_at DESC）
CREATE INDEX IF NOT EXISTS idx_node_tips_owner_created ON node_tips(owner_user_id, created_at DESC);
COMMENT ON TABLE node_tips IS '节点租赁小费表';
COMMENT ON COLUMN node_tips.usage_log_id IS '关联的计费记录 ID';
COMMENT ON COLUMN node_tips.node_id IS '提供服务的节点 ID';
COMMENT ON COLUMN node_tips.owner_user_id IS '节点所有者（tips 受益人）';
COMMENT ON COLUMN node_tips.consumer_user_id IS '消费用户（付费方）';
COMMENT ON COLUMN node_tips.tip_amount IS '小费金额（元）';
COMMENT ON COLUMN node_tips.tip_ratio IS '计算比例（快照）';
COMMENT ON COLUMN node_tips.bill_amount IS '原始计费金额（快照，审计用）';

-- ============================================================================
-- node_tip_withdrawals: 小费提现记录表
--
-- 支持两种提现方式：
--   1. alipay  - 用户提供支付宝账户+姓名，管理员线下打款
--   2. balance - 直接转入用户 available_balance
--
-- PII 敏感信息加密存储：
--   - alipay_account 和 real_name 使用 AES-256-GCM 加密
--   - 加密格式：base64(nonce || ciphertext)
--   - 密钥复用 CRYPTO__SECRET_KEY 配置
-- ============================================================================

CREATE TABLE IF NOT EXISTS node_tip_withdrawals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 申请人
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    -- 提现方式：alipay / balance
    withdrawal_type VARCHAR(20) NOT NULL
        CONSTRAINT chk_node_tip_withdrawals_type CHECK (withdrawal_type IN ('alipay', 'balance')),
    -- 提现总额（10 位小数与计费精度对齐）
    total_amount DECIMAL(20, 10) NOT NULL,
    -- 币种
    currency VARCHAR(8) NOT NULL DEFAULT 'CNY',
    -- 加密的支付宝账号（仅 alipay 方式）
    -- 格式：base64(nonce || ciphertext)，使用 AES-256-GCM 加密
    -- 密钥复用 CRYPTO__SECRET_KEY 配置
    encrypted_alipay_account TEXT,
    -- 加密的真实姓名（仅 alipay 方式）
    -- 格式：base64(nonce || ciphertext)，使用 AES-256-GCM 加密
    -- 密钥复用 CRYPTO__SECRET_KEY 配置
    encrypted_real_name TEXT,
    -- 状态：pending / approved / completed / rejected
    status VARCHAR(20) NOT NULL DEFAULT 'pending'
        CONSTRAINT chk_node_tip_withdrawals_status CHECK (status IN ('pending', 'approved', 'completed', 'rejected')),
    -- 处理该提现的管理员
    admin_id UUID REFERENCES users(id),
    -- 管理员备注
    admin_remark TEXT,
    -- 管理员操作时间
    actioned_at TIMESTAMPTZ,
    -- 创建时间
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- 更新时间
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_node_tip_withdrawals_user_id ON node_tip_withdrawals(user_id);
CREATE INDEX IF NOT EXISTS idx_node_tip_withdrawals_status ON node_tip_withdrawals(status);
-- 待审批提现列表查询优化
CREATE INDEX IF NOT EXISTS idx_node_tip_withdrawals_pending ON node_tip_withdrawals(status) WHERE status = 'pending';

COMMENT ON TABLE node_tip_withdrawals IS '小费提现记录表';
COMMENT ON COLUMN node_tip_withdrawals.withdrawal_type IS '提现方式：alipay / balance';
COMMENT ON COLUMN node_tip_withdrawals.encrypted_alipay_account IS '加密的支付宝账号（仅 alipay 方式，AES-256-GCM 加密，格式：base64(nonce || ciphertext)）';
COMMENT ON COLUMN node_tip_withdrawals.encrypted_real_name IS '加密的真实姓名（仅 alipay 方式，AES-256-GCM 加密，格式：base64(nonce || ciphertext)）';
COMMENT ON COLUMN node_tip_withdrawals.status IS '状态：pending / approved / completed / rejected';
COMMENT ON COLUMN node_tip_withdrawals.admin_remark IS '管理员备注（审批/操作备注，非审计日志，生产环境建议独立审计表）';

-- Platform-owned Responses state; it never shares the upstream resource namespace.
CREATE TABLE IF NOT EXISTS scoped_conversations (
    id TEXT PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    access_mode TEXT NOT NULL CHECK (access_mode IN ('passthrough','node_dispatch')),
    account_id UUID,
    model TEXT,
    metadata_json JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(metadata_json)='object'),
    items_json JSONB NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(items_json)='array'),
    active_response_id TEXT,
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT fk_scoped_conversations_membership FOREIGN KEY (tenant_id,user_id) REFERENCES tenant_memberships(tenant_id,user_id) ON DELETE RESTRICT
);
CREATE INDEX IF NOT EXISTS idx_scoped_conversations_scope
    ON scoped_conversations(tenant_id,user_id,access_mode,created_at);
CREATE INDEX IF NOT EXISTS idx_scoped_conversations_expiry ON scoped_conversations(expires_at);
CREATE TABLE IF NOT EXISTS scoped_responses (
    id TEXT PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    access_mode TEXT NOT NULL CHECK (access_mode IN ('passthrough','node_dispatch')),
    account_id UUID,
    model TEXT NOT NULL,
    request_id UUID NOT NULL UNIQUE,
    owner_id UUID NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('queued','in_progress','completed','incomplete','failed','cancelled')),
    background BOOLEAN NOT NULL DEFAULT FALSE,
    store_response BOOLEAN NOT NULL DEFAULT TRUE,
    stream BOOLEAN NOT NULL DEFAULT FALSE,
    request_json JSONB NOT NULL CHECK (jsonb_typeof(request_json)='object'),
    input_json JSONB NOT NULL CHECK (jsonb_typeof(input_json)='array'),
    new_input_json JSONB NOT NULL CHECK (jsonb_typeof(new_input_json)='array'),
    output_json JSONB NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(output_json)='array'),
    response_json JSONB,
    execution_json JSONB,
    previous_id TEXT,
    conversation_id TEXT,
    idempotency_hash TEXT,
    request_hash TEXT NOT NULL,
    next_seq BIGINT NOT NULL DEFAULT 0 CHECK (next_seq >= 0),
    event_bytes BIGINT NOT NULL DEFAULT 0 CHECK (event_bytes >= 0),
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deadline_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT fk_scoped_responses_membership FOREIGN KEY (tenant_id,user_id) REFERENCES tenant_memberships(tenant_id,user_id) ON DELETE RESTRICT
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_scoped_responses_idempotency
    ON scoped_responses(tenant_id,user_id,access_mode,idempotency_hash)
    WHERE idempotency_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_scoped_responses_scope
    ON scoped_responses(tenant_id,user_id,access_mode,created_at);
CREATE INDEX IF NOT EXISTS idx_scoped_responses_pending
    ON scoped_responses(status,deadline_at) WHERE status IN ('queued','in_progress');
CREATE INDEX IF NOT EXISTS idx_scoped_responses_expiry ON scoped_responses(expires_at);
CREATE TABLE IF NOT EXISTS scoped_response_events (
    response_id TEXT NOT NULL REFERENCES scoped_responses(id) ON DELETE CASCADE,
    seq BIGINT NOT NULL CHECK (seq >= 0),
    frame TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(response_id,seq)
);

-- Administrative writes acquire a real versioned fence BEFORE row locks.
-- Updating (not merely locking) this row also fences REPEATABLE READ snapshots:
-- the loser must retry its whole transaction after a serialization failure.
-- Order: fence -> tenant parents (UUID order) -> users -> memberships/invitations.
CREATE TABLE IF NOT EXISTS identity_admin_fence (
    id BOOLEAN PRIMARY KEY CHECK (id),
    version BIGINT NOT NULL DEFAULT 0
);
INSERT INTO identity_admin_fence(id) VALUES (TRUE) ON CONFLICT (id) DO NOTHING;
CREATE OR REPLACE FUNCTION serialize_identity_admin() RETURNS TRIGGER AS $$
BEGIN
    UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE;
    RETURN NULL;
END; $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS identity_users_insert_fence ON users;
CREATE TRIGGER identity_users_insert_fence BEFORE INSERT OR DELETE ON users
    FOR EACH STATEMENT EXECUTE FUNCTION serialize_identity_admin();
DROP TRIGGER IF EXISTS identity_users_update_fence ON users;
CREATE TRIGGER identity_users_update_fence BEFORE UPDATE OF platform_role,status ON users
    FOR EACH STATEMENT EXECUTE FUNCTION serialize_identity_admin();
DROP TRIGGER IF EXISTS identity_tenants_fence ON tenants;
CREATE TRIGGER identity_tenants_fence BEFORE INSERT OR DELETE ON tenants
    FOR EACH STATEMENT EXECUTE FUNCTION serialize_identity_admin();
DROP TRIGGER IF EXISTS identity_tenants_update_fence ON tenants;
CREATE TRIGGER identity_tenants_update_fence
    BEFORE UPDATE OF owner_user_id, name, slug, description, status,
        default_rpm_limit, default_tpm_limit ON tenants
    FOR EACH STATEMENT EXECUTE FUNCTION serialize_identity_admin();
DROP TRIGGER IF EXISTS identity_memberships_fence ON tenant_memberships;
CREATE TRIGGER identity_memberships_fence BEFORE INSERT OR DELETE ON tenant_memberships
    FOR EACH STATEMENT EXECUTE FUNCTION serialize_identity_admin();
DROP TRIGGER IF EXISTS identity_memberships_update_fence ON tenant_memberships;
CREATE TRIGGER identity_memberships_update_fence
    BEFORE UPDATE OF tenant_id, user_id, role, status ON tenant_memberships
    FOR EACH STATEMENT EXECUTE FUNCTION serialize_identity_admin();

CREATE OR REPLACE FUNCTION version_user_authority() RETURNS TRIGGER AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id THEN
        RAISE EXCEPTION 'user identity is immutable' USING ERRCODE='23514';
    END IF;
    IF ROW(NEW.platform_role,NEW.status) IS DISTINCT FROM ROW(OLD.platform_role,OLD.status) THEN
        NEW.token_version := OLD.token_version+1;
    ELSIF NEW.token_version < OLD.token_version THEN
        RAISE EXCEPTION 'token version cannot decrease' USING ERRCODE='23514';
    END IF;
    NEW.updated_at := NOW();
    RETURN NEW;
END; $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS user_authority_version ON users;
CREATE TRIGGER user_authority_version BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION version_user_authority();

CREATE OR REPLACE FUNCTION version_tenant_authority() RETURNS TRIGGER AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id THEN
        RAISE EXCEPTION 'tenant identity is immutable' USING ERRCODE='23514';
    END IF;
    IF ROW(NEW.owner_user_id,NEW.name,NEW.slug,NEW.description,NEW.status,NEW.default_rpm_limit,NEW.default_tpm_limit)
       IS DISTINCT FROM ROW(OLD.owner_user_id,OLD.name,OLD.slug,OLD.description,OLD.status,OLD.default_rpm_limit,OLD.default_tpm_limit) THEN
        NEW.authz_version := OLD.authz_version+1;
    ELSIF NEW.authz_version < OLD.authz_version THEN
        RAISE EXCEPTION 'tenant version cannot decrease' USING ERRCODE='23514';
    END IF;
    NEW.updated_at := NOW();
    RETURN NEW;
END; $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS tenant_authority_version ON tenants;
CREATE TRIGGER tenant_authority_version BEFORE UPDATE ON tenants
    FOR EACH ROW EXECUTE FUNCTION version_tenant_authority();

CREATE OR REPLACE FUNCTION version_membership_authority() RETURNS TRIGGER AS $$
DECLARE target_tenant UUID;
BEGIN
    target_tenant := CASE WHEN TG_OP='DELETE' THEN OLD.tenant_id ELSE NEW.tenant_id END;
    IF TG_OP='DELETE' THEN
        IF EXISTS (SELECT 1 FROM tenants WHERE id=target_tenant) THEN
            RAISE EXCEPTION 'memberships must be revoked, not deleted' USING ERRCODE='23514';
        END IF;
        RETURN OLD;
    END IF;
    IF TG_OP='UPDATE' THEN
        IF ROW(NEW.tenant_id,NEW.user_id) IS DISTINCT FROM ROW(OLD.tenant_id,OLD.user_id) THEN
            RAISE EXCEPTION 'membership identity is immutable' USING ERRCODE='23514';
        END IF;
        IF ROW(NEW.role,NEW.status) IS DISTINCT FROM ROW(OLD.role,OLD.status) THEN
            NEW.version := OLD.version+1;
        ELSE
            NEW.version := OLD.version;
        END IF;
    END IF;
    NEW.updated_at := NOW();
    RETURN NEW;
END; $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS membership_authority_version ON tenant_memberships;
CREATE TRIGGER membership_authority_version BEFORE INSERT OR UPDATE OR DELETE ON tenant_memberships
    FOR EACH ROW EXECUTE FUNCTION version_membership_authority();

-- A restored membership never restores credentials issued before suspension.
CREATE OR REPLACE FUNCTION revoke_membership_credentials() RETURNS TRIGGER AS $$
BEGIN
    IF OLD.status IS DISTINCT FROM NEW.status AND NEW.status <> 'active' THEN
        UPDATE produce_ai_keys
           SET revoked=TRUE, revoked_at=COALESCE(revoked_at,NOW()), updated_at=NOW()
         WHERE tenant_id=NEW.tenant_id AND user_id=NEW.user_id AND NOT revoked;
        UPDATE user_node_gateway_tokens
           SET status='rejected', actioned_at=NOW(), revoke_reason='membership deactivated', updated_at=NOW()
         WHERE tenant_id=NEW.tenant_id AND user_id=NEW.user_id AND status IN ('pending','approved');
        UPDATE node_sessions SET accepting_tasks=FALSE
         WHERE node_id IN (SELECT id FROM nodes WHERE tenant_id=NEW.tenant_id AND owner_user_id=NEW.user_id);
    END IF;
    IF NEW.status <> 'active' OR (OLD.role='admin' AND NEW.role<>'admin') THEN
        UPDATE tenant_invitations
           SET status='revoked', revoked_at=NOW(), updated_at=NOW()
         WHERE tenant_id=NEW.tenant_id AND status='pending'
           AND (invited_by_user_id=NEW.user_id OR (
               NEW.status <> 'active' AND email=(
                   SELECT lower(btrim(email)) FROM users WHERE id=NEW.user_id
               )
           ));
        -- Deactivation also invalidates invitations addressed to this member.
        -- A pre-existing token must not undo an explicit suspend/revoke.
    END IF;
    RETURN NEW;
END; $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS membership_credentials_revoked ON tenant_memberships;
CREATE TRIGGER membership_credentials_revoked AFTER UPDATE OF role,status ON tenant_memberships
    FOR EACH ROW EXECUTE FUNCTION revoke_membership_credentials();

-- Validate final transaction state, permitting atomic create and transfer.
CREATE OR REPLACE FUNCTION assert_identity_invariants() RETURNS TRIGGER AS $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM tenants t WHERE NOT EXISTS (
            SELECT 1 FROM tenant_memberships m JOIN users u ON u.id=m.user_id
            WHERE m.tenant_id=t.id AND m.user_id=t.owner_user_id
              AND m.role='admin' AND m.status='active' AND u.status='active'
        )
    ) THEN
        RAISE EXCEPTION 'tenant owner must be an active user and active admin; last admin cannot be removed' USING ERRCODE='23514';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM users WHERE platform_role='root' AND status='active') THEN
        RAISE EXCEPTION 'at least one active root is required' USING ERRCODE='23514';
    END IF;
    RETURN NULL;
END; $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS identity_users_guard ON users;
CREATE CONSTRAINT TRIGGER identity_users_guard AFTER INSERT OR UPDATE OF platform_role,status OR DELETE ON users
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION assert_identity_invariants();
DROP TRIGGER IF EXISTS identity_tenants_guard ON tenants;
CREATE CONSTRAINT TRIGGER identity_tenants_guard AFTER INSERT OR UPDATE OF owner_user_id,status OR DELETE ON tenants
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION assert_identity_invariants();
DROP TRIGGER IF EXISTS identity_memberships_guard ON tenant_memberships;
CREATE CONSTRAINT TRIGGER identity_memberships_guard AFTER INSERT OR UPDATE OF role,status OR DELETE ON tenant_memberships
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION assert_identity_invariants();

CREATE OR REPLACE FUNCTION reject_tenant_audit_mutation() RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'tenant audit events are immutable' USING ERRCODE='42501';
END; $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS tenant_audit_immutable ON tenant_audit_events;
CREATE TRIGGER tenant_audit_immutable BEFORE UPDATE OR DELETE ON tenant_audit_events
    FOR EACH ROW EXECUTE FUNCTION reject_tenant_audit_mutation();
DROP TRIGGER IF EXISTS tenant_audit_no_truncate ON tenant_audit_events;
CREATE TRIGGER tenant_audit_no_truncate BEFORE TRUNCATE ON tenant_audit_events
    FOR EACH STATEMENT EXECUTE FUNCTION reject_tenant_audit_mutation();

CREATE OR REPLACE FUNCTION assert_node_task_scope()
RETURNS TRIGGER AS $$
BEGIN
    IF NEW.assigned_node_id IS NOT NULL
       AND NOT EXISTS (
           SELECT 1 FROM nodes n
           WHERE n.id = NEW.assigned_node_id AND n.tenant_id = NEW.tenant_id
       ) THEN
        RAISE EXCEPTION 'node task % crosses tenant boundary', NEW.id
            USING ERRCODE = '23514';
    END IF;
    IF NEW.assigned_session_id IS NOT NULL
       AND NOT EXISTS (
           SELECT 1
           FROM node_sessions ns
           JOIN nodes n ON n.id = ns.node_id
           WHERE ns.id = NEW.assigned_session_id AND n.tenant_id = NEW.tenant_id
       ) THEN
        RAISE EXCEPTION 'node task % session crosses tenant boundary', NEW.id
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS node_task_scope_guard ON node_tasks;
CREATE TRIGGER node_task_scope_guard
    BEFORE INSERT OR UPDATE OF tenant_id, assigned_node_id, assigned_session_id
    ON node_tasks FOR EACH ROW EXECUTE FUNCTION assert_node_task_scope();

-- Membership changes never move accepted financial obligations or resources.
CREATE OR REPLACE FUNCTION guard_resource_identity() RETURNS TRIGGER AS $$
BEGIN
    IF ROW(NEW.tenant_id,NEW.user_id) IS DISTINCT FROM ROW(OLD.tenant_id,OLD.user_id) THEN
        RAISE EXCEPTION 'resource tenant and owner are immutable' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END; $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS resource_identity_immutable ON payment_orders;
CREATE TRIGGER resource_identity_immutable BEFORE UPDATE OF tenant_id,user_id ON payment_orders
    FOR EACH ROW EXECUTE FUNCTION guard_resource_identity();
DROP TRIGGER IF EXISTS resource_identity_immutable ON user_balances;
CREATE TRIGGER resource_identity_immutable BEFORE UPDATE OF tenant_id,user_id ON user_balances
    FOR EACH ROW EXECUTE FUNCTION guard_resource_identity();
DROP TRIGGER IF EXISTS resource_identity_immutable ON usage_logs;
CREATE TRIGGER resource_identity_immutable BEFORE UPDATE OF tenant_id,user_id ON usage_logs
    FOR EACH ROW EXECUTE FUNCTION guard_resource_identity();
DROP TRIGGER IF EXISTS resource_identity_immutable ON balance_transactions;
CREATE TRIGGER resource_identity_immutable BEFORE UPDATE OF tenant_id,user_id ON balance_transactions
    FOR EACH ROW EXECUTE FUNCTION guard_resource_identity();
DROP TRIGGER IF EXISTS resource_identity_immutable ON balance_reservations;
CREATE TRIGGER resource_identity_immutable BEFORE UPDATE OF tenant_id,user_id ON balance_reservations
    FOR EACH ROW EXECUTE FUNCTION guard_resource_identity();
DROP TRIGGER IF EXISTS resource_identity_immutable ON node_tasks;
CREATE TRIGGER resource_identity_immutable BEFORE UPDATE OF tenant_id,user_id ON node_tasks
    FOR EACH ROW EXECUTE FUNCTION guard_resource_identity();
DROP TRIGGER IF EXISTS resource_identity_immutable ON produce_ai_keys;
CREATE TRIGGER resource_identity_immutable BEFORE UPDATE OF tenant_id,user_id ON produce_ai_keys
    FOR EACH ROW EXECUTE FUNCTION guard_resource_identity();
DROP TRIGGER IF EXISTS resource_identity_immutable ON scoped_responses;
CREATE TRIGGER resource_identity_immutable BEFORE UPDATE OF tenant_id,user_id ON scoped_responses
    FOR EACH ROW EXECUTE FUNCTION guard_resource_identity();
DROP TRIGGER IF EXISTS resource_identity_immutable ON scoped_conversations;
CREATE TRIGGER resource_identity_immutable BEFORE UPDATE OF tenant_id,user_id ON scoped_conversations
    FOR EACH ROW EXECUTE FUNCTION guard_resource_identity();

CREATE OR REPLACE FUNCTION revoke_suspended_user_credentials() RETURNS TRIGGER AS $$
BEGIN
    IF OLD.status IS DISTINCT FROM NEW.status AND NEW.status='suspended' THEN
        UPDATE produce_ai_keys SET revoked=TRUE,revoked_at=COALESCE(revoked_at,NOW()),updated_at=NOW()
          WHERE user_id=NEW.id AND NOT revoked;
        UPDATE user_node_gateway_tokens SET status='rejected',actioned_at=NOW(),revoke_reason='user suspended',updated_at=NOW()
          WHERE user_id=NEW.id AND status IN ('pending','approved');
        UPDATE node_sessions SET accepting_tasks=FALSE WHERE node_id IN (SELECT id FROM nodes WHERE owner_user_id=NEW.id);
        UPDATE tenant_invitations SET status='revoked',revoked_at=NOW(),updated_at=NOW()
          WHERE status='pending' AND (
              invited_by_user_id=NEW.id OR email=lower(btrim(NEW.email))
          );
    END IF;
    RETURN NEW;
END; $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS user_credentials_revoked ON users;
CREATE TRIGGER user_credentials_revoked AFTER UPDATE OF status ON users
    FOR EACH ROW EXECUTE FUNCTION revoke_suspended_user_credentials();
DROP TRIGGER IF EXISTS resource_identity_immutable ON user_node_gateway_tokens;
CREATE TRIGGER resource_identity_immutable BEFORE UPDATE OF tenant_id,user_id ON user_node_gateway_tokens
    FOR EACH ROW EXECUTE FUNCTION guard_resource_identity();
CREATE OR REPLACE FUNCTION guard_node_identity() RETURNS TRIGGER AS $$
BEGIN
    IF ROW(NEW.tenant_id,NEW.owner_user_id) IS DISTINCT FROM ROW(OLD.tenant_id,OLD.owner_user_id) THEN
        RAISE EXCEPTION 'node tenant and owner are immutable' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END; $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS node_identity_immutable ON nodes;
CREATE TRIGGER node_identity_immutable BEFORE UPDATE OF tenant_id,owner_user_id ON nodes
    FOR EACH ROW EXECUTE FUNCTION guard_node_identity();
