use std::collections::HashMap;
pub(super) fn register(m: &mut HashMap<&'static str, &'static str>, en: bool) {
    for (key, english, chinese) in [
        ("tenant_keys.title", "Tenant API Keys", "工作区 API 密钥"),
        (
            "tenant_keys.hint",
            "Manage tenant key metadata; each owner receives their own secrets.",
            "管理本工作区的密钥信息；明文只能由密钥拥有者领取。",
        ),
        (
            "tenant_keys.metadata_only",
            "Issuance and rotation create a pending request, not a readable key. Existing keys stay unchanged until the owner claims.",
            "签发和轮换只创建待领取申请，不返回明文；拥有者领取前不改变原 API 密钥。",
        ),
        ("tenant_keys.my_requests", "Issuance requests", "签发申请"),
        ("tenant_keys.keys", "Key metadata", "密钥信息"),
        ("tenant_keys.pending", "Pending issuance", "待领取申请"),
        ("tenant_keys.owner", "Key owner ID", "API 密钥拥有者 ID"),
        (
            "tenant_keys.filter",
            "Apply key owner filter",
            "应用 API 密钥拥有者筛选",
        ),
        (
            "tenant_keys.active_filter",
            "Current key owner filter:",
            "当前 API 密钥拥有者筛选：",
        ),
        (
            "tenant_keys.all_owners",
            "All owners in this tenant",
            "本租户所有拥有者",
        ),
        (
            "tenant_keys.include_revoked",
            "Include revoked keys",
            "包含已撤销 API 密钥",
        ),
        (
            "tenant_keys.request",
            "Request a new key",
            "申请签发 API 密钥",
        ),
        ("tenant_keys.edit", "Edit key metadata", "编辑 API 密钥信息"),
        (
            "tenant_keys.rotate",
            "Request key rotation",
            "申请轮换 API 密钥",
        ),
        (
            "tenant_keys.delete",
            "Permanently delete key",
            "永久删除 API 密钥",
        ),
        (
            "tenant_keys.delete_permanent_warning",
            "Confirm permanent deletion. The API key credential and recoverable secret will be removed immediately and cannot be restored. Security audit history is retained, and any pending rotation that replaces this key will be cancelled.",
            "请二次确认永久删除。该 API 密钥的凭证及可恢复明文将立即移除且无法还原；安全审计历史仍会保留，以该密钥为目标的待处理轮换申请也会同时取消。",
        ),
        (
            "tenant_keys.cancel_request",
            "Cancel issuance request",
            "取消签发申请",
        ),
        ("tenant_keys.name", "Key name", "API 密钥名称"),
        ("tenant_keys.state", "Key state", "API 密钥状态"),
        (
            "tenant_keys.expiration",
            "Key expiration",
            "API 密钥到期时间",
        ),
        (
            "tenant_keys.version",
            "Metadata last updated:",
            "元数据最后更新时间：",
        ),
        ("tenant_keys.revoked", "Revoked", "已撤销"),
        ("tenant_keys.expired", "Expired", "已过期"),
        ("tenant_keys.active", "Active", "有效"),
        ("tenant_keys.never", "Never expires", "永不过期"),
        ("tenant_keys.requester", "Requested by:", "申请人："),
        (
            "tenant_keys.replaces",
            "Key to replace:",
            "待替换 API 密钥：",
        ),
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
            "Expiration (UTC)",
            "到期时间（UTC）",
        ),
        (
            "tenant_keys.requested",
            "Request recorded. The owner must claim it before it expires.",
            "申请已登记，需由拥有者在领取截止前确认领取。",
        ),
        (
            "tenant_keys.already_pending",
            "The same rotation request is already pending; no key has been issued again.",
            "相同轮换申请已在等待领取，没有重复签发 API 密钥。",
        ),
        (
            "tenant_keys.deleted_result",
            "API key permanently deleted.",
            "API 密钥已永久删除。",
        ),
        (
            "tenant_keys.cancelled_result",
            "Issuance request cancelled. Existing keys are unchanged.",
            "签发申请已取消，原 API 密钥不变。",
        ),
        (
            "tenant_keys.search_member",
            "Search tenant members",
            "搜索租户成员",
        ),
        (
            "tenant_keys.search_placeholder",
            "Search by member name or email",
            "按成员姓名或邮箱搜索",
        ),
        (
            "tenant_keys.manual_owner",
            "Member ID (advanced)",
            "成员 ID（高级选项）",
        ),
        ("tenant_keys.key_identity", "Key identity", "API 密钥标识"),
        (
            "tenant_keys.request_identity",
            "Request identity",
            "申请标识",
        ),
        (
            "tenant_keys.choose_member",
            "Choose an active member, or enter their member ID below",
            "选择有效成员，或在下方输入其成员 ID",
        ),
        (
            "tenant_keys.selected_member",
            "Selected key owner",
            "已选择的 API 密钥拥有者",
        ),
        (
            "tenant_keys.no_eligible_members",
            "No selectable active member is available in the current results. Try another page, or invite and activate a member.",
            "当前结果中没有可选择的有效成员。请尝试其它分页，或先邀请并启用成员。",
        ),
        (
            "tenant_keys.no_matching_members",
            "No eligible active member matches this search.",
            "没有符合当前搜索条件的可用有效成员。",
        ),
        (
            "tenant_keys.select_workspace",
            "Select a verified tenant workspace to manage your personal keys.",
            "请先选择已验证的工作区，再管理个人 API 密钥。",
        ),
        (
            "tenant_keys.owner_hint",
            "Only you can claim your requests. The resulting key is stored encrypted for your account and is kept only in short-lived page memory while displayed.",
            "只有本人能领取自己的申请。生成的 API 密钥会加密保存到账号，显示期间仅短暂保留在当前页面内存中。",
        ),
        ("tenant_keys.claim", "Claim my key", "领取我的 API 密钥"),
        (
            "tenant_keys.decline",
            "Decline my request",
            "拒绝我的签发申请",
        ),
        (
            "tenant_keys.rotation_hint",
            "Claiming this rotation revokes the original key and creates your replacement in one transaction.",
            "确认领取轮换会在同一事务中撤销原 API 密钥，并为本人创建替代密钥。",
        ),
        (
            "tenant_keys.declined_result",
            "Request declined; no key was created.",
            "已拒绝申请，没有创建 API 密钥。",
        ),
        (
            "tenant_keys.claim_uncertain",
            "Refresh request and key records before another attempt. Do not repeat an uncertain claim; after a confirmed claim, the owner can reveal the key from the key list.",
            "再次操作前请刷新申请和 API 密钥记录；不确定时不要重复领取，确认领取成功后可在密钥列表中由本人显示原文。",
        ),
        (
            "tenant_keys.secret_title",
            "Your API key",
            "本人的 API 密钥",
        ),
        (
            "tenant_keys.secret_hint",
            "This key is hidden by default. Only its owner can retrieve it, and this page keeps a temporary in-memory copy only until you close it or leave.",
            "此 API 密钥默认隐藏且只有本人可以读取；本页面仅在关闭明文区域或离开页面前，于内存中短暂持有副本。",
        ),
        (
            "tenant_keys.reveal_secret",
            "Reveal key",
            "显示 API 密钥原文",
        ),
        ("tenant_keys.hide_secret", "Hide key", "隐藏 API 密钥原文"),
        ("tenant_keys.copy", "Copy my key", "复制本人的 API 密钥"),
        (
            "tenant_keys.clear_secret",
            "Saved securely — hide key",
            "已安全保存，隐藏明文",
        ),
        ("tenant_keys.copied", "Key copied.", "API 密钥已复制。"),
        (
            "tenant_keys.copy_failed",
            "Clipboard access failed. Select and copy the displayed key manually.",
            "剪贴板写入失败，请手动选择并复制上方明文。",
        ),
    ] {
        m.insert(key, if en { english } else { chinese });
    }
}
