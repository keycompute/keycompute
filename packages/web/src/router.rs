use dioxus::prelude::*;

use crate::app::{AdminLayout, AppLayout};
use crate::views::{
    Billing, NotFound, Usage,
    api_keys::ApiKeyList,
    auth::{ForgotPassword, Login, Register, ResetPassword},
    dashboard::Dashboard,
    distribution::DistributionOverview,
    payments::{PaymentsOverview, Recharge},
    shared::{
        Accounts, DistributionRecords, PaymentOrders, Pricing, Settings, System, Tenants, Users,
    },
    user::{UserProfile, UserSettings},
};

#[derive(Debug, Clone, Routable, PartialEq)]
#[rustfmt::skip]
pub enum Route {
    // 认证页面（无 AppShell 布局）
    #[route("/auth/login")]
    Login {},
    #[route("/auth/register")]
    Register {},
    #[route("/auth/forgot-password")]
    ForgotPassword {},
    #[route("/auth/reset-password/:token")]
    ResetPassword { token: String },

    // 主应用（带 AppShell 布局）
    #[layout(AppLayout)]
        #[route("/")]
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

        // Admin 功能页面（额外加一层 AdminLayout 做角色验证）
        #[layout(AdminLayout)]
            #[route("/admin/users")]
            Users {},
            #[route("/admin/accounts")]
            Accounts {},
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
            #[route("/admin/settings")]
            Settings {},
        #[end_layout]
    #[end_layout]

    // 404
    #[route("/:..route")]
    NotFound { route: Vec<String> },
}
