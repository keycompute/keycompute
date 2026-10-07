use std::collections::HashMap;
pub(super) fn register(m: &mut HashMap<&'static str, &'static str>, en: bool) {
    for (key, english, chinese) in [
        ("tenant_finance.title", "Tenant finances", "租户财务"),
        (
            "tenant_finance.hint",
            "View this tenant's usage, payment records and individual member wallets.",
            "查看本租户用量、支付订单和成员独立钱包。",
        ),
        (
            "tenant_finance.shared_payment",
            "Payment channels are platform-wide; these records remain tenant-scoped. This page performs no money movements.",
            "支付渠道由平台统一提供，业务记录按租户隔离；本页不执行资金变更。",
        ),
        ("tenant_finance.usage", "Usage and billing", "用量与账单"),
        ("tenant_finance.orders", "Payment records", "支付记录"),
        ("tenant_finance.wallet", "Member wallet", "成员钱包"),
        ("tenant_finance.owner", "Member ID", "成员 ID"),
        (
            "tenant_finance.owner_placeholder",
            "Optional member ID",
            "可选：填写成员 ID",
        ),
        ("tenant_finance.from", "From (UTC)", "起始时间（UTC）"),
        (
            "tenant_finance.to",
            "Until, exclusive (UTC)",
            "截止时间，不含（UTC）",
        ),
        (
            "tenant_finance.apply",
            "Apply report filters",
            "应用报表筛选",
        ),
        (
            "tenant_finance.active_owner",
            "Current member filter:",
            "当前成员筛选：",
        ),
        (
            "tenant_finance.all_owners",
            "All members of this tenant",
            "本租户全部成员",
        ),
        (
            "tenant_finance.window",
            "Applied usage window (maximum 31 days):",
            "已应用的用量区间（最多 31 天）：",
        ),
        (
            "tenant_finance.currency_totals",
            "Totals by currency",
            "按币种汇总",
        ),
        ("tenant_finance.amount", "Recorded amount", "记录金额"),
        ("tenant_finance.requests", "Requests", "请求数"),
        (
            "tenant_finance.tokens",
            "Input / output / total tokens",
            "输入 / 输出 / 总 token",
        ),
        (
            "tenant_finance.model",
            "Model / provider",
            "模型 / 上游协议",
        ),
        ("tenant_finance.state", "Payment state", "支付状态"),
        ("tenant_finance.state_pending", "Pending", "待支付"),
        ("tenant_finance.state_paid", "Paid", "已支付"),
        ("tenant_finance.state_failed", "Failed", "失败"),
        ("tenant_finance.state_closed", "Closed", "已关闭"),
        (
            "tenant_finance.all_states",
            "All payment states",
            "全部支付状态",
        ),
        ("tenant_finance.created", "Created", "创建时间"),
        ("tenant_finance.order", "Payment order", "支付订单"),
        (
            "tenant_finance.details",
            "Inspect financial metadata",
            "查看财务元数据",
        ),
        (
            "tenant_finance.safe_fields",
            "Read-only metadata. Payment URLs, credentials, callback payloads and conversation content are not displayed.",
            "只读元数据，不显示支付链接、凭证、回调载荷或对话内容。",
        ),
        (
            "tenant_finance.choose_wallet",
            "Select and apply a member to read that member's wallet.",
            "请选择并应用成员，以查看该成员钱包。",
        ),
        (
            "tenant_finance.wallet_scope",
            "An individual wallet in this tenant, not a shared tenant balance pool. Values remain in the backend wallet's units without conversion.",
            "这是成员在本租户的独立钱包，不是租户共享余额池；金额保持后端钱包单位，不做换算。",
        ),
        ("tenant_finance.available", "Available balance", "可用余额"),
        ("tenant_finance.frozen", "Frozen balance", "冻结余额"),
        ("tenant_finance.recharged", "Total recharged", "累计充值"),
        ("tenant_finance.consumed", "Total consumed", "累计消费"),
        ("tenant_finance.as_of", "Snapshot at", "快照时间"),
        (
            "tenant_finance.uninitialized",
            "The member exists but no wallet has been initialized. Viewing this record does not create a wallet.",
            "该成员存在，但尚未初始化钱包；查看本页不会创建钱包。",
        ),
    ] {
        m.insert(key, if en { english } else { chinese });
    }
}
