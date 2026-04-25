use std::collections::HashMap;
use std::sync::LazyLock;

pub static EN: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::new();

    // ── Navigation ──────────────────────────────
    m.insert("nav.home", "Home");
    m.insert("nav.usage", "Usage");
    m.insert("nav.billing", "Billing");
    m.insert("nav.api_keys", "API Keys");
    m.insert("nav.payments", "Payments");
    m.insert("nav.payments.balance", "Balance");
    m.insert("nav.payments.orders", "Orders");
    m.insert("nav.payments.recharge", "Recharge");
    m.insert("nav.distribution", "Distribution");
    m.insert("nav.distribution.earnings", "Earnings");
    m.insert("nav.distribution.referrals", "Referrals");
    m.insert("nav.distribution.invite", "Invite");
    m.insert("nav.user", "Profile");
    m.insert("nav.user.profile", "My Profile");
    m.insert("nav.user.security", "Security");
    m.insert("nav.users", "Users");
    m.insert("nav.accounts", "Accounts");
    m.insert("nav.pricing", "Pricing");
    m.insert("nav.payment_orders", "Payment Orders");
    m.insert("nav.distribution_records", "Distribution Records");
    m.insert("nav.tenants", "Tenants");
    m.insert("nav.system", "System");
    m.insert("nav.account_settings", "Account Settings");
    m.insert("nav.settings", "Settings");
    m.insert("nav.group.usage", "Usage");
    m.insert("nav.group.billing", "Billing");
    m.insert("nav.group.account", "Account");
    m.insert("nav.group.admin", "Admin");

    // ── Auth ────────────────────────────────────
    m.insert("auth.login", "Sign In");
    m.insert("auth.register", "Sign Up");
    m.insert("auth.logout", "Sign Out");
    m.insert("auth.forgot_password", "Forgot Password");
    m.insert("auth.reset_password", "Reset Password");
    m.insert("auth.email", "Email");
    m.insert("auth.username", "Username");
    m.insert("auth.password", "Password");
    m.insert("auth.confirm_password", "Confirm Password");
    m.insert("auth.name", "Name");
    m.insert("auth.remember_me", "Remember me");
    m.insert("auth.no_account", "Don't have an account?");
    m.insert("auth.has_account", "Already have an account?");
    m.insert("auth.send_reset_email", "Send Reset Email");
    m.insert("auth.back_to_login", "Back to Sign In");
    m.insert("auth.login_subtitle", "Sign in to your account to continue");
    m.insert("auth.register_subtitle", "Create your account");
    m.insert(
        "auth.reset_subtitle",
        "Enter your username and email, and we'll send a reset link after verification",
    );
    m.insert(
        "auth.reset_sent",
        "If the username and email match, a reset link has been sent to the corresponding email",
    );
    m.insert("auth.register_now", "Sign up");
    m.insert("auth.login_now", "Sign in");
    m.insert("auth.email_placeholder", "Enter your email");
    m.insert("auth.password_placeholder", "Enter your password");
    m.insert("auth.username_placeholder", "Enter your username");
    m.insert("auth.name_placeholder", "Enter your name");
    m.insert("auth.confirm_password_placeholder", "Re-enter password");
    m.insert(
        "auth.reset_email_placeholder",
        "Enter your registered email",
    );
    m.insert(
        "auth.reset_username_placeholder",
        "Enter your registered username",
    );
    m.insert("auth.fill_all", "Please fill in email and password");
    m.insert("auth.fill_required", "Please fill in all required fields");
    m.insert("auth.enter_email", "Please enter your email address");
    m.insert("auth.enter_username", "Please enter your username");
    m.insert("auth.login_failed", "Login failed");
    m.insert("auth.register_failed", "Registration failed");
    m.insert("auth.send_failed", "Failed to send");
    m.insert("auth.sending", "Sending...");
    m.insert("auth.cooldown_retry", "Try again later");
    m.insert("auth.send_reset_link", "Send Reset Link");
    m.insert("auth.logging_in", "Signing in...");
    m.insert("auth.registering", "Signing up...");
    m.insert("auth.request_code", "Get Code");
    m.insert("auth.requesting_code", "Sending code...");
    m.insert("auth.request_code_failed", "Failed to get code");
    m.insert("auth.resend_code", "Resend Code");
    m.insert("auth.complete_registration", "Complete Registration");
    m.insert("auth.verification_code", "Email Code");
    m.insert(
        "auth.verification_code_placeholder",
        "Enter the 6-digit code",
    );
    m.insert("auth.code_required", "Please enter the email code");
    m.insert("auth.code_sent_to", "Code sent to:");
    m.insert(
        "auth.code_sent_hint",
        "The code is valid for 10 minutes. Your account will be created only after verification succeeds.",
    );
    m.insert("auth.change_email", "Change Email");
    m.insert(
        "auth.registration_success",
        "Registration is complete. You can now sign in with your email and password.",
    );
    m.insert(
        "auth.password_min8",
        "Password must be at least 8 characters",
    );

    // ── Page Titles ──────────────────────────────
    m.insert("page.home", "Dashboard");
    m.insert("page.usage", "Usage");
    m.insert("page.billing", "Billing");
    m.insert("page.api_keys", "API Keys");
    m.insert("page.payments", "Payments");
    m.insert("page.distribution", "Distribution");
    m.insert("page.profile", "Profile");
    m.insert("page.security", "Security");
    m.insert("page.users", "User Management");
    m.insert("page.accounts", "Account Management");
    m.insert("page.pricing", "Pricing");
    m.insert("page.payment_orders", "Payment Orders");
    m.insert("page.distribution_records", "Distribution Records");
    m.insert("page.tenants", "Tenants");
    m.insert("page.system", "System Diagnostics");
    m.insert("page.account_settings", "Account Settings");
    m.insert("page.settings", "Settings");
    m.insert("page.not_found", "Page Not Found");

    // ── Form ────────────────────────────────────
    m.insert("form.save", "Save");
    m.insert("form.cancel", "Cancel");
    m.insert("form.confirm", "Confirm");
    m.insert("form.delete", "Delete");
    m.insert("form.create", "Create");
    m.insert("form.edit", "Edit");
    m.insert("form.search", "Search");
    m.insert("form.reset", "Reset");
    m.insert("form.submit", "Submit");
    m.insert("form.saving", "Saving...");
    m.insert("form.save_changes", "Save Changes");
    m.insert("form.required", "This field is required");
    m.insert("form.invalid_email", "Please enter a valid email");
    m.insert(
        "form.password_too_short",
        "Password must be at least 8 characters",
    );
    m.insert("form.password_mismatch", "Passwords do not match");

    // ── Table ───────────────────────────────────
    m.insert("table.no_data", "No data");
    m.insert("table.loading", "Loading...");
    m.insert("table.actions", "Actions");
    m.insert("table.status", "Status");
    m.insert("table.created_at", "Created At");
    m.insert("table.name", "Name");
    m.insert("table.email", "Email");
    m.insert("table.role", "Role");

    // ── Common ──────────────────────────────────
    m.insert("common.loading", "Loading");
    m.insert("common.error", "Something went wrong");
    m.insert("common.success", "Success");
    m.insert(
        "common.confirm_delete",
        "Are you sure? This action cannot be undone.",
    );
    m.insert("common.copied", "Copied to clipboard");
    m.insert("common.copy", "Copy");
    m.insert("common.refresh", "Refresh");
    m.insert("common.back", "Back");
    m.insert("common.time", "Time");
    m.insert("common.total_items", "Total");
    m.insert("common.created_at_label", "Created");
    m.insert("common.load_failed", "Load failed");
    m.insert("common.redirecting", "Redirecting");
    m.insert("common.redirect_to_login", "Redirecting to sign in...");
    m.insert(
        "common.admin_only_page",
        "Permission denied: this page is available to admins only",
    );
    m.insert("common.expand", "Expand");
    m.insert("common.collapse", "Collapse");
    m.insert("common.enabled", "Enabled");
    m.insert("common.disabled", "Disabled");
    m.insert("common.yes", "Yes");
    m.insert("common.no", "No");
    m.insert("common.admin", "Admin");
    m.insert("common.user", "User");
    m.insert(
        "common.no_permission",
        "You don't have permission to view this page",
    );
    m.insert("common.balance", "Balance");
    m.insert("common.amount", "Amount");
    m.insert("common.currency", "Currency");
    m.insert("common.tokens", "Tokens");
    m.insert("common.requests", "Requests");
    m.insert("common.cost", "Cost");
    m.insert("dashboard.greeting", "Hello");
    m.insert("dashboard.subtitle", "Here is your console overview");
    m.insert("dashboard.api_calls", "API Calls");
    m.insert("dashboard.weekly_total", "This Week");
    m.insert("dashboard.balance", "Account Balance");
    m.insert("dashboard.available", "Available");
    m.insert("dashboard.active_keys", "Active Keys");
    m.insert("dashboard.total", "Total");
    m.insert("dashboard.weekly_cost", "Weekly Cost");
    m.insert("dashboard.used", "Used");
    m.insert("dashboard.quick_links", "Quick Links");
    m.insert("dashboard.manage_api_keys", "Manage API Keys");
    m.insert("dashboard.recharge", "Recharge Balance");
    m.insert("dashboard.account_settings", "Account Settings");
    m.insert(
        "api_keys.subtitle",
        "Manage OpenAI-compatible access keys. Full key values are shown only once after creation.",
    );
    m.insert("api_keys.create", "Create API Key");
    m.insert("api_keys.active", "Active");
    m.insert("api_keys.all_with_revoked", "All, including revoked");
    m.insert("api_keys.created_title", "API Key created");
    m.insert("api_keys.created_once", "Shown only once. Save it now.");
    m.insert("api_keys.example", "Usage example");
    m.insert("api_keys.copy_hint", "Click to copy");
    m.insert("api_keys.copied", "Copied");
    m.insert(
        "api_keys.example_note",
        "Use this configuration with OpenAI-compatible SDKs or tools.",
    );
    m.insert("api_keys.close_saved", "Saved, close");
    m.insert("api_keys.create_title", "Create API Key");
    m.insert("api_keys.name", "Name");
    m.insert("api_keys.name_placeholder", "Name this key");
    m.insert("api_keys.creating", "Creating...");
    m.insert("api_keys.create_failed", "Create failed");
    m.insert("api_keys.loading_failed", "Load failed");
    m.insert("api_keys.registry", "API Key Management");
    m.insert("api_keys.empty_meta", "No keys match the current filter.");
    m.insert(
        "api_keys.active_meta",
        "Showing active keys available for gateway requests.",
    );
    m.insert(
        "api_keys.all_meta",
        "Showing all keys, including revoked records.",
    );
    m.insert(
        "api_keys.empty",
        "No API Keys yet. Create one using the button above.",
    );
    m.insert("api_keys.prefix", "Prefix");
    m.insert("api_keys.revoked", "Revoked");

    // ── Layout ──────────────────────────────────
    m.insert("layout.toggle_sidebar", "Toggle sidebar");
    m.insert("layout.open_menu", "Open menu");
    m.insert("layout.switch_to_light", "Switch to light theme");
    m.insert("layout.switch_to_dark", "Switch to dark theme");
    m.insert("layout.switch_to_zh", "Switch to Chinese");
    m.insert("layout.switch_to_en", "Switch to English");
    m.insert("layout.expand_sidebar", "Expand sidebar");
    m.insert("layout.collapse_sidebar", "Collapse sidebar");

    // ── Error ───────────────────────────────────
    m.insert(
        "error.not_found_desc",
        "The page you requested does not exist or has been removed",
    );
    m.insert("error.back_home", "Back to Dashboard");

    // ── Login ───────────────────────────────────
    m.insert("login.tagline_1", "Give");
    m.insert("login.tagline_highlight", "AI Apps");
    m.insert("login.tagline_2", "a");
    m.insert("login.tagline_3", "high-performance compute gateway");
    m.insert(
        "login.description",
        "Unified LLM access, intelligent routing, real-time billing, and end-to-end observability. An enterprise-ready AI token management platform out of the box.",
    );
    m.insert("login.feature_routing", "Smart Routing");
    m.insert("login.feature_billing", "Real-time Billing");
    m.insert("login.feature_ha", "High Availability");
    m.insert("login.feature_api", "API Management");
    m.insert("login.title", "Sign in to your account");
    m.insert(
        "login.subtitle",
        "Manage your AI tokens and compute resources",
    );
    m.insert("login.email_label", "Email Address");
    m.insert("login.hide_password", "Hide password");
    m.insert("login.show_password", "Show password");
    m.insert("login.verifying", "Verifying...");
    m.insert("login.submit", "Sign in to Console");
    m.insert("reset_password.failed", "Reset failed");
    m.insert("reset_password.success", "Password reset successfully!");
    m.insert("reset_password.go_login", "Go to Sign In");
    m.insert("reset_password.submit", "Confirm Reset");

    // ── Account Settings ────────────────────────
    m.insert(
        "account_settings.fill_all_passwords",
        "Please fill in all password fields",
    );
    m.insert(
        "account_settings.password_mismatch",
        "The new passwords do not match",
    );
    m.insert(
        "account_settings.password_too_short",
        "The new password must be at least 8 characters",
    );
    m.insert(
        "account_settings.password_changed",
        "Password changed successfully",
    );
    m.insert("account_settings.change_failed", "Change failed");
    m.insert("account_settings.change_password", "Change Password");
    m.insert(
        "account_settings.section_desc",
        "This page focuses on account security actions. The form stays narrow to avoid stretching across wide screens.",
    );
    m.insert("account_settings.current_password", "Current Password");
    m.insert(
        "account_settings.current_password_desc",
        "Used to confirm this action comes from the currently signed-in account.",
    );
    m.insert(
        "account_settings.current_password_placeholder",
        "Enter your current password",
    );
    m.insert("account_settings.new_password", "New Password");
    m.insert(
        "account_settings.new_password_desc",
        "Use a stronger password with more characters, mixed case, and symbols when possible.",
    );
    m.insert(
        "account_settings.new_password_placeholder",
        "Enter a new password (at least 8 characters)",
    );
    m.insert("account_settings.confirm_password", "Confirm New Password");
    m.insert(
        "account_settings.confirm_password_desc",
        "Re-enter the same password to avoid mistakes that could lock you out.",
    );
    m.insert(
        "account_settings.confirm_password_placeholder",
        "Re-enter the new password",
    );

    // ── Profile ─────────────────────────────────
    m.insert("profile.saved", "Saved successfully");
    m.insert("profile.save_failed", "Save failed");
    m.insert(
        "profile.page_desc",
        "View your current account identity information and maintain the display name shown in the console.",
    );
    m.insert("profile.tenant", "Tenant");
    m.insert("profile.user_id", "User ID");
    m.insert("profile.edit", "Edit Profile");

    // ── Usage ───────────────────────────────────
    m.insert(
        "usage.subtitle",
        "Review API call history and token consumption",
    );
    m.insert("usage.calls", "Calls");
    m.insert("usage.total_calls", "Total Calls");
    m.insert("usage.period", "Period");
    m.insert("usage.total_tokens", "Total Tokens");
    m.insert("usage.prompt_tokens", "Prompt Tokens");
    m.insert("usage.completion_tokens", "Completion Tokens");
    m.insert("usage.total_cost", "Total Cost");
    m.insert("usage.usage_billed", "Usage-based billing");
    m.insert("usage.trend", "Call Trend");
    m.insert("usage.records", "Call Records");
    m.insert("usage.no_records", "No records yet");
    m.insert("usage.model", "Model");
    m.insert("usage.total_token", "Total Tokens");

    // ── Payments ────────────────────────────────
    m.insert("payments.title", "Payments and Billing");
    m.insert(
        "payments.subtitle",
        "Review account balance, recharge records, and billing details",
    );
    m.insert("payments.recharge_now", "Recharge Now");
    m.insert("payments.account_balance", "Account Balance");
    m.insert("payments.frozen_amount", "Frozen Amount");
    m.insert("payments.total_recharge", "Total Recharged");
    m.insert("payments.total_consumed", "Total Consumed");
    m.insert("payments.usage_requests", "Usage Requests");
    m.insert("payments.input_tokens", "Input Tokens");
    m.insert("payments.output_tokens", "Output Tokens");
    m.insert("payments.total_tokens", "Total Tokens");
    m.insert("payments.total_cost", "Total Cost");
    m.insert("payments.recharge_records", "Recharge Records");
    m.insert("payments.no_recharge_records", "No recharge records yet");
    m.insert("payments.order_no", "Order No.");
    m.insert("payments.subject", "Subject");
    m.insert("payments.usage_details", "Usage Details");
    m.insert("payments.no_usage_records", "No usage records yet");

    // ── Distribution ────────────────────────────
    m.insert("distribution.title", "Distribution");
    m.insert(
        "distribution.subtitle",
        "Review your distribution earnings and referral records",
    );
    m.insert("distribution.fetch_failed", "Fetch failed");
    m.insert("distribution.disabled_title", "Distribution is disabled");
    m.insert(
        "distribution.disabled_desc",
        "Distribution has not been enabled for this system yet, so earnings, referral code, and referral user data are currently unavailable.",
    );
    m.insert("distribution.total_earnings", "Total Earnings");
    m.insert("distribution.available_balance", "Available Balance");
    m.insert("distribution.pending", "Pending Settlement");
    m.insert("distribution.referral_count", "Referral Count");
    m.insert("distribution.my_referral_code", "My Referral Code");
    m.insert("distribution.referral_code", "Referral Code");
    m.insert("distribution.invite_link", "Invite Link");
    m.insert("distribution.referral_users", "Referred Users");
    m.insert("distribution.user", "User");
    m.insert("distribution.joined_at", "Joined At");
    m.insert("distribution.total_spent", "Total Spent");
    m.insert("distribution.my_earnings", "My Earnings");
    m.insert("distribution.no_referrals", "No referral records yet");
    m.insert(
        "distribution.disabled_message",
        "Distribution is currently disabled",
    );

    // ── Settings ────────────────────────────────
    m.insert(
        "settings.admin_desc",
        "Manage platform parameters through a compact, reviewable console configuration layout.",
    );
    m.insert(
        "settings.user_desc",
        "View current system configuration. Only admins can update global parameters.",
    );
    m.insert("settings.admin_only_hint", "Only admins can modify system settings. Personal language and theme preferences can be changed from the top-right navigation controls.");
    m.insert("settings.load_failed", "Failed to load settings");
    m.insert("settings.saved", "Settings saved");
    m.insert("settings.basic_title", "Basic Configuration");
    m.insert("settings.basic_desc", "Define the platform name, default new-user credit, and recharge baseline settings. The form stays intentionally narrow on wide screens.");
    m.insert("settings.site_name_label", "Platform Name");
    m.insert(
        "settings.site_name_desc",
        "Shown in the sign-in page, admin navigation, and email templates.",
    );
    m.insert(
        "settings.default_user_quota_label",
        "Default New User Credit",
    );
    m.insert("settings.default_user_quota_desc", "Controls the runtime signup credit for new users. Credit is only granted when the value is greater than 0; 0 or negative values disable the gift.");
    m.insert("settings.default_currency_label", "Default Currency");
    m.insert(
        "settings.default_currency_desc",
        "Affects amount display in the console, default order currency, and some frontend labels.",
    );
    m.insert("settings.min_recharge_label", "Minimum Recharge Amount");
    m.insert(
        "settings.min_recharge_desc",
        "Prevents extremely small recharge orders from entering the payment flow.",
    );
    m.insert("settings.security_title", "Security Configuration");
    m.insert("settings.security_desc", "Control token lifetime and related security parameters. New user registration always requires email code verification.");
    m.insert("settings.jwt_expire_label", "JWT Token Expiry (Hours)");
    m.insert("settings.jwt_expire_desc", "Default lifetime for access tokens after sign-in. Longer expiry improves convenience but increases exposure risk.");
    m.insert("settings.save_failed", "Failed to save");
    m.insert("settings.non_negative", "Value cannot be negative");
    m.insert("settings.invalid_number", "Please enter a valid number");
    m.insert("settings.distribution_title", "Distribution Switch");
    m.insert(
        "settings.distribution_desc",
        "Distribution now uses a single global switch managed only by the system role.",
    );
    m.insert("settings.distribution_enabled_label", "Enable Distribution");
    m.insert(
        "settings.distribution_enabled_desc",
        "When enabled, users can access the distribution center and referral APIs. When disabled, those endpoints return a disabled state.",
    );
    m.insert(
        "settings.distribution_enabled_system_only_desc",
        "Current status is read-only here. Only the system role can change the distribution switch in the admin console.",
    );

    // ── Pricing ─────────────────────────────────
    m.insert(
        "pricing.admin_desc",
        "Manage platform pricing policies and model call rates",
    );
    m.insert(
        "pricing.user_desc",
        "View pricing policies currently available on the platform",
    );
    m.insert("pricing.create", "+ Create Pricing");
    m.insert("pricing.empty", "No pricing policies yet");
    m.insert("pricing.table_title", "Model Pricing Table");
    m.insert("pricing.table_subtitle", "Review provider ownership, input/output rates, and default strategies for each model in a single place.");
    m.insert("pricing.items_suffix", "items");
    m.insert("pricing.model_provider", "Model / Provider");
    m.insert("pricing.input_price", "Input Price");
    m.insert("pricing.output_price", "Output Price");
    m.insert("pricing.billing_status", "Billing Status");
    m.insert("pricing.input_tokens", "input tokens");
    m.insert("pricing.output_tokens", "output tokens");
    m.insert("pricing.default", "Default");
    m.insert("pricing.alternative", "Alternative");
    m.insert(
        "pricing.default_note",
        "This rule is currently used as the default billing record for the model",
    );
    m.insert(
        "pricing.alternative_note",
        "Not set as default. It only takes effect after a manual switch",
    );
    m.insert("pricing.set_default_ok", "Set as default pricing");
    m.insert(
        "pricing.set_default_failed",
        "Failed to set default pricing",
    );
    m.insert("pricing.set_default", "Set Default");
    m.insert("pricing.deleted", "Pricing deleted");
    m.insert("pricing.delete_failed", "Failed to delete");
    m.insert("pricing.created", "Pricing created successfully");
    m.insert("pricing.updated", "Pricing updated successfully");
    m.insert("pricing.fill_all", "Please fill in all fields");
    m.insert("pricing.invalid_input_price", "Invalid input price format");
    m.insert(
        "pricing.invalid_output_price",
        "Invalid output price format",
    );
    m.insert(
        "pricing.negative_input_price",
        "Input price cannot be negative",
    );
    m.insert(
        "pricing.negative_output_price",
        "Output price cannot be negative",
    );
    m.insert("pricing.create_failed", "Create failed");
    m.insert("pricing.update_failed", "Update failed");
    m.insert("pricing.create_title", "Create Pricing");
    m.insert("pricing.edit_title", "Edit Pricing");
    m.insert("pricing.model_name", "Model Name");
    m.insert("pricing.model_placeholder", "e.g. gpt-4o");
    m.insert("pricing.input_price_label", "Input Price (per 1K tokens)");
    m.insert("pricing.output_price_label", "Output Price (per 1K tokens)");
    m.insert("pricing.input_placeholder", "e.g. 0.000005");
    m.insert("pricing.output_placeholder", "e.g. 0.000015");
    m.insert("pricing.currency_cny", "CNY (Chinese Yuan)");
    m.insert("pricing.currency_usd", "USD (US Dollar)");
    m.insert("pricing.creating", "Creating...");

    // ── Dashboard ───────────────────────────────
    m.insert(
        "dashboard.subtitle_long",
        "This is your console overview, including live metrics, recent activity, and key actions for the current account.",
    );
    m.insert("dashboard.balance_available", "Available Balance");
    m.insert("dashboard.total_cost", "Total Cost");
    m.insert("dashboard.meta_usage", "Aggregated from real usage data");
    m.insert(
        "dashboard.meta_balance",
        "Returned from live account balance",
    );
    m.insert("dashboard.meta_keys", "Currently enabled keys");
    m.insert("dashboard.meta_cost", "Aggregated from real usage_logs");
    m.insert("dashboard.recent_active_days", "Recent 7 Active Days");
    m.insert(
        "dashboard.recent_active_days_desc",
        "Aggregated from real request records to quickly gauge recent activity changes.",
    );
    m.insert("dashboard.live_data", "Live Data");
    m.insert(
        "dashboard.quick_links_desc",
        "Organized around recharge, keys, and account operations.",
    );
    m.insert(
        "dashboard.manage_api_keys_desc",
        "Create, review, and revoke access keys",
    );
    m.insert("dashboard.payments", "Payments and Billing");
    m.insert(
        "dashboard.payments_desc",
        "Review balance, recharge records, and order status",
    );
    m.insert("dashboard.usage_details", "Usage Details");
    m.insert(
        "dashboard.usage_details_desc",
        "Review model calls, tokens, and cost",
    );
    m.insert(
        "dashboard.account_settings_desc",
        "Update profile and security information",
    );
    m.insert("dashboard.recent_calls", "Recent Calls");
    m.insert(
        "dashboard.recent_calls_desc",
        "Uses real usage records as the console activity stream.",
    );
    m.insert("dashboard.no_recent_calls", "No recent call records.");
    m.insert("dashboard.active_keys_panel", "Active Keys");
    m.insert(
        "dashboard.active_keys_panel_desc",
        "Only keys that are still enabled are shown.",
    );
    m.insert("dashboard.no_active_keys", "No active keys.");
    m.insert("dashboard.system_status", "System Status");
    m.insert("dashboard.account_status", "Account Status");
    m.insert(
        "dashboard.system_status_desc",
        "Gateway and provider health summary visible to admins.",
    );
    m.insert(
        "dashboard.account_status_desc",
        "Summarizes the current account through balance, distribution, and order status.",
    );
    m.insert("dashboard.online", "Online");
    m.insert("dashboard.pending_check", "Pending Check");
    m.insert("dashboard.gateway_providers", "Gateway Providers");
    m.insert(
        "dashboard.gateway_providers_desc",
        "Number of loaded providers",
    );
    m.insert("dashboard.healthy_providers", "Healthy Providers");
    m.insert(
        "dashboard.healthy_providers_desc",
        "Currently healthy routing targets",
    );
    m.insert("dashboard.account_cache", "Channel Status Cache");
    m.insert(
        "dashboard.account_cache_desc",
        "Entries currently stored in account status cache",
    );
    m.insert("dashboard.fallback_count", "Fallback Count");
    m.insert(
        "dashboard.fallback_count_desc",
        "From real gateway statistics",
    );
    m.insert(
        "dashboard.total_distribution_earnings",
        "Total Distribution Earnings",
    );
    m.insert(
        "dashboard.total_distribution_earnings_desc",
        "Accumulated referral earnings",
    );
    m.insert(
        "dashboard.pending_distribution_earnings",
        "Pending Distribution Earnings",
    );
    m.insert(
        "dashboard.pending_distribution_earnings_desc",
        "Not yet settled into withdrawable balance",
    );
    m.insert(
        "dashboard.referral_count_desc",
        "Currently bound referral relationships",
    );
    m.insert("dashboard.latest_order", "Latest Order");
    m.insert(
        "dashboard.latest_order_desc",
        "Status of the most recent recharge order",
    );
    m.insert("dashboard.none", "None");
    m.insert("dashboard.last_used_prefix", "Last used");
    m.insert("dashboard.no_usage_record", "No usage record");

    m.insert(
        "system.subtitle",
        "Review provider health, gateway runtime metrics, and routing diagnostics",
    );
    m.insert("system.provider_health", "Provider Health");
    m.insert(
        "system.no_healthy_provider",
        "No healthy providers right now",
    );
    m.insert("system.gateway_stats", "Gateway Stats");
    m.insert("system.total_requests", "Total Requests");
    m.insert("system.success_rate", "Success Rate");
    m.insert("system.avg_latency", "Average Latency");
    m.insert("system.fallback_count", "Fallback Count");
    m.insert("system.routing_debug", "Routing Debug");
    m.insert(
        "system.provider_status_diagnosis",
        "Provider Status Diagnostics",
    );
    m.insert("system.route_success", "Route succeeded");
    m.insert("system.primary_target", "Primary Target");
    m.insert("system.fallback_chain", "Fallback Chain");
    m.insert("system.items", "items");
    m.insert("system.route_failed", "Route failed");
    m.insert("system.provider_status", "Provider Status");
    m.insert("system.no_provider_configured", "No providers configured");
    m.insert("system.health_status", "Health");
    m.insert("system.account_count", "Accounts");
    m.insert("system.healthy", "Healthy");
    m.insert("system.unhealthy", "Unhealthy");
    m.insert("system.pricing_info", "Pricing");
    m.insert("system.degraded", "Degraded");
    m.insert("system.abnormal", "Abnormal");
    m.insert("system.unknown", "Unknown");
    m.insert(
        "users.subtitle",
        "View and manage all registered users on the platform",
    );
    m.insert("users.search_placeholder", "Search by email or username...");
    m.insert("users.empty", "No users yet");
    m.insert("users.user", "User");
    m.insert("users.tenant", "Tenant");
    m.insert("users.registered_at", "Registered At");
    m.insert("users.updated", "User updated");
    m.insert("users.update_failed", "Update failed");
    m.insert("users.deleted", "User deleted");
    m.insert("users.delete_failed", "Delete failed");
    m.insert("users.edit_title", "Edit User");
    m.insert("users.display_name", "Display Name");
    m.insert(
        "users.display_name_placeholder",
        "Leave blank to keep unchanged",
    );
    m.insert("users.role_user", "user (standard)");
    m.insert("users.role_admin", "admin (administrator)");
    m.insert("users.delete_confirm_title", "Confirm Deletion");
    m.insert("users.delete_confirm_prefix", "Delete user");
    m.insert(
        "users.delete_confirm_suffix",
        "This action cannot be undone.",
    );
    m.insert("users.deleting", "Deleting...");
    m.insert("users.confirm_delete", "Confirm Delete");
    m.insert("users.self_title", "My Account");
    m.insert(
        "users.self_desc",
        "Review and manage your personal account information",
    );
    m.insert("users.account_info", "Account Information");
    m.insert(
        "tenants.subtitle",
        "View and manage all tenant records on the platform",
    );
    m.insert(
        "tenants.search_placeholder",
        "Search by tenant name or ID...",
    );
    m.insert("tenants.empty", "No tenants yet");
    m.insert("tenants.tenant_id", "Tenant ID");
    m.insert("tenants.active", "Active");
    m.insert(
        "distribution_records.admin_desc",
        "Review platform-wide distribution earnings and currently effective rules",
    );
    m.insert(
        "distribution_records.user_desc",
        "Review referral earnings generated from your invitations",
    );
    m.insert(
        "distribution_records.rules_title",
        "Distribution Rules (Read Only)",
    );
    m.insert("distribution_records.rules_hint", "Distribution rules are managed centrally by the platform. Contact a system administrator to change them.");
    m.insert(
        "distribution_records.no_rules",
        "No distribution rules found",
    );
    m.insert("distribution_records.rule_name", "Rule Name");
    m.insert("distribution_records.commission_rate", "Commission Rate");
    m.insert(
        "distribution_records.empty_admin",
        "No distribution records yet",
    );
    m.insert("distribution_records.record_id", "Record ID");
    m.insert("distribution_records.source_user_id", "Source User ID");
    m.insert("distribution_records.amount_spent", "Spent Amount");
    m.insert("distribution_records.commission_amount", "Commission");
    m.insert("distribution_records.referrer_id", "Referrer ID");
    m.insert("distribution_records.empty_user", "No referral records yet");
    m.insert("distribution_records.referred_user", "Referred User");
    m.insert("accounts.subtitle", "Maintain provider channels, model mapping, and availability in one reviewable asset pool for the routing layer.");
    m.insert("accounts.reset_failed", "Reset failed");
    m.insert("accounts.fill_required", "Please fill in required fields");
    m.insert("accounts.created", "Channel created");
    m.insert("accounts.create_failed", "Create failed");
    m.insert("accounts.name_required", "Channel name is required");
    m.insert("accounts.updated", "Channel updated");
    m.insert("accounts.update_failed", "Update failed");
    m.insert("accounts.resetting", "Resetting...");
    m.insert("accounts.reset_health", "Reset Health");
    m.insert("accounts.add_channel", "+ Add Channel");
    m.insert(
        "accounts.empty",
        "No channels configured yet. Use Add Channel to create one.",
    );
    m.insert("accounts.table_title", "Channel Asset Table");
    m.insert(
        "accounts.table_subtitle",
        "Review channel availability, model coverage, and rate headroom grouped by provider.",
    );
    m.insert("accounts.channels_suffix", "channels");
    m.insert("accounts.channel", "Channel");
    m.insert("accounts.provider_model", "Provider / Model");
    m.insert("accounts.runtime_status", "Runtime Status");
    m.insert("accounts.rate_quota", "Rate Quota");
    m.insert("accounts.key_preview", "Key Preview");
    m.insert(
        "accounts.default_endpoint",
        "Using provider default endpoint",
    );
    m.insert("accounts.no_models", "No models configured");
    m.insert("accounts.route_ready", "Available for normal routing");
    m.insert(
        "accounts.enabled_but_unhealthy",
        "Enabled, but health status is abnormal",
    );
    m.insert("accounts.not_routed", "Not participating in routing");
    m.insert("accounts.rpm_label", "Current RPM / Limit");
    m.insert("accounts.last_used", "Last Used");
    m.insert("accounts.no_usage_record", "No record");
    m.insert("accounts.test_success", "Connection test succeeded");
    m.insert("accounts.test_failed", "Connection test failed");
    m.insert("accounts.test", "Test");
    m.insert("accounts.create_title", "Create LLM Channel");
    m.insert("accounts.channel_name", "Channel Name *");
    m.insert(
        "accounts.channel_name_placeholder",
        "For example: OpenAI Official",
    );
    m.insert("accounts.provider", "Provider *");
    m.insert(
        "accounts.supported_models",
        "Supported Models (Optional, defaults if blank)",
    );
    m.insert(
        "accounts.models_hint",
        "Separate multiple models with commas. Leave blank to use provider defaults.",
    );
    m.insert("accounts.api_key", "API Key *");
    m.insert("accounts.custom_base_url", "Custom Base URL (Optional)");
    m.insert("accounts.edit_title", "Edit LLM Channel");
    m.insert(
        "accounts.new_api_key",
        "New API Key (leave blank to keep current)",
    );
    m.insert(
        "accounts.new_api_key_placeholder",
        "Leave blank to keep the current key",
    );
    m.insert(
        "accounts.custom_base_url_optional",
        "Custom Base URL (leave blank to keep current)",
    );
    m.insert("accounts.enable_channel", "Enable channel");
    m.insert("accounts.delete_confirm_title", "Confirm Deletion");
    m.insert("accounts.delete_confirm_prefix", "Delete channel \"");
    m.insert(
        "accounts.delete_confirm_suffix",
        "\"? This action cannot be undone.",
    );
    m.insert("accounts.deleted", "Channel deleted");
    m.insert("accounts.delete_failed", "Delete failed");
    m.insert("accounts.deleting", "Deleting...");
    m.insert("accounts.confirm_delete", "Confirm Delete");
    m.insert("accounts.no_permission_title", "No Access");
    m.insert(
        "accounts.no_permission_desc",
        "You do not have permission to access \"{resource}\". Contact an administrator.",
    );
    m.insert(
        "accounts.models_placeholder_openai",
        "For example: gpt-4o, gpt-4o-mini, gpt-4-turbo",
    );
    m.insert(
        "accounts.models_placeholder_claude",
        "For example: claude-3-5-sonnet-latest, claude-3-opus-latest",
    );
    m.insert(
        "accounts.models_placeholder_deepseek",
        "For example: deepseek-chat, deepseek-coder",
    );
    m.insert(
        "accounts.models_placeholder_gemini",
        "For example: gemini-1.5-pro, gemini-1.5-flash",
    );
    m.insert(
        "accounts.models_placeholder_vllm",
        "Enter vLLM model names, separated by commas",
    );
    m.insert(
        "accounts.models_placeholder_ollama",
        "Enter Ollama model names, separated by commas",
    );
    m.insert(
        "accounts.models_placeholder_default",
        "Enter model names, separated by commas",
    );

    m
});
