use dioxus::prelude::*;

use crate::app::{AdminLayout, AppLayout};
use crate::views::{
    Billing, Home, NotFound, Usage,
    api_keys::ApiKeyList,
    auth::{ForgotPassword, Login, Register, ResetPassword},
    dashboard::Dashboard,
    distribution::DistributionOverview,
    node::{node_earnings::NodeEarnings, node_token::NodeToken},
    operations::PlatformOperations,
    payments::{PaymentsOverview, Recharge},
    shared::{
        Accounts, DistributionRecords, ModelBindings, ModelManagement, ModelManagementBase,
        Monitoring, MonitoringDiagnostics, NodeGateway, PaymentOrders, Pricing, Settings, System,
        Tenants, UpstreamAccounts, UpstreamNodes, UpstreamPassthrough, Users,
    },
    tenant::{
        TenantAdminLayout, TenantAudit, TenantInvitationAccept, TenantInvitations, TenantMembers,
        TenantNodes, TenantPricing, TenantWorkspace,
    },
    user::{UserProfile, UserSettings},
};

#[derive(Debug, Clone, Routable, PartialEq)]
#[rustfmt::skip]
pub enum Route {
    // 首页（无 AppShell 布局，单独处理认证弹窗）
    #[route("/")]
    Home {},

    // 认证页面（无 AppShell 布局）
    #[route("/auth/login")]
    Login {},
    // 必须在路由上声明 query 段，否则 Router 初始化规范化 URL 时会把 ?ref= 参数抹掉
    #[route("/auth/register?:..query")]
    Register { query: RegisterQuery },
    #[route("/auth/forgot-password")]
    ForgotPassword {},
    #[route("/auth/reset-password/:token")]
    ResetPassword { token: String },

    // Invitation token is captured and scrubbed before Router construction.
    #[route("/invite")]
    TenantInvitationAccept {},

    // 主应用（带 AppShell 布局）
    #[layout(AppLayout)]
        #[route("/dashboard")]
        Dashboard {},
        #[route("/api-keys")]
        ApiKeyList {},
        #[route("/usage")]
        Usage {},
        #[route("/billing")]
        Billing {},
        #[route("/payments")]
        PaymentsOverview {},
        #[route("/payments/recharge")]
        Recharge {},
        #[route("/distribution")]
        DistributionOverview {},
        #[route("/user/profile")]
        UserProfile {},
        #[route("/user/settings")]
        UserSettings {},
        #[route("/node/token")]
        NodeToken {},
        #[route("/node/earnings")]
        NodeEarnings {},

        #[route("/platform/operations")]
        PlatformOperations {},

        #[route("/tenant")]
        TenantWorkspace {},
        #[layout(TenantAdminLayout)]
            #[route("/tenant/pricing")]
            TenantPricing {},
            #[route("/tenant/nodes")]
            TenantNodes {},
            #[route("/tenant/members")]
            TenantMembers {},
            #[route("/tenant/invitations")]
            TenantInvitations {},
            #[route("/tenant/audit")]
            TenantAudit {},
        #[end_layout]

        // Admin 功能页面（额外加一层 AdminLayout 做角色验证）
        #[layout(AdminLayout)]
            #[route("/admin/users")]
            Users {},
            #[route("/admin/accounts")]
            Accounts {},
            #[route("/admin/model-bindings")]
            ModelBindings {},
            #[route("/admin/models")]
            ModelManagementBase {},
            #[route("/admin/models/:mode")]
            ModelManagement { mode: String },
            #[route("/admin/upstreams/accounts")]
            UpstreamAccounts {},
            #[route("/admin/upstreams/passthrough")]
            UpstreamPassthrough {},
            #[route("/admin/upstreams/nodes")]
            UpstreamNodes {},
            #[route("/admin/pricing")]
            Pricing {},
            #[route("/admin/payment-orders")]
            PaymentOrders {},
            #[route("/admin/distribution-records")]
            DistributionRecords {},
            #[route("/admin/tenants")]
            Tenants {},
            #[route("/admin/system")]
            System {},
            #[route("/admin/node-gateway")]
            NodeGateway {},
            #[route("/admin/monitoring")]
            Monitoring {},
            #[route("/admin/monitoring/diagnostics")]
            MonitoringDiagnostics {},
            #[route("/admin/settings")]
            Settings {},
        #[end_layout]
    #[end_layout]

    // 404
    #[route("/:..route")]
    NotFound { route: Vec<String> },
}

/// 注册页 query 参数。线上邀请链接格式为 `?ref=<推荐码>`，而 `ref` 是 Rust 关键字，
/// 无法作为命名 query 段的字段声明，因此用 spread query（`?:..query`）手动解析。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RegisterQuery {
    /// 推荐码（来自 `?ref=` 参数，保证非空）
    pub ref_code: Option<String>,
}

impl From<&str> for RegisterQuery {
    fn from(query: &str) -> Self {
        let ref_code = query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == "ref" && !value.is_empty()).then(|| value.to_string())
        });
        Self { ref_code }
    }
}

impl std::fmt::Display for RegisterQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(code) = &self.ref_code {
            write!(f, "ref={code}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn tenant_console_and_secret_free_invitation_routes_are_mounted() {
        for (url, expected) in [
            ("/tenant", Route::TenantWorkspace {}),
            ("/tenant/members", Route::TenantMembers {}),
            ("/tenant/invitations", Route::TenantInvitations {}),
            ("/tenant/audit", Route::TenantAudit {}),
            ("/invite", Route::TenantInvitationAccept {}),
        ] {
            let route = Route::from_str(url).unwrap();
            assert_eq!(route, expected);
            assert_eq!(route.to_string(), url);
        }
    }

    /// 邀请链接的 ?ref= 参数必须能被解析，且序列化回 URL 时不丢失
    /// （Router 初始化时会用 to_string 规范化地址栏，丢失即意味着推荐码被抹掉）
    #[test]
    fn register_route_preserves_ref_query() {
        let url = "/auth/register?ref=15804e83-f088-44f6-8df0-982dcf8182a1";
        let route = Route::from_str(url).expect("should parse invite link");
        assert_eq!(
            route,
            Route::Register {
                query: RegisterQuery {
                    ref_code: Some("15804e83-f088-44f6-8df0-982dcf8182a1".to_string()),
                },
            }
        );
        assert_eq!(route.to_string(), url);
    }

    #[test]
    fn register_query_ignores_empty_and_unknown_keys() {
        assert_eq!(RegisterQuery::from("ref="), RegisterQuery::default());
        assert_eq!(RegisterQuery::from("foo=1"), RegisterQuery::default());
        assert_eq!(
            RegisterQuery::from("foo=1&ref=abc"),
            RegisterQuery {
                ref_code: Some("abc".to_string()),
            }
        );
    }

    /// dioxus 对 spread query 变体序列化时总会追加 '?'，query 为空时 URL
    /// 为 `/auth/register?`。浏览器视其与无 query 等价且页面会立即重定向，
    /// 此为有意接受的已知行为，用测试固定下来防止升级时静默变化
    #[test]
    fn register_without_ref_produces_trailing_question_mark() {
        let route = Route::Register {
            query: RegisterQuery::default(),
        };
        assert_eq!(route.to_string(), "/auth/register?");
    }

    #[test]
    fn monitoring_diagnostics_has_a_stable_nested_route() {
        let url = "/admin/monitoring/diagnostics";
        let route = Route::from_str(url).expect("diagnostics route should parse");
        assert_eq!(route, Route::MonitoringDiagnostics {});
        assert_eq!(route.to_string(), url);
    }

    #[test]
    fn legacy_system_route_remains_parseable_for_compatibility() {
        let url = "/admin/system";
        let route = Route::from_str(url).expect("legacy system route should parse");
        assert_eq!(route, Route::System {});
        assert_eq!(route.to_string(), url);
    }

    #[test]
    fn upstream_tabs_have_canonical_route_backed_pages() {
        for (url, expected) in [
            ("/admin/upstreams/accounts", Route::UpstreamAccounts {}),
            (
                "/admin/upstreams/passthrough",
                Route::UpstreamPassthrough {},
            ),
            ("/admin/upstreams/nodes", Route::UpstreamNodes {}),
        ] {
            let route = Route::from_str(url).expect("upstream route should parse");
            assert_eq!(route, expected);
            assert_eq!(route.to_string(), url);
        }
    }

    #[test]
    fn nginx_node_api_rule_does_not_capture_console_routes() {
        let nginx = include_str!("../../../nginx/nginx.conf");
        assert!(nginx.contains("location /node/v1/ {"));
        assert!(!nginx.contains("location /node/ {"));
    }

    #[test]
    fn nginx_responses_rule_streams_admitted_inline_skill_payloads() {
        let nginx = include_str!("../../../nginx/nginx.conf");
        let responses_location = nginx
            .split_once("location ^~ /v1/responses {")
            .expect("Responses location must exist")
            .1
            .split_once("\n        }")
            .expect("Responses location must be closed")
            .0;
        assert!(responses_location.contains("client_max_body_size 80m;"));
        assert!(responses_location.contains("proxy_request_buffering off;"));
    }

    #[test]
    fn nginx_passthrough_routes_preserve_uri_and_do_not_fall_back_to_spa() {
        let nginx = include_str!("../../../nginx/nginx.conf");
        let chat = nginx
            .split_once("location = /pt/v1/chat/completions {")
            .expect("model-bound chat location must exist")
            .1
            .split_once("\n        }")
            .expect("model-bound chat location must be closed")
            .0;
        assert!(chat.contains("proxy_pass         http://keycompute_backend;"));
        assert!(chat.contains("client_max_body_size 96m;"));
        assert!(chat.contains("proxy_request_buffering off;"));
        assert!(chat.contains("proxy_buffering    off;"));
        assert!(nginx.contains("location = /pt/v1/models {"));
        assert!(nginx.contains("location ^~ /pt/v1/models/ {"));
        assert!(!nginx.contains("location /pt/ {"));
    }

    #[test]
    fn nginx_node_dispatch_routes_preserve_uri_and_proxy_unknown_nt_paths() {
        let nginx = include_str!("../../../nginx/nginx.conf");
        let chat = nginx
            .split_once("location = /nt/v1/chat/completions {")
            .expect("NodeDispatch chat location must exist")
            .1
            .split_once("\n        }")
            .expect("NodeDispatch chat location must be closed")
            .0;
        assert!(chat.contains("proxy_pass         http://keycompute_backend;"));
        assert!(chat.contains("client_max_body_size 96m;"));
        assert!(chat.contains("proxy_request_buffering off;"));
        assert!(chat.contains("proxy_buffering    off;"));
        assert!(nginx.contains("location = /nt/v1/models {"));
        assert!(nginx.contains("location ^~ /nt/v1/models/ {"));
        assert!(nginx.contains("location ^~ /nt/v1/ {"));
        assert!(!nginx.contains("location /nt/ {"));
        assert!(!nginx.contains("location /nt/v1/ {"));
    }

    #[test]
    fn dioxus_dev_proxy_preserves_the_nt_family_prefix() {
        let dioxus = include_str!("../Dioxus.toml");
        assert!(dioxus.contains("backend = \"http://127.0.0.1:3000/nt/\""));
    }
}
