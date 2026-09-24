use std::collections::HashMap;
pub(super) fn register(m: &mut HashMap<&'static str, &'static str>, en: bool) {
    for (key, english, chinese) in [
        ("tenant_keys.title", "Tenant API Keys", "租户 API Key 管理"),
        (
            "tenant_keys.hint",
            "Manage tenant key metadata; each owner receives their own secrets.",
            "管理本租户 Key 元数据；明文只能由 Key 拥有者领取。",
        ),
        (
            "tenant_keys.metadata_only",
            "Issuance and rotation create a pending request, not a readable key. Existing keys stay unchanged until the owner claims.",
            "签发和轮换只创建待领取申请，不返回明文；拥有者领取前不改变原 Key。",
        ),
        (
            "tenant_keys.my_requests",
            "My pending key requests",
            "我的待领取 Key",
        ),
        ("tenant_keys.keys", "Key metadata", "Key 元数据"),
        ("tenant_keys.pending", "Pending issuance", "待领取申请"),
        ("tenant_keys.owner", "Key owner UUID", "Key 拥有者 UUID"),
        (
            "tenant_keys.filter",
            "Apply key owner filter",
            "应用 Key 拥有者筛选",
        ),
        (
            "tenant_keys.active_filter",
            "Current key owner filter:",
            "当前 Key 拥有者筛选：",
        ),
        (
            "tenant_keys.all_owners",
            "All owners in this tenant",
            "本租户所有拥有者",
        ),
        (
            "tenant_keys.include_revoked",
            "Include revoked keys",
            "包含已撤销 Key",
        ),
        ("tenant_keys.request", "Request a new key", "申请签发 Key"),
        ("tenant_keys.edit", "Edit key metadata", "编辑 Key 元数据"),
        ("tenant_keys.rotate", "Request key rotation", "申请轮换 Key"),
        ("tenant_keys.revoke", "Revoke key", "撤销 Key"),
        ("tenant_keys.delete", "Remove key", "移除 Key"),
        (
            "tenant_keys.cancel_request",
            "Cancel issuance request",
            "取消签发申请",
        ),
        ("tenant_keys.name", "Key name", "Key 名称"),
        ("tenant_keys.state", "Key state", "Key 状态"),
        ("tenant_keys.expiration", "Key expiration", "Key 到期时间"),
        (
            "tenant_keys.version",
            "Observed metadata version:",
            "已观察的元数据版本：",
        ),
        ("tenant_keys.revoked", "Revoked", "已撤销"),
        ("tenant_keys.expired", "Expired", "已过期"),
        ("tenant_keys.active", "Active", "有效"),
        ("tenant_keys.never", "Never expires", "永不过期"),
        ("tenant_keys.requester", "Requested by:", "申请人："),
        ("tenant_keys.replaces", "Key to replace:", "待替换 Key："),
        (
            "tenant_keys.claim_by",
            "Claim request before:",
            "申请领取截止：",
        ),
        (
            "tenant_keys.keep_expiry",
            "Keep existing expiration",
            "保留现有到期时间",
        ),
        (
            "tenant_keys.set_expiry",
            "Set explicit expiration",
            "指定到期时间",
        ),
        (
            "tenant_keys.expiry_time",
            "Expiration (RFC3339)",
            "到期时间（RFC3339）",
        ),
        (
            "tenant_keys.requested",
            "Request recorded. The owner must claim it before it expires.",
            "申请已登记，需由拥有者在领取截止前确认领取。",
        ),
        (
            "tenant_keys.already_pending",
            "The same rotation request is already pending; no key has been issued again.",
            "相同轮换申请已在等待领取，没有重复签发 Key。",
        ),
        (
            "tenant_keys.revoked_result",
            "Key revoked; original ownership and history retained.",
            "Key 已撤销，原始归属和历史记录保留。",
        ),
        (
            "tenant_keys.deleted_result",
            "Unused key removed.",
            "未被历史记录引用的 Key 已移除。",
        ),
        (
            "tenant_keys.retained_result",
            "Key revoked, not physically deleted: existing records retain its identity.",
            "Key 已撤销，但未物理删除：已有记录仍需保留其身份。",
        ),
        (
            "tenant_keys.cancelled_result",
            "Issuance request cancelled. Existing keys are unchanged.",
            "签发申请已取消，原 Key 不变。",
        ),
        (
            "tenant_keys.search_member",
            "Search tenant members",
            "搜索租户成员",
        ),
        (
            "tenant_keys.choose_member",
            "Choose an active member, or enter their UUID below",
            "选择有效成员，或在下方输入其 UUID",
        ),
        (
            "tenant_keys.select_workspace",
            "Select a verified tenant workspace to manage your personal keys.",
            "请先选择已验证的租户工作区，再管理个人 Key。",
        ),
        (
            "tenant_keys.owner_hint",
            "Only you can claim your requests. A successful claim returns the secret once; this page never saves it to browser storage.",
            "只有本人能领取自己的申请。领取成功只返回一次明文，本页不会将其写入浏览器存储。",
        ),
        ("tenant_keys.claim", "Claim my key", "领取我的 Key"),
        (
            "tenant_keys.decline",
            "Decline my request",
            "拒绝我的签发申请",
        ),
        (
            "tenant_keys.rotation_hint",
            "Claiming this rotation revokes the original key and creates your replacement in one transaction.",
            "确认领取轮换会在同一事务中撤销原 Key，并为本人创建替代 Key。",
        ),
        (
            "tenant_keys.declined_result",
            "Request declined; no key was created.",
            "已拒绝申请，没有创建 Key。",
        ),
        (
            "tenant_keys.claim_uncertain",
            "Refresh request and key records before another attempt. A lost one-time response cannot be recovered by repeating a claim.",
            "再次操作前请刷新申请和 Key 记录；丢失的一次性响应无法通过重复领取找回。",
        ),
        (
            "tenant_keys.secret_title",
            "Your one-time key",
            "本人一次性 Key 明文",
        ),
        (
            "tenant_keys.secret_hint",
            "Save this key securely now. Closing this value, leaving the page or switching workspace removes it from this view; it cannot be retrieved again.",
            "请立即安全保存。关闭明文、离开页面或切换工作区后，本页将不再显示，无法再次读取。",
        ),
        ("tenant_keys.copy", "Copy my key", "复制本人的 Key"),
        (
            "tenant_keys.clear_secret",
            "Saved securely — hide key",
            "已安全保存，隐藏明文",
        ),
        ("tenant_keys.copied", "Key copied.", "Key 已复制。"),
        (
            "tenant_keys.copy_failed",
            "Clipboard access failed. Select and copy the displayed key manually.",
            "剪贴板写入失败，请手动选择并复制上方明文。",
        ),
    ] {
        m.insert(key, if en { english } else { chinese });
    }
}
