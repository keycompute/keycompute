use std::collections::HashMap;
pub(super) fn register(map: &mut HashMap<&'static str, &'static str>, zh: bool) {
    for (key, cn, en) in [
        ("tenant_pricing.search", "搜索模型", "Search models"),
        ("tenant_pricing.title", "租户定价", "Tenant pricing"),
        (
            "tenant_pricing.hint",
            "仅管理当前租户拥有的定价；平台共享定价仍由平台管理。",
            "Manage prices owned by this tenant only. Platform shared prices remain platform-managed.",
        ),
        ("tenant_pricing.scope", "当前租户：", "Current tenant:"),
        (
            "tenant_pricing.create",
            "新增租户定价",
            "Create tenant price",
        ),
        ("tenant_pricing.edit", "编辑定价", "Edit price"),
        ("tenant_pricing.delete", "删除定价", "Delete price"),
        ("tenant_pricing.default", "设为默认", "Make default"),
        ("tenant_pricing.default_badge", "默认", "Default"),
        ("tenant_pricing.model", "模型名称", "Model name"),
        ("tenant_pricing.dimension", "计费维度", "Billing dimension"),
        ("tenant_pricing.currency", "币种", "Currency"),
        (
            "tenant_pricing.prices",
            "每千 token：输入 / 输出",
            "Per 1k tokens: input / output",
        ),
        (
            "tenant_pricing.input",
            "每千输入 token 价格",
            "Price per 1k input tokens",
        ),
        (
            "tenant_pricing.output",
            "每千输出 token 价格",
            "Price per 1k output tokens",
        ),
        ("tenant_pricing.validity", "生效状态", "Validity"),
        ("tenant_pricing.effective", "当前有效", "Effective"),
        (
            "tenant_pricing.ineffective",
            "当前未生效或已过期",
            "Not currently effective",
        ),
        ("tenant_pricing.window", "生效时间范围", "Effective window"),
        ("tenant_pricing.version", "版本", "Version"),
        (
            "tenant_pricing.from",
            "开始时间（RFC3339）",
            "Start time (RFC3339)",
        ),
        (
            "tenant_pricing.until",
            "结束时间（RFC3339）",
            "End time (RFC3339)",
        ),
        (
            "tenant_pricing.create_times",
            "开始留空表示立即生效，结束留空表示无到期时间。金额最多10位整数、10位小数，不进行浮点舍入。",
            "Empty start means now; empty end means no expiration. Prices allow 10 integer and 10 fractional digits without float rounding.",
        ),
        (
            "tenant_pricing.edit_times",
            "模型、币种、维度和开始时间不可变。结束留空保留原到期时间，不会清除。修改必须匹配打开时的版本。",
            "Model, currency, dimension and start are immutable. Empty end keeps the previous expiration. Updates require the displayed version.",
        ),
        (
            "tenant_pricing.delete_hint",
            "删除当前租户的这条定价。后续请求按服务端剩余定价规则解析，既有账务记录不由此页面重写。",
            "Delete this tenant-owned price. Subsequent requests use server pricing resolution; this page does not rewrite historical billing.",
        ),
        (
            "tenant_pricing.default_hint",
            "将这条租户定价设为默认。平台共享定价不会因此变成本租户资源。",
            "Mark this tenant price as default. Platform shared pricing does not become tenant-owned.",
        ),
    ] {
        map.insert(key, if zh { cn } else { en });
    }
}
