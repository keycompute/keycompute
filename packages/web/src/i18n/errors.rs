pub const TEXT: &[(&str, &str, &str)] = &[
    (
        "error.unauthorized",
        "登录已过期，请重新登录",
        "Your session has expired. Sign in again.",
    ),
    (
        "error.tenant_selection_required",
        "请先选择租户工作区",
        "Select a tenant workspace first.",
    ),
    (
        "error.forbidden",
        "权限不足，无法执行此操作",
        "You do not have permission to perform this action.",
    ),
    (
        "error.not_found",
        "资源不存在或已被删除",
        "This resource does not exist or has been deleted.",
    ),
    (
        "error.rate_limited",
        "请求过于频繁，请稍候再试",
        "Too many requests. Try again shortly.",
    ),
    (
        "error.verification",
        "验证码校验失败，请检查后重试",
        "Verification failed. Check the provided value and try again.",
    ),
    (
        "error.network",
        "网络连接失败，请检查网络设置",
        "The network connection failed. Check your connection and try again.",
    ),
    (
        "error.server",
        "服务器内部错误，请稍候重试",
        "The server encountered an error. Try again shortly.",
    ),
    (
        "error.service_unavailable",
        "服务暂时不可用，请稍候再试",
        "The service is temporarily unavailable. Try again shortly.",
    ),
    (
        "error.invalid_response",
        "数据解析失败，请刷新页面",
        "The response could not be read. Refresh the page and try again.",
    ),
    (
        "error.config",
        "页面配置异常，请刷新后重试",
        "The page configuration is invalid. Refresh and try again.",
    ),
    (
        "error.bad_request",
        "请求参数错误，请检查输入",
        "The request is invalid. Check the input and try again.",
    ),
    (
        "error.conflict",
        "数据冲突，该资源可能已存在",
        "The request conflicts with existing data.",
    ),
    (
        "error.request_failed",
        "请求失败，请稍候重试",
        "The request failed. Try again shortly.",
    ),
];
