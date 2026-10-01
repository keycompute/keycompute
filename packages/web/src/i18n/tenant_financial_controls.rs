use std::collections::HashMap;
pub(super) fn register(m: &mut HashMap<&'static str, &'static str>, en: bool) {
    for (k, a, b) in [
        (
            "tenant_financial_controls.title",
            "Financial controls",
            "财务控制",
        ),
        (
            "tenant_financial_controls.hint",
            "Recover expired request reservations and review tenant payout requests.",
            "回收过期请求预留并审核租户提现申请。",
        ),
        (
            "tenant_financial_controls.boundary",
            "Tenant administrators cannot adjust balances, reveal payout recipients, attest external payment, or invoke payment channels here.",
            "租户管理员不能在此调整余额、查看收款人明文、确认外部付款或调用支付渠道。",
        ),
        (
            "tenant_financial_controls.reservations",
            "Expired reservations",
            "过期预留",
        ),
        (
            "tenant_financial_controls.withdrawals",
            "Withdrawal review",
            "提现审核",
        ),
        (
            "tenant_financial_controls.owner",
            "Member UUID",
            "成员 UUID",
        ),
        ("tenant_financial_controls.load", "Load", "加载"),
        (
            "tenant_financial_controls.choose_owner",
            "Select a member UUID to inspect that member's active request reservations.",
            "请选择成员 UUID 查看该成员当前请求预留。",
        ),
        (
            "tenant_financial_controls.reservation_hint",
            "Only an expired request reservation with the exact observed version can be recovered. Active reservations stay protected.",
            "仅允许使用刚读取到的精确版本回收已过期请求预留；活动预留继续受到保护。",
        ),
        (
            "tenant_financial_controls.request_reserved",
            "Request reserved",
            "请求预留",
        ),
        (
            "tenant_financial_controls.manual_frozen",
            "Manual frozen",
            "手工冻结",
        ),
        (
            "tenant_financial_controls.request",
            "Request / version",
            "请求 / 版本",
        ),
        ("tenant_financial_controls.expires", "Expires", "过期时间"),
        (
            "tenant_financial_controls.release",
            "Recover expired reservation",
            "回收过期预留",
        ),
        (
            "tenant_financial_controls.release_warning",
            "This does not cancel upstream work. Late accepted usage may still debit the wallet after recovery.",
            "这不会取消上游任务；回收后迟到但已接受的用量仍可能扣减钱包余额。",
        ),
        ("tenant_financial_controls.reason", "Reason", "原因"),
        (
            "tenant_financial_controls.withdrawal_hint",
            "Review metadata only. Recipient account/name stay encrypted and are available only to the explicit platform payout support flow.",
            "这里只审核安全元数据；收款账号和姓名保持加密，仅显式平台付款支持流程可读取。",
        ),
        (
            "tenant_financial_controls.apply",
            "Apply filters",
            "应用筛选",
        ),
        ("tenant_financial_controls.kind", "Type", "类型"),
        ("tenant_financial_controls.created", "Created", "创建时间"),
        ("tenant_financial_controls.approve", "Approve", "批准"),
        ("tenant_financial_controls.reject", "Reject", "拒绝"),
        (
            "tenant_financial_controls.review",
            "Review withdrawal",
            "审核提现",
        ),
        (
            "tenant_financial_controls.approve_hint",
            "Approval does not send money or mark an external payout completed.",
            "批准不会发送资金，也不会把外部付款标记为已完成。",
        ),
        (
            "tenant_financial_controls.reject_hint",
            "Reject the current revision with an auditable reason.",
            "使用当前版本并填写可审计原因拒绝申请。",
        ),
    ] {
        m.insert(k, if en { a } else { b });
    }
}
