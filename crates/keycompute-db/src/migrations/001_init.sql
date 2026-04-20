-- tenants: 租户/组织表
CREATE TABLE tenants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(255) NOT NULL,
    slug VARCHAR(100) NOT NULL UNIQUE,
    description TEXT,
    status VARCHAR(50) NOT NULL DEFAULT 'active',
    -- 租户配置
    default_rpm_limit INTEGER NOT NULL DEFAULT 60,
    default_tpm_limit INTEGER NOT NULL DEFAULT 100000,
    distribution_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_tenants_slug ON tenants(slug);
CREATE INDEX idx_tenants_status ON tenants(status);

-- users: 用户表
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    email VARCHAR(255) NOT NULL UNIQUE,
    name VARCHAR(255),
    role VARCHAR(50) NOT NULL DEFAULT 'user'
        CONSTRAINT chk_users_role_allowed CHECK (role IN ('system', 'admin', 'user')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_users_tenant_id ON users(tenant_id);
CREATE INDEX idx_users_email ON users(email);
CREATE UNIQUE INDEX uq_users_single_system_role ON users (role) WHERE role = 'system';

CREATE OR REPLACE FUNCTION prevent_system_role_change()
RETURNS TRIGGER AS $$
BEGIN
    IF OLD.role = 'system' AND NEW.role <> 'system' THEN
        RAISE EXCEPTION 'system user role cannot be changed';
    END IF;

    IF OLD.role <> 'system' AND NEW.role = 'system' THEN
        RAISE EXCEPTION 'system role cannot be assigned by update';
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_prevent_system_role_change
BEFORE UPDATE OF role ON users
FOR EACH ROW
EXECUTE FUNCTION prevent_system_role_change();

CREATE OR REPLACE FUNCTION prevent_system_user_delete()
RETURNS TRIGGER AS $$
BEGIN
    IF OLD.role = 'system' THEN
        RAISE EXCEPTION 'system user cannot be deleted';
    END IF;

    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_prevent_system_user_delete
BEFORE DELETE ON users
FOR EACH ROW
EXECUTE FUNCTION prevent_system_user_delete();

-- produce_ai_keys: Produce AI Key 表（用户访问系统的 API Key）
CREATE TABLE produce_ai_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    user_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    produce_ai_key_hash VARCHAR(255) NOT NULL UNIQUE,
    produce_ai_key_preview VARCHAR(20) NOT NULL,
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    revoked_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    last_used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_produce_ai_keys_tenant ON produce_ai_keys(tenant_id);
CREATE INDEX idx_produce_ai_keys_user ON produce_ai_keys(user_id);
CREATE INDEX idx_produce_ai_keys_hash ON produce_ai_keys(produce_ai_key_hash);
CREATE INDEX idx_produce_ai_keys_revoked ON produce_ai_keys(revoked) WHERE revoked = FALSE;
-- accounts: 上游 Provider 账号池
CREATE TABLE accounts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    provider VARCHAR(50) NOT NULL,
    name VARCHAR(255) NOT NULL,
    endpoint VARCHAR(500) NOT NULL,
    upstream_api_key_encrypted TEXT NOT NULL,
    upstream_api_key_preview VARCHAR(20) NOT NULL,
    rpm_limit INTEGER NOT NULL DEFAULT 60,
    tpm_limit INTEGER NOT NULL DEFAULT 100000,
    priority INTEGER NOT NULL DEFAULT 0,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    models_supported TEXT[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_accounts_tenant_id ON accounts(tenant_id);
CREATE INDEX idx_accounts_provider ON accounts(provider);
CREATE INDEX idx_accounts_enabled ON accounts(enabled) WHERE enabled = TRUE;
-- pricing_models: 模型定价表
CREATE TABLE pricing_models (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID,
    model_name VARCHAR(100) NOT NULL,
    provider VARCHAR(50) NOT NULL,
    currency VARCHAR(10) NOT NULL DEFAULT 'CNY',
    input_price_per_1k DECIMAL(20, 10) NOT NULL,
    output_price_per_1k DECIMAL(20, 10) NOT NULL,
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    effective_from TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    effective_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(tenant_id, model_name, provider, effective_from)
);

CREATE INDEX idx_pricing_models_tenant_id ON pricing_models(tenant_id);
CREATE INDEX idx_pricing_models_model ON pricing_models(model_name);
CREATE INDEX idx_pricing_models_provider ON pricing_models(provider);
CREATE INDEX idx_pricing_models_default ON pricing_models(is_default) WHERE is_default = TRUE;
-- usage_logs: 计费主账本，不可变
CREATE TABLE usage_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    request_id UUID NOT NULL UNIQUE,
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
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_usage_logs_tenant ON usage_logs(tenant_id);
CREATE INDEX idx_usage_logs_user ON usage_logs(user_id);
CREATE INDEX idx_usage_logs_produce_ai_key ON usage_logs(produce_ai_key_id);
CREATE INDEX idx_usage_logs_created ON usage_logs(created_at);
CREATE INDEX idx_usage_logs_request ON usage_logs(request_id);
-- distribution_records: 二级分销记录
CREATE TABLE distribution_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    usage_log_id UUID NOT NULL REFERENCES usage_logs(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL,
    beneficiary_id UUID NOT NULL,
    share_amount DECIMAL(20, 10) NOT NULL,
    share_ratio DECIMAL(5, 4) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    settled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_distribution_records_tenant_id ON distribution_records(tenant_id);
CREATE INDEX idx_distribution_records_usage_log_id ON distribution_records(usage_log_id);
CREATE INDEX idx_distribution_records_beneficiary_id ON distribution_records(beneficiary_id);
CREATE INDEX idx_distribution_records_status ON distribution_records(status);
-- tenant_distribution_rules: 租户分销规则
CREATE TABLE tenant_distribution_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    beneficiary_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL DEFAULT '默认分销规则',
    description TEXT,
    commission_rate DECIMAL(5, 4) NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    effective_from TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    effective_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(tenant_id, beneficiary_id, effective_from)
);

CREATE INDEX idx_tenant_distribution_rules_tenant ON tenant_distribution_rules(tenant_id);
CREATE INDEX idx_tenant_distribution_rules_active ON tenant_distribution_rules(is_active) WHERE is_active = TRUE;
-- pending_registrations: 待完成注册表
-- 用于邮箱验证码注册流程，在验证码验证成功前暂存注册占位状态

CREATE TABLE pending_registrations (
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

CREATE INDEX idx_pending_registrations_email ON pending_registrations(email);
CREATE INDEX idx_pending_registrations_expires ON pending_registrations(expires_at);
CREATE INDEX idx_pending_registrations_referral_code ON pending_registrations(referral_code);
-- user_credentials: 用户密码凭证表
-- 存储用户密码哈希和登录安全相关信息

CREATE TABLE user_credentials (
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
CREATE INDEX idx_user_credentials_user ON user_credentials(user_id);
CREATE INDEX idx_user_credentials_locked ON user_credentials(locked_until) 
    WHERE locked_until IS NOT NULL;
CREATE INDEX idx_user_credentials_verified ON user_credentials(email_verified) 
    WHERE email_verified = FALSE;
-- password_resets: 密码重置令牌表
-- 管理用户密码重置流程

CREATE TABLE password_resets (
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
CREATE INDEX idx_password_resets_token ON password_resets(token);
CREATE INDEX idx_password_resets_expires ON password_resets(expires_at) 
    WHERE used = FALSE;
CREATE INDEX idx_password_resets_user ON password_resets(user_id);
-- user_referrals: 用户推荐关系表
-- 用于存储谁推荐了谁，支持二级分销

CREATE TABLE user_referrals (
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
CREATE INDEX idx_user_referrals_user ON user_referrals(user_id);
CREATE INDEX idx_user_referrals_level1 ON user_referrals(level1_referrer_id);
CREATE INDEX idx_user_referrals_level2 ON user_referrals(level2_referrer_id);
CREATE INDEX idx_user_referrals_status ON user_referrals(status);
-- 为 distribution_records 添加 level 字段
-- 用于明确标识分销层级（level1 或 level2）

ALTER TABLE distribution_records
ADD COLUMN level VARCHAR(20) NOT NULL DEFAULT 'level1';

-- 创建索引以支持按层级查询
CREATE INDEX idx_distribution_records_level ON distribution_records(level);

-- 更新已有数据（根据 share_ratio 推断层级）
-- share_ratio > 2% 的为 level1，否则为 level2
UPDATE distribution_records
SET level = CASE
    WHEN share_ratio > 0.02 THEN 'level1'
    ELSE 'level2'
END
WHERE level = 'level1';

-- 清理可能的重复数据（保留最新的一条）
-- 使用 DELETE 配合子查询删除重复记录
DELETE FROM distribution_records
WHERE id IN (
    SELECT id FROM (
        SELECT id,
               ROW_NUMBER() OVER (
                   PARTITION BY usage_log_id, beneficiary_id, level
                   ORDER BY created_at DESC, id DESC
               ) as rn
        FROM distribution_records
    ) t
    WHERE rn > 1
);

-- 添加唯一约束防止重复分销记录
-- 同一 usage_log 的同一受益人在同一层级只能有一条记录
ALTER TABLE distribution_records
ADD CONSTRAINT uk_distribution_records_unique
UNIQUE (usage_log_id, beneficiary_id, level);

-- 添加注释说明幂等性保护
COMMENT ON CONSTRAINT uk_distribution_records_unique ON distribution_records IS 
'幂等性保护：防止同一 usage_log 对同一受益人的重复分销记录';
-- 支付订单表
-- 用于存储用户充值订单记录

CREATE TABLE IF NOT EXISTS payment_orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 租户ID
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    -- 用户ID
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- 支付宝订单号（外部订单号）
    out_trade_no VARCHAR(64) NOT NULL UNIQUE,
    -- 支付宝交易号（支付宝返回）
    trade_no VARCHAR(64),
    -- 订单金额（单位：元）
    amount DECIMAL(12, 2) NOT NULL,
    -- 币种（默认CNY）
    currency VARCHAR(8) NOT NULL DEFAULT 'CNY',
    -- 订单状态: pending/paid/failed/closed
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    -- 支付方式: alipay
    payment_method VARCHAR(20) NOT NULL DEFAULT 'alipay',
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
    -- 备注信息
    remarks TEXT,
    -- 创建时间
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- 更新时间
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 创建索引
CREATE INDEX idx_payment_orders_tenant_id ON payment_orders(tenant_id);
CREATE INDEX idx_payment_orders_user_id ON payment_orders(user_id);
CREATE INDEX idx_payment_orders_out_trade_no ON payment_orders(out_trade_no);
CREATE INDEX idx_payment_orders_trade_no ON payment_orders(trade_no);
CREATE INDEX idx_payment_orders_status ON payment_orders(status);
CREATE INDEX idx_payment_orders_created_at ON payment_orders(created_at);

-- 添加注释
COMMENT ON TABLE payment_orders IS '支付订单表';
COMMENT ON COLUMN payment_orders.id IS '订单ID';
COMMENT ON COLUMN payment_orders.tenant_id IS '租户ID';
COMMENT ON COLUMN payment_orders.user_id IS '用户ID';
COMMENT ON COLUMN payment_orders.out_trade_no IS '商户订单号（外部订单号）';
COMMENT ON COLUMN payment_orders.trade_no IS '支付宝交易号';
COMMENT ON COLUMN payment_orders.amount IS '订单金额（单位：元）';
COMMENT ON COLUMN payment_orders.currency IS '币种';
COMMENT ON COLUMN payment_orders.status IS '订单状态: pending/paid/failed/closed';
COMMENT ON COLUMN payment_orders.payment_method IS '支付方式';
COMMENT ON COLUMN payment_orders.subject IS '商品标题';
COMMENT ON COLUMN payment_orders.body IS '商品描述';
COMMENT ON COLUMN payment_orders.paid_at IS '支付时间';
COMMENT ON COLUMN payment_orders.closed_at IS '关闭时间';
COMMENT ON COLUMN payment_orders.expired_at IS '过期时间';
COMMENT ON COLUMN payment_orders.pay_url IS '支付URL';
COMMENT ON COLUMN payment_orders.notify_data IS '回调通知原始数据';
COMMENT ON COLUMN payment_orders.remarks IS '备注信息';

-- 用户余额表
CREATE TABLE IF NOT EXISTS user_balances (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 租户ID
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    -- 用户ID
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE UNIQUE,
    -- 可用余额（单位：元）
    available_balance DECIMAL(12, 2) NOT NULL DEFAULT 0,
    -- 冻结余额（单位：元）
    frozen_balance DECIMAL(12, 2) NOT NULL DEFAULT 0,
    -- 累计充值金额
    total_recharged DECIMAL(12, 2) NOT NULL DEFAULT 0,
    -- 累计消费金额
    total_consumed DECIMAL(12, 2) NOT NULL DEFAULT 0,
    -- 创建时间
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- 更新时间
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 创建索引
CREATE INDEX idx_user_balances_tenant_id ON user_balances(tenant_id);
CREATE INDEX idx_user_balances_user_id ON user_balances(user_id);

-- 添加注释
COMMENT ON TABLE user_balances IS '用户余额表';
COMMENT ON COLUMN user_balances.available_balance IS '可用余额（单位：元）';
COMMENT ON COLUMN user_balances.frozen_balance IS '冻结余额（单位：元）';
COMMENT ON COLUMN user_balances.total_recharged IS '累计充值金额';
COMMENT ON COLUMN user_balances.total_consumed IS '累计消费金额';

-- 余额变动记录表
CREATE TABLE IF NOT EXISTS balance_transactions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 租户ID
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    -- 用户ID
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- 关联订单ID（可选）
    order_id UUID REFERENCES payment_orders(id),
    -- 关联使用日志ID（可选）
    usage_log_id UUID REFERENCES usage_logs(id),
    -- 交易类型: recharge/consume/freeze/unfreeze
    transaction_type VARCHAR(20) NOT NULL,
    -- 变动金额（正数为增加，负数为减少）
    amount DECIMAL(12, 2) NOT NULL,
    -- 变动前余额
    balance_before DECIMAL(12, 2) NOT NULL,
    -- 变动后余额
    balance_after DECIMAL(12, 2) NOT NULL,
    -- 币种
    currency VARCHAR(8) NOT NULL DEFAULT 'CNY',
    -- 备注
    description TEXT,
    -- 创建时间
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 创建索引
CREATE INDEX idx_balance_transactions_tenant_id ON balance_transactions(tenant_id);
CREATE INDEX idx_balance_transactions_user_id ON balance_transactions(user_id);
CREATE INDEX idx_balance_transactions_order_id ON balance_transactions(order_id);
CREATE INDEX idx_balance_transactions_usage_log_id ON balance_transactions(usage_log_id);
CREATE INDEX idx_balance_transactions_type ON balance_transactions(transaction_type);
CREATE INDEX idx_balance_transactions_created_at ON balance_transactions(created_at);

-- 添加注释
COMMENT ON TABLE balance_transactions IS '余额变动记录表';
COMMENT ON COLUMN balance_transactions.transaction_type IS '交易类型: recharge/consume/freeze/unfreeze';
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
    value TEXT NOT NULL
        CONSTRAINT chk_system_settings_default_user_role
        CHECK (key <> 'default_user_role' OR value = 'user'),
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
CREATE INDEX IF NOT EXISTS idx_system_settings_key ON system_settings(key);

-- 插入默认系统设置
INSERT INTO system_settings (key, value, value_type, description) VALUES
    -- 站点设置
    ('site_name', 'KeyCompute', 'string', '站点名称'),
    ('site_description', 'AI Gateway Platform', 'string', '站点描述'),
    ('site_logo_url', '', 'string', '站点 Logo URL'),
    ('site_favicon_url', '', 'string', '站点 Favicon URL'),
    
    -- 注册设置
    ('default_user_quota', '10.00', 'decimal', '新用户默认配额（元）'),
    ('default_user_role', 'user', 'string', '新用户默认角色'),
    
    -- 限流设置
    ('default_rpm_limit', '60', 'int', '默认 RPM 限制'),
    ('default_tpm_limit', '100000', 'int', '默认 TPM 限制'),
    
    -- 系统状态
    ('maintenance_mode', 'false', 'bool', '维护模式（开启后禁止所有 API 访问）'),
    ('maintenance_message', '', 'string', '维护模式提示信息'),
    
    -- 分销设置
    ('distribution_enabled', 'false', 'bool', '是否启用分销系统'),
    ('distribution_level1_default_ratio', '0.03', 'decimal', '一级分销默认分成比例'),
    ('distribution_level2_default_ratio', '0.01', 'decimal', '二级分销默认分成比例'),
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

CREATE TRIGGER trigger_update_system_settings_updated_at
    BEFORE UPDATE ON system_settings
    FOR EACH ROW
    EXECUTE FUNCTION update_system_settings_updated_at();
