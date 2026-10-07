use std::collections::HashMap;
pub(super) fn register(m: &mut HashMap<&'static str, &'static str>, en: bool) {
    for (k, e, z) in [
        (
            "tenant_distribution.title",
            "Distribution policies",
            "分配规则",
        ),
        (
            "tenant_distribution.hint",
            "Configure future tenant distribution allocation rules. This page does not settle or pay existing earnings.",
            "配置租户未来的分配规则；本页不会结算或支付既有收益。",
        ),
        ("tenant_distribution.scope", "Current tenant", "当前租户"),
        ("tenant_distribution.policy_id", "Policy ID", "规则 ID"),
        ("tenant_distribution.create", "Create policy", "新增规则"),
        ("tenant_distribution.edit", "Edit policy", "编辑规则"),
        ("tenant_distribution.delete", "Delete policy", "删除规则"),
        (
            "tenant_distribution.default",
            "Set tenant default",
            "设置租户默认规则",
        ),
        ("tenant_distribution.name", "Name", "名称"),
        ("tenant_distribution.description", "Description", "描述"),
        ("tenant_distribution.beneficiary", "Beneficiary", "受益范围"),
        ("tenant_distribution.everyone", "Everyone", "全部成员"),
        ("tenant_distribution.member", "Tenant member", "租户成员"),
        (
            "tenant_distribution.member_hint",
            "Select an active member suggestion or enter an explicit member ID; the server verifies current membership.",
            "可选择有效成员建议或输入明确的成员 ID；服务端会验证当前成员关系。",
        ),
        (
            "tenant_distribution.immutable_beneficiary",
            "Beneficiary identity is immutable; create another policy to change it.",
            "受益对象不可在编辑时改变；需要变更时请新建规则。",
        ),
        ("tenant_distribution.rate", "Commission rate", "分配比例"),
        (
            "tenant_distribution.rate_hint",
            "Exact decimal from 0 to 1 with at most four decimal places; no percentage or float conversion.",
            "使用 0 到 1 的精确小数，最多四位小数；不使用百分号或浮点转换。",
        ),
        ("tenant_distribution.priority", "Priority", "优先级"),
        ("tenant_distribution.state", "State", "状态"),
        ("tenant_distribution.active", "Active", "启用"),
        ("tenant_distribution.inactive", "Inactive", "停用"),
        (
            "tenant_distribution.from",
            "Effective from (UTC)",
            "生效时间（UTC）",
        ),
        (
            "tenant_distribution.until",
            "Effective until (UTC) / blank to clear",
            "结束时间（UTC）/ 留空清除",
        ),
        (
            "tenant_distribution.window",
            "Validity / revision",
            "有效期 / 版本",
        ),
        ("tenant_distribution.reason", "Change reason", "变更原因"),
        (
            "tenant_distribution.effect_hint",
            "Changes affect future distribution policy resolution only; existing settlement records are not rewritten.",
            "修改只影响未来的分配规则解析，不会改写已有结算记录。",
        ),
        (
            "tenant_distribution.delete_hint",
            "Delete exactly the displayed revision of this policy.",
            "仅删除当前页面显示版本对应的规则。",
        ),
    ] {
        m.insert(k, if en { e } else { z });
    }
}
