use std::collections::HashMap;
use std::sync::LazyLock;

pub static ZH: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::new();

    // ── 导航 ────────────────────────────────────
    m.insert("nav.home", "首页");
    m.insert("nav.usage", "用量统计");
    m.insert("nav.billing", "账单管理");
    m.insert("nav.api_keys", "API Keys");
    m.insert("nav.payments", "支付中心");
    m.insert("nav.payments.balance", "余额查询");
    m.insert("nav.payments.orders", "订单列表");
    m.insert("nav.payments.recharge", "充值");
    m.insert("nav.distribution", "分销中心");
    m.insert("nav.distribution.earnings", "分销收益");
    m.insert("nav.distribution.referrals", "推荐列表");
    m.insert("nav.distribution.invite", "邀请管理");
    m.insert("nav.user", "个人中心");
    m.insert("nav.user.profile", "个人资料");
    m.insert("nav.user.security", "安全设置");
    m.insert("nav.users", "用户管理");
    m.insert("nav.accounts", "账号管理");
    m.insert("nav.pricing", "定价管理");
    m.insert("nav.payment_orders", "支付订单");
    m.insert("nav.distribution_records", "分销记录");
    m.insert("nav.tenants", "租户管理");
    m.insert("nav.system", "系统诊断");
    m.insert("nav.account_settings", "账户设置");
    m.insert("nav.settings", "系统设置");
    m.insert("nav.group.usage", "用量");
    m.insert("nav.group.billing", "账务");
    m.insert("nav.group.account", "账户");
    m.insert("nav.group.admin", "管理");

    // ── 认证 ────────────────────────────────────
    m.insert("auth.login", "登录");
    m.insert("auth.register", "注册");
    m.insert("auth.logout", "退出登录");
    m.insert("auth.forgot_password", "忘记密码");
    m.insert("auth.reset_password", "重置密码");
    m.insert("auth.email", "邮箱");
    m.insert("auth.password", "密码");
    m.insert("auth.confirm_password", "确认密码");
    m.insert("auth.name", "姓名");
    m.insert("auth.remember_me", "记住我");
    m.insert("auth.no_account", "还没有账号？");
    m.insert("auth.has_account", "已有账号？");
    m.insert("auth.send_reset_email", "发送重置邮件");
    m.insert("auth.back_to_login", "返回登录");
    m.insert("auth.login_subtitle", "登录您的账户以继续");
    m.insert("auth.register_subtitle", "创建您的账户");
    m.insert("auth.reset_subtitle", "输入您的邮箱，我们将发送重置链接");
    m.insert("auth.reset_sent", "重置链接已发送到您的邮箱，请查收");
    m.insert("auth.register_now", "立即注册");
    m.insert("auth.login_now", "立即登录");
    m.insert("auth.email_placeholder", "请输入邮箱");
    m.insert("auth.password_placeholder", "请输入密码");
    m.insert("auth.name_placeholder", "请输入姓名");
    m.insert("auth.confirm_password_placeholder", "再次输入密码");
    m.insert("auth.reset_email_placeholder", "请输入注册邮箱");
    m.insert("auth.fill_all", "请填写邮箱和密码");
    m.insert("auth.fill_required", "请填写所有必填项");
    m.insert("auth.enter_email", "请输入邮箱地址");
    m.insert("auth.login_failed", "登录失败");
    m.insert("auth.register_failed", "注册失败");
    m.insert("auth.send_failed", "发送失败");
    m.insert("auth.sending", "发送中...");
    m.insert("auth.send_reset_link", "发送重置链接");
    m.insert("auth.logging_in", "登录中...");
    m.insert("auth.registering", "注册中...");
    m.insert("auth.request_code", "获取验证码");
    m.insert("auth.requesting_code", "验证码发送中...");
    m.insert("auth.request_code_failed", "获取验证码失败");
    m.insert("auth.resend_code", "重新发送验证码");
    m.insert("auth.complete_registration", "完成注册");
    m.insert("auth.verification_code", "邮箱验证码");
    m.insert("auth.verification_code_placeholder", "请输入 6 位验证码");
    m.insert("auth.code_required", "请输入邮箱验证码");
    m.insert("auth.code_sent_to", "验证码已发送至：");
    m.insert(
        "auth.code_sent_hint",
        "验证码 10 分钟内有效，验证成功后才会正式创建账号。",
    );
    m.insert("auth.change_email", "更换邮箱");
    m.insert(
        "auth.registration_success",
        "注册已完成，请使用刚刚设置的邮箱和密码登录。",
    );
    m.insert("auth.password_min8", "密码至少8位");

    // ── 页面标题 ─────────────────────────────────
    m.insert("page.home", "仪表盘");
    m.insert("page.usage", "用量统计");
    m.insert("page.billing", "账单管理");
    m.insert("page.api_keys", "API Key 管理");
    m.insert("page.payments", "支付中心");
    m.insert("page.distribution", "分销中心");
    m.insert("page.profile", "个人资料");
    m.insert("page.security", "安全设置");
    m.insert("page.users", "用户管理");
    m.insert("page.accounts", "账号管理");
    m.insert("page.pricing", "定价管理");
    m.insert("page.payment_orders", "支付订单");
    m.insert("page.distribution_records", "分销记录");
    m.insert("page.tenants", "租户管理");
    m.insert("page.system", "系统诊断");
    m.insert("page.account_settings", "账户设置");
    m.insert("page.settings", "系统设置");
    m.insert("page.not_found", "页面不存在");

    // ── 表单 ────────────────────────────────────
    m.insert("form.save", "保存");
    m.insert("form.cancel", "取消");
    m.insert("form.confirm", "确认");
    m.insert("form.delete", "删除");
    m.insert("form.create", "新建");
    m.insert("form.edit", "编辑");
    m.insert("form.search", "搜索");
    m.insert("form.reset", "重置");
    m.insert("form.submit", "提交");
    m.insert("form.saving", "保存中...");
    m.insert("form.save_changes", "保存修改");
    m.insert("form.required", "此字段为必填项");
    m.insert("form.invalid_email", "请输入有效的邮箱地址");
    m.insert("form.password_too_short", "密码至少 8 位");
    m.insert("form.password_mismatch", "两次密码不一致");

    // ── 表格 ────────────────────────────────────
    m.insert("table.no_data", "暂无数据");
    m.insert("table.loading", "加载中...");
    m.insert("table.actions", "操作");
    m.insert("table.status", "状态");
    m.insert("table.created_at", "创建时间");
    m.insert("table.name", "名称");
    m.insert("table.email", "邮箱");
    m.insert("table.role", "角色");

    // ── 通用 ────────────────────────────────────
    m.insert("common.loading", "加载中");
    m.insert("common.error", "出错了");
    m.insert("common.success", "操作成功");
    m.insert("common.confirm_delete", "确定要删除吗？此操作不可撤销。");
    m.insert("common.copied", "已复制到剪贴板");
    m.insert("common.copy", "复制");
    m.insert("common.refresh", "刷新");
    m.insert("common.back", "返回");
    m.insert("common.time", "时间");
    m.insert("common.total_items", "共");
    m.insert("common.created_at_label", "创建于");
    m.insert("common.load_failed", "加载失败");
    m.insert("common.redirecting", "跳转中");
    m.insert("common.redirect_to_login", "正在跳转到登录页…");
    m.insert("common.admin_only_page", "权限不足：该页面仅管理员可访问");
    m.insert("common.expand", "展开");
    m.insert("common.collapse", "折叠");
    m.insert("common.enabled", "已启用");
    m.insert("common.disabled", "已禁用");
    m.insert("common.yes", "是");
    m.insert("common.no", "否");
    m.insert("common.admin", "管理员");
    m.insert("common.user", "普通用户");
    m.insert("common.no_permission", "您没有权限访问此页面");
    m.insert("common.balance", "余额");
    m.insert("common.amount", "金额");
    m.insert("common.currency", "货币");
    m.insert("common.tokens", "Token 数");
    m.insert("common.requests", "请求数");
    m.insert("common.cost", "费用");
    m.insert("dashboard.greeting", "你好");
    m.insert("dashboard.subtitle", "这是您的控制台概览");
    m.insert("dashboard.api_calls", "API 调用次数");
    m.insert("dashboard.weekly_total", "本周累计");
    m.insert("dashboard.balance", "账户余额");
    m.insert("dashboard.available", "可用");
    m.insert("dashboard.active_keys", "活跃 Key");
    m.insert("dashboard.total", "总计");
    m.insert("dashboard.weekly_cost", "本周消耗");
    m.insert("dashboard.used", "已用");
    m.insert("dashboard.quick_links", "快速入口");
    m.insert("dashboard.manage_api_keys", "管理 API Key");
    m.insert("dashboard.recharge", "充値余额");
    m.insert("dashboard.account_settings", "账户设置");
    m.insert(
        "api_keys.subtitle",
        "管理 OpenAI 兼容接口访问密钥。密钥明文仅在创建成功后显示一次。",
    );
    m.insert("api_keys.create", "创建 API Key");
    m.insert("api_keys.active", "活跃");
    m.insert("api_keys.all_with_revoked", "全部（含已撤销）");
    m.insert("api_keys.created_title", "API Key 已创建");
    m.insert("api_keys.created_once", "仅显示一次，请立即保存。");
    m.insert("api_keys.example", "使用示例");
    m.insert("api_keys.copy_hint", "点击复制");
    m.insert("api_keys.copied", "已复制");
    m.insert(
        "api_keys.example_note",
        "将以上配置用于 OpenAI 兼容的 SDK 或工具中。",
    );
    m.insert("api_keys.close_saved", "我已记录，关闭");
    m.insert("api_keys.create_title", "创建 API Key");
    m.insert("api_keys.name", "名称");
    m.insert("api_keys.name_placeholder", "为此 Key 取个名字");
    m.insert("api_keys.creating", "创建中...");
    m.insert("api_keys.create_failed", "创建失败");
    m.insert("api_keys.loading_failed", "加载失败");
    m.insert("api_keys.registry", "API 密钥管理");
    m.insert("api_keys.empty_meta", "当前筛选条件下没有可用凭证。");
    m.insert("api_keys.active_meta", "正在显示可用于调用网关的活跃凭证。");
    m.insert("api_keys.all_meta", "正在显示全部凭证，包括已撤销记录。");
    m.insert("api_keys.empty", "暂无可用的 API Key，点击上方按钮创建");
    m.insert("api_keys.prefix", "前缀");
    m.insert("api_keys.revoked", "已撤销");

    // ── Layout ──────────────────────────────────
    m.insert("layout.toggle_sidebar", "切换侧边栏");
    m.insert("layout.open_menu", "打开菜单");
    m.insert("layout.switch_to_light", "切换到亮色主题");
    m.insert("layout.switch_to_dark", "切换到暗色主题");
    m.insert("layout.switch_to_zh", "切换到中文");
    m.insert("layout.switch_to_en", "切换到英文");
    m.insert("layout.expand_sidebar", "展开侧边栏");
    m.insert("layout.collapse_sidebar", "折叠侧边栏");

    // ── Error ───────────────────────────────────
    m.insert("error.not_found_desc", "您访问的页面不存在或已被移除");
    m.insert("error.back_home", "返回首页");

    // ── Login ───────────────────────────────────
    m.insert("login.tagline_1", "为");
    m.insert("login.tagline_highlight", "AI 应用");
    m.insert("login.tagline_2", "提供");
    m.insert("login.tagline_3", "高性能算力中转");
    m.insert(
        "login.description",
        "统一大模型接入、智能路由调度、实时计费结算与全链路可观测性。开箱即用的企业级 AI Token 管理平台。",
    );
    m.insert("login.feature_routing", "智能路由");
    m.insert("login.feature_billing", "实时计费");
    m.insert("login.feature_ha", "高可用性");
    m.insert("login.feature_api", "API 管理");
    m.insert("login.title", "登录您的账户");
    m.insert("login.subtitle", "管理您的 AI Token 与算力资源");
    m.insert("login.email_label", "邮箱地址");
    m.insert("login.hide_password", "隐藏密码");
    m.insert("login.show_password", "显示密码");
    m.insert("login.verifying", "验证中...");
    m.insert("login.submit", "登录到控制台");
    m.insert("reset_password.failed", "重置失败");
    m.insert("reset_password.success", "密码已重置成功！");
    m.insert("reset_password.go_login", "前往登录");
    m.insert("reset_password.submit", "确认重置");

    // ── Account Settings ────────────────────────
    m.insert("account_settings.fill_all_passwords", "请填写所有密码字段");
    m.insert("account_settings.password_mismatch", "两次新密码输入不一致");
    m.insert("account_settings.password_too_short", "新密码至少需要 8 位");
    m.insert("account_settings.password_changed", "密码修改成功");
    m.insert("account_settings.change_failed", "修改失败");
    m.insert(
        "account_settings.page_desc",
        "维护登录安全信息与密码策略入口。布局参考系统诊断与系统设置页，保持纵向、紧凑、易扫描。",
    );
    m.insert("account_settings.change_password", "修改密码");
    m.insert(
        "account_settings.section_desc",
        "当前页专注于账户安全操作。输入区保持窄栏，避免在右侧无内容时被直接拉满。",
    );
    m.insert("account_settings.current_password", "当前密码");
    m.insert(
        "account_settings.current_password_desc",
        "用于确认本次操作来自当前登录账户。",
    );
    m.insert(
        "account_settings.current_password_placeholder",
        "请输入当前密码",
    );
    m.insert("account_settings.new_password", "新密码");
    m.insert(
        "account_settings.new_password_desc",
        "建议使用长度更高、包含大小写和符号的强密码。",
    );
    m.insert(
        "account_settings.new_password_placeholder",
        "请输入新密码（至少8位）",
    );
    m.insert("account_settings.confirm_password", "确认新密码");
    m.insert(
        "account_settings.confirm_password_desc",
        "再次输入相同密码，避免误录导致锁定登录。",
    );
    m.insert(
        "account_settings.confirm_password_placeholder",
        "再次输入新密码",
    );

    // ── Profile ─────────────────────────────────
    m.insert("profile.saved", "保存成功");
    m.insert("profile.save_failed", "保存失败");
    m.insert(
        "profile.page_desc",
        "查看当前账户身份信息，并维护控制台显示名称。",
    );
    m.insert("profile.tenant", "租户");
    m.insert("profile.user_id", "用户 ID");
    m.insert("profile.edit", "编辑资料");

    // ── Usage ───────────────────────────────────
    m.insert("usage.subtitle", "查看 API 调用记录与 Token 消耗");
    m.insert("usage.calls", "调用次数");
    m.insert("usage.total_calls", "总调用次数");
    m.insert("usage.period", "统计周期");
    m.insert("usage.total_tokens", "总 Token 数");
    m.insert("usage.prompt_tokens", "提示词");
    m.insert("usage.completion_tokens", "补全");
    m.insert("usage.total_cost", "累计费用");
    m.insert("usage.usage_billed", "按使用量计费");
    m.insert("usage.trend", "调用趋势");
    m.insert("usage.records", "调用记录");
    m.insert("usage.no_records", "暂无记录");
    m.insert("usage.model", "模型");
    m.insert("usage.total_token", "总 Token");

    // ── Payments ────────────────────────────────
    m.insert("payments.title", "支付与账单");
    m.insert("payments.subtitle", "查看账户余额、充值记录与账单明细");
    m.insert("payments.recharge_now", "立即充值");
    m.insert("payments.account_balance", "账户余额");
    m.insert("payments.frozen_amount", "冻结金额");
    m.insert("payments.total_recharge", "总充值");
    m.insert("payments.total_consumed", "总消耗");
    m.insert("payments.usage_requests", "用量请求数");
    m.insert("payments.input_tokens", "输入 Tokens");
    m.insert("payments.output_tokens", "输出 Tokens");
    m.insert("payments.total_tokens", "总 Tokens");
    m.insert("payments.total_cost", "总费用");
    m.insert("payments.recharge_records", "充值记录");
    m.insert("payments.no_recharge_records", "暂无充值记录");
    m.insert("payments.order_no", "订单号");
    m.insert("payments.subject", "主题");
    m.insert("payments.usage_details", "用量明细");
    m.insert("payments.no_usage_records", "暂无用量记录");

    // ── Distribution ────────────────────────────
    m.insert("distribution.title", "分销管理");
    m.insert("distribution.subtitle", "查看您的分销收益和推荐记录");
    m.insert("distribution.fetch_failed", "获取失败");
    m.insert("distribution.total_earnings", "总收益");
    m.insert("distribution.available_balance", "可用余额");
    m.insert("distribution.pending", "待结算");
    m.insert("distribution.referral_count", "推荐人数");
    m.insert("distribution.my_referral_code", "我的推荐码");
    m.insert("distribution.referral_code", "推荐码");
    m.insert("distribution.invite_link", "邀请链接");
    m.insert("distribution.referral_users", "推荐用户");
    m.insert("distribution.user", "用户");
    m.insert("distribution.joined_at", "加入时间");
    m.insert("distribution.total_spent", "消费总额");
    m.insert("distribution.my_earnings", "我的收益");
    m.insert("distribution.no_referrals", "暂无推荐记录");

    // ── Settings ────────────────────────────────
    m.insert(
        "settings.admin_desc",
        "按运行策略统一管理平台参数，保持控制台配置紧凑、清晰、可审阅。",
    );
    m.insert(
        "settings.user_desc",
        "查看系统运行配置（仅供参考）。仅管理员可以修改全局参数。",
    );
    m.insert(
        "settings.admin_only_hint",
        "系统设置仅 Admin 可修改。个人语言与主题偏好请通过顶部导航栏右侧按钮切换。",
    );
    m.insert("settings.load_failed", "设置加载失败");
    m.insert("settings.saved", "设置已保存");
    m.insert("settings.basic_title", "基础配置");
    m.insert(
        "settings.basic_desc",
        "定义平台名称、新用户赠送额度和充值基础参数。界面刻意保持窄栏，避免输入区在宽屏下失控铺开。",
    );
    m.insert("settings.site_name_label", "平台名称");
    m.insert(
        "settings.site_name_desc",
        "显示在登录页、后台导航和邮件模板中的平台名称。",
    );
    m.insert("settings.default_user_quota_label", "新用户默认赠送额度");
    m.insert(
        "settings.default_user_quota_desc",
        "运行时按此值决定新用户注册赠送额度；只有大于 0 才会赠送，0 或负数表示不赠送。",
    );
    m.insert("settings.default_currency_label", "默认货币");
    m.insert(
        "settings.default_currency_desc",
        "影响后台金额展示、订单默认币种和部分前端文案。",
    );
    m.insert("settings.min_recharge_label", "最低充值金额");
    m.insert(
        "settings.min_recharge_desc",
        "限制单次充值的最低金额，避免异常小额订单进入支付链路。",
    );
    m.insert("settings.security_title", "安全配置");
    m.insert(
        "settings.security_desc",
        "控制令牌有效期等安全参数。新用户注册始终强制邮箱验证码验证。",
    );
    m.insert("settings.jwt_expire_label", "JWT Token 有效期（小时）");
    m.insert(
        "settings.jwt_expire_desc",
        "登录后访问令牌的默认有效期。时间越长，体验更顺滑，但凭证暴露窗口也更大。",
    );
    m.insert("settings.save_failed", "保存失败");
    m.insert("settings.non_negative", "值不能为负数");
    m.insert("settings.invalid_number", "请输入有效的数字");

    // ── Pricing ─────────────────────────────────
    m.insert("pricing.admin_desc", "管理平台定价策略，设置模型调用费率");
    m.insert("pricing.user_desc", "查看当前平台可用的定价策略");
    m.insert("pricing.create", "+ 新建定价");
    m.insert("pricing.empty", "暂无定价策略");
    m.insert("pricing.table_title", "模型定价表");
    m.insert(
        "pricing.table_subtitle",
        "统一查看各模型的 Provider 归属、输入输出费率和默认策略，保证计费配置可审阅且易于对比。",
    );
    m.insert("pricing.items_suffix", "条");
    m.insert("pricing.model_provider", "模型 / Provider");
    m.insert("pricing.input_price", "输入价格");
    m.insert("pricing.output_price", "输出价格");
    m.insert("pricing.billing_status", "计费状态");
    m.insert("pricing.input_tokens", "input tokens");
    m.insert("pricing.output_tokens", "output tokens");
    m.insert("pricing.default", "默认");
    m.insert("pricing.alternative", "备选");
    m.insert("pricing.default_note", "当前模型计费默认落在这条规则");
    m.insert("pricing.alternative_note", "未设为默认，需手动切换后生效");
    m.insert("pricing.set_default_ok", "已设为默认定价");
    m.insert("pricing.set_default_failed", "设置默认失败");
    m.insert("pricing.set_default", "设为默认");
    m.insert("pricing.deleted", "定价已删除");
    m.insert("pricing.delete_failed", "删除失败");
    m.insert("pricing.created", "定价创建成功");
    m.insert("pricing.updated", "定价更新成功");
    m.insert("pricing.fill_all", "请填写所有字段");
    m.insert("pricing.invalid_input_price", "输入单价格式不正确");
    m.insert("pricing.invalid_output_price", "输出单价格式不正确");
    m.insert("pricing.negative_input_price", "输入单价不能为负数");
    m.insert("pricing.negative_output_price", "输出单价不能为负数");
    m.insert("pricing.create_failed", "创建失败");
    m.insert("pricing.update_failed", "更新失败");
    m.insert("pricing.create_title", "新建定价");
    m.insert("pricing.edit_title", "编辑定价");
    m.insert("pricing.model_name", "模型名称");
    m.insert("pricing.model_placeholder", "如 gpt-4o");
    m.insert("pricing.input_price_label", "输入单价（每1K tokens）");
    m.insert("pricing.output_price_label", "输出单价（每1K tokens）");
    m.insert("pricing.input_placeholder", "如 0.000005");
    m.insert("pricing.output_placeholder", "如 0.000015");
    m.insert("pricing.currency_cny", "CNY（人民币）");
    m.insert("pricing.currency_usd", "USD（美元）");
    m.insert("pricing.creating", "创建中...");

    // ── Dashboard ───────────────────────────────
    m.insert(
        "dashboard.subtitle_long",
        "这是您的控制台概览，下面是当前账户的实时指标、最近活动与关键操作入口。",
    );
    m.insert("dashboard.balance_available", "可用余额");
    m.insert("dashboard.total_cost", "累计费用");
    m.insert("dashboard.meta_usage", "来自真实用量聚合");
    m.insert("dashboard.meta_balance", "账户余额实时返回");
    m.insert("dashboard.meta_keys", "当前启用中的密钥");
    m.insert("dashboard.meta_cost", "真实 usage_logs 聚合");
    m.insert("dashboard.recent_active_days", "最近 7 个活跃日");
    m.insert(
        "dashboard.recent_active_days_desc",
        "按真实请求记录聚合，快速判断近期活跃度变化。",
    );
    m.insert("dashboard.live_data", "实时数据");
    m.insert(
        "dashboard.quick_links_desc",
        "围绕充值、密钥和账户操作组织控制台主路径。",
    );
    m.insert("dashboard.manage_api_keys_desc", "创建、查看与吊销访问密钥");
    m.insert("dashboard.payments", "支付与账单");
    m.insert("dashboard.payments_desc", "查看余额、充值记录和订单状态");
    m.insert("dashboard.usage_details", "用量明细");
    m.insert(
        "dashboard.usage_details_desc",
        "审阅模型调用、Tokens 与费用",
    );
    m.insert("dashboard.account_settings_desc", "更新个人资料与安全信息");
    m.insert("dashboard.recent_calls", "最近调用");
    m.insert(
        "dashboard.recent_calls_desc",
        "使用真实 usage 记录作为控制台活动流。",
    );
    m.insert("dashboard.no_recent_calls", "暂无最近调用记录。");
    m.insert("dashboard.active_keys_panel", "活跃密钥");
    m.insert(
        "dashboard.active_keys_panel_desc",
        "只展示仍在启用状态的 Key。",
    );
    m.insert("dashboard.no_active_keys", "暂无活跃密钥。");
    m.insert("dashboard.system_status", "系统状态");
    m.insert("dashboard.account_status", "账户状态");
    m.insert(
        "dashboard.system_status_desc",
        "管理员可见的网关与 Provider 健康摘要。",
    );
    m.insert(
        "dashboard.account_status_desc",
        "围绕余额、分销和订单状态汇总当前账户。",
    );
    m.insert("dashboard.online", "在线");
    m.insert("dashboard.pending_check", "待检查");
    m.insert("dashboard.gateway_providers", "网关 Provider");
    m.insert("dashboard.gateway_providers_desc", "已加载 Provider 数量");
    m.insert("dashboard.healthy_providers", "健康 Provider");
    m.insert("dashboard.healthy_providers_desc", "当前健康的路由目标");
    m.insert("dashboard.account_cache", "渠道状态缓存");
    m.insert("dashboard.account_cache_desc", "账号状态存储中的条目数");
    m.insert("dashboard.fallback_count", "Fallback 次数");
    m.insert("dashboard.fallback_count_desc", "来自真实网关统计");
    m.insert("dashboard.total_distribution_earnings", "总分销收益");
    m.insert("dashboard.total_distribution_earnings_desc", "累计推荐收益");
    m.insert("dashboard.pending_distribution_earnings", "待结算收益");
    m.insert(
        "dashboard.pending_distribution_earnings_desc",
        "尚未结算到可提现金额",
    );
    m.insert("dashboard.referral_count_desc", "当前已绑定推荐关系");
    m.insert("dashboard.latest_order", "最近订单");
    m.insert("dashboard.latest_order_desc", "最近一笔充值订单状态");
    m.insert("dashboard.none", "暂无");
    m.insert("dashboard.last_used_prefix", "最近使用");
    m.insert("dashboard.no_usage_record", "暂无使用记录");
    m.insert(
        "system.subtitle",
        "查看 Provider 健康状态、网关运行统计和路由调试信息",
    );
    m.insert("system.provider_health", "Provider 健康状态");
    m.insert("system.no_healthy_provider", "当前没有健康 Provider");
    m.insert("system.gateway_stats", "网关运行统计");
    m.insert("system.total_requests", "总请求数");
    m.insert("system.success_rate", "成功率");
    m.insert("system.avg_latency", "平均响应时间");
    m.insert("system.fallback_count", "Fallback 次数");
    m.insert("system.routing_debug", "路由调试");
    m.insert("system.provider_status_diagnosis", "Provider 状态诊断");
    m.insert("system.route_success", "路由成功");
    m.insert("system.primary_target", "主目标");
    m.insert("system.fallback_chain", "备用链路");
    m.insert("system.items", "个");
    m.insert("system.route_failed", "路由失败");
    m.insert("system.provider_status", "Provider 状态");
    m.insert("system.no_provider_configured", "未配置任何 Provider");
    m.insert("system.health_status", "健康状态");
    m.insert("system.account_count", "账号数量");
    m.insert("system.healthy", "健康");
    m.insert("system.unhealthy", "不健康");
    m.insert("system.pricing_info", "定价信息");
    m.insert("system.degraded", "降级");
    m.insert("system.abnormal", "异常");
    m.insert("system.unknown", "未知");
    m.insert("users.subtitle", "查看和管理平台所有注册用户");
    m.insert("users.search_placeholder", "搜索邮箱或用户名...");
    m.insert("users.empty", "暂无用户数据");
    m.insert("users.user", "用户");
    m.insert("users.tenant", "租户");
    m.insert("users.registered_at", "注册时间");
    m.insert("users.updated", "用户信息已更新");
    m.insert("users.update_failed", "更新失败");
    m.insert("users.deleted", "用户已删除");
    m.insert("users.delete_failed", "删除失败");
    m.insert("users.edit_title", "编辑用户");
    m.insert("users.display_name", "显示名称");
    m.insert("users.display_name_placeholder", "留空则不修改");
    m.insert("users.role_user", "user（普通用户）");
    m.insert("users.role_admin", "admin（管理员）");
    m.insert("users.delete_confirm_title", "确认删除");
    m.insert("users.delete_confirm_prefix", "确定要删除用户");
    m.insert("users.delete_confirm_suffix", "吗？此操作不可撤销。");
    m.insert("users.deleting", "删除中...");
    m.insert("users.confirm_delete", "确认删除");
    m.insert("users.self_title", "我的账户");
    m.insert("users.self_desc", "查看和管理您的个人账户信息");
    m.insert("users.account_info", "账户信息");
    m.insert("tenants.subtitle", "查看和管理平台所有租户信息");
    m.insert("tenants.search_placeholder", "搜索租户名称或 ID...");
    m.insert("tenants.empty", "暂无租户数据");
    m.insert("tenants.tenant_id", "租户 ID");
    m.insert("tenants.active", "活跃");
    m.insert(
        "distribution_records.admin_desc",
        "查看全平台分销收益记录，及当前生效的分销规则",
    );
    m.insert(
        "distribution_records.user_desc",
        "查看您通过邀请获得的分销收益明细",
    );
    m.insert("distribution_records.rules_title", "分销规则（只读）");
    m.insert(
        "distribution_records.rules_hint",
        "分销规则由平台运营方统一配置，如需调整请联系系统管理员。",
    );
    m.insert("distribution_records.no_rules", "当前无分销规则");
    m.insert("distribution_records.rule_name", "规则名称");
    m.insert("distribution_records.commission_rate", "分销比例");
    m.insert("distribution_records.empty_admin", "暂无分销记录");
    m.insert("distribution_records.record_id", "记录编号");
    m.insert("distribution_records.source_user_id", "来源用户 ID");
    m.insert("distribution_records.amount_spent", "消费金额");
    m.insert("distribution_records.commission_amount", "分销金额");
    m.insert("distribution_records.referrer_id", "推荐人 ID");
    m.insert("distribution_records.empty_user", "暂无推荐记录");
    m.insert("distribution_records.referred_user", "被推荐用户");
    m.insert(
        "accounts.subtitle",
        "统一维护各 Provider 渠道、模型映射与可用性状态，确保路由层始终有可审阅的账号资产池。",
    );
    m.insert("accounts.reset_failed", "重置失败");
    m.insert("accounts.fill_required", "请填写必填项");
    m.insert("accounts.created", "渠道已创建");
    m.insert("accounts.create_failed", "创建失败");
    m.insert("accounts.name_required", "渠道名称不能为空");
    m.insert("accounts.updated", "渠道已更新");
    m.insert("accounts.update_failed", "更新失败");
    m.insert("accounts.resetting", "重置中...");
    m.insert("accounts.reset_health", "重置健康状态");
    m.insert("accounts.add_channel", "+ 新增渠道");
    m.insert("accounts.empty", "暂无渠道配置，请点击“新增渠道”添加");
    m.insert("accounts.table_title", "渠道资产表");
    m.insert(
        "accounts.table_subtitle",
        "按 Provider 汇总当前账号池的可用状态、模型覆盖和速率余量。",
    );
    m.insert("accounts.channels_suffix", "个渠道");
    m.insert("accounts.channel", "渠道");
    m.insert("accounts.provider_model", "Provider / 模型");
    m.insert("accounts.runtime_status", "运行状态");
    m.insert("accounts.rate_quota", "速率配额");
    m.insert("accounts.key_preview", "密钥预览");
    m.insert("accounts.default_endpoint", "使用 Provider 默认 Endpoint");
    m.insert("accounts.no_models", "未配置模型");
    m.insert("accounts.route_ready", "可参与正常路由");
    m.insert("accounts.enabled_but_unhealthy", "已启用，但健康状态异常");
    m.insert("accounts.not_routed", "当前不参与路由调度");
    m.insert("accounts.rpm_label", "当前 RPM / 上限");
    m.insert("accounts.last_used", "最近使用");
    m.insert("accounts.no_usage_record", "暂无记录");
    m.insert("accounts.test_success", "连接测试成功");
    m.insert("accounts.test_failed", "测试失败");
    m.insert("accounts.test", "测试");
    m.insert("accounts.create_title", "新增 LLM 渠道");
    m.insert("accounts.channel_name", "渠道名称 *");
    m.insert("accounts.channel_name_placeholder", "如 OpenAI 官方");
    m.insert("accounts.provider", "Provider *");
    m.insert(
        "accounts.supported_models",
        "支持模型（可选，留空使用默认）",
    );
    m.insert(
        "accounts.models_hint",
        "多个模型用逗号分隔，留空则使用该 Provider 的默认模型",
    );
    m.insert("accounts.api_key", "API Key *");
    m.insert("accounts.custom_base_url", "自定义 Base URL（可选）");
    m.insert("accounts.edit_title", "编辑 LLM 渠道");
    m.insert("accounts.new_api_key", "新 API Key（留空则不修改）");
    m.insert("accounts.new_api_key_placeholder", "留空不修改当前 Key");
    m.insert(
        "accounts.custom_base_url_optional",
        "自定义 Base URL（留空则不修改）",
    );
    m.insert("accounts.enable_channel", "启用渠道");
    m.insert("accounts.delete_confirm_title", "确认删除");
    m.insert("accounts.delete_confirm_prefix", "确定要删除渠道「");
    m.insert("accounts.delete_confirm_suffix", "」吗？该操作不可恢复。");
    m.insert("accounts.deleted", "渠道已删除");
    m.insert("accounts.delete_failed", "删除失败");
    m.insert("accounts.deleting", "删除中...");
    m.insert("accounts.confirm_delete", "确认删除");
    m.insert("accounts.no_permission_title", "暂无访问权限");
    m.insert(
        "accounts.no_permission_desc",
        "您没有访问「{resource}」的权限，请联系管理员",
    );
    m.insert(
        "accounts.models_placeholder_openai",
        "如: gpt-4o, gpt-4o-mini, gpt-4-turbo",
    );
    m.insert(
        "accounts.models_placeholder_claude",
        "如: claude-3-5-sonnet-latest, claude-3-opus-latest",
    );
    m.insert(
        "accounts.models_placeholder_deepseek",
        "如: deepseek-chat, deepseek-coder",
    );
    m.insert(
        "accounts.models_placeholder_gemini",
        "如: gemini-1.5-pro, gemini-1.5-flash",
    );
    m.insert(
        "accounts.models_placeholder_vllm",
        "输入 vLLM 支持的模型名称，多个用逗号分隔",
    );
    m.insert(
        "accounts.models_placeholder_ollama",
        "输入 Ollama 模型名称，多个用逗号分隔",
    );
    m.insert(
        "accounts.models_placeholder_default",
        "输入模型名称，多个用逗号分隔",
    );

    m
});
