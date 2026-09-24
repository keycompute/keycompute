use std::collections::HashMap;
pub(super) fn register(zh: &mut HashMap<&'static str, &'static str>, en: bool) {
    zh.insert(
        "tenant_responses.item_id",
        if en { "Item ID" } else { "条目 ID" },
    );
    zh.insert(
        "tenant_responses.title",
        if en {
            "Responses and conversations"
        } else {
            "Responses 与会话"
        },
    );
    zh.insert(
        "tenant_responses.hint",
        if en {
            "Manage hosted resources by current tenant, original owner and execution family."
        } else {
            "按当前租户、原拥有者和执行模式管理托管资源。"
        },
    );
    zh.insert("tenant_responses.local_only", if en {"This page manages local Passthrough/Node resources only. Native account-pool management is not available here."} else {"本页只管理透传与节点模式的本地托管资源；账号池原生资源尚未接入。"});
    zh.insert(
        "tenant_responses.responses",
        if en {
            "Response records"
        } else {
            "Responses 记录"
        },
    );
    zh.insert(
        "tenant_responses.conversations",
        if en {
            "Conversations"
        } else {
            "Conversation 会话"
        },
    );
    zh.insert(
        "tenant_responses.mode",
        if en {
            "Execution family"
        } else {
            "执行模式"
        },
    );
    zh.insert(
        "tenant_responses.owner",
        if en {
            "Original owner UUID"
        } else {
            "原拥有者 UUID"
        },
    );
    zh.insert(
        "tenant_responses.filter",
        if en {
            "Apply owner filter"
        } else {
            "应用拥有者过滤"
        },
    );
    zh.insert(
        "tenant_responses.resource",
        if en {
            "Resource and model"
        } else {
            "资源与模型"
        },
    );
    zh.insert(
        "tenant_responses.state",
        if en { "Resource state" } else { "资源状态" },
    );
    zh.insert(
        "tenant_responses.created",
        if en { "Created" } else { "创建时间" },
    );
    zh.insert(
        "tenant_responses.revision",
        if en {
            "Observed revision"
        } else {
            "观察到的版本"
        },
    );
    zh.insert(
        "tenant_responses.active",
        if en {
            "Active response"
        } else {
            "活动 Response"
        },
    );
    zh.insert(
        "tenant_responses.expires",
        if en { "Expires" } else { "过期时间" },
    );
    zh.insert(
        "tenant_responses.inspect",
        if en {
            "Inspect resource content"
        } else {
            "查看资源内容"
        },
    );
    zh.insert(
        "tenant_responses.items",
        if en { "Inspect items" } else { "查看条目" },
    );
    zh.insert(
        "tenant_responses.cancel",
        if en {
            "Request response cancellation"
        } else {
            "请求取消 Response"
        },
    );
    zh.insert(
        "tenant_responses.delete",
        if en {
            "Delete resource"
        } else {
            "删除资源"
        },
    );
    zh.insert(
        "tenant_responses.metadata",
        if en {
            "Edit conversation metadata"
        } else {
            "编辑会话元数据"
        },
    );
    zh.insert(
        "tenant_responses.append",
        if en {
            "Append conversation items"
        } else {
            "追加会话条目"
        },
    );
    zh.insert(
        "tenant_responses.remove_item",
        if en {
            "Remove conversation item"
        } else {
            "删除会话条目"
        },
    );
    zh.insert(
        "tenant_responses.json",
        if en { "JSON content" } else { "JSON 内容" },
    );
    zh.insert(
        "tenant_responses.metadata_hint",
        if en {
            "At most 16 string pairs; keys up to 64 characters and values up to 512 characters."
        } else {
            "最多 16 个字符串键值对，键不超过 64 字符、值不超过 512 字符。"
        },
    );
    zh.insert("tenant_responses.items_hint", if en {"Use a JSON array of 1–512 item objects up to 2 MiB; the server also checks the resulting history limit."} else {"提交包含 1–512 个条目对象的 JSON 数组，最多 2 MiB；服务器还会验证总历史上限。"});
    zh.insert("tenant_responses.owner_preserved", if en {"Original tenant, owner and billing identity are preserved. Cancellation does not prove a worker stopped; deletion does not erase existing billing evidence."} else {"操作保留原租户、原拥有者和账务主体。取消状态不代表 worker 已停止；删除不会清除已存在的计费证据。"});
    zh.insert("tenant_responses.private_hint", if en {"Content is read through backend authorization and audit, held only in this page and discarded on close or workspace change."} else {"内容经后端授权及审计读取，仅留在当前页面；关闭或切换工作区将丢弃。"});
    zh.insert(
        "tenant_responses.saved",
        if en {
            "Resource operation returned; refreshing the original tenant records."
        } else {
            "资源操作已返回，正在刷新原租户记录。"
        },
    );
    zh.insert(
        "tenant_responses.active_owner",
        if en {
            "Current owner filter:"
        } else {
            "当前过滤："
        },
    );
    zh.insert(
        "tenant_responses.all_owners",
        if en {
            "All owners in this tenant"
        } else {
            "本租户的所有拥有者"
        },
    );
}
