//! 数据库模型模块
//
//! 包含所有表的 ORM 模型和 CRUD 操作

pub mod account;
pub mod api_key;
pub mod distribution_policy;
pub mod distribution_record;
pub mod distribution_scope;
pub mod financial_scope;
pub mod native_capability;
pub mod node;
pub mod node_control;
pub mod node_session;
pub mod node_task;
pub mod node_task_submission;
pub mod node_tip;
pub mod node_tip_withdrawal;
pub mod passthrough_binding;
pub mod password_reset;
pub mod payment_order;
pub mod pending_registration;
pub mod pricing_model;
mod query;
pub mod response_affinity;
pub mod responses_idempotency_claim;
pub mod system_setting;
pub mod tenant;
pub mod tenant_audit_event;
pub mod tenant_distribution_rule;
pub mod tenant_invitation;
pub mod tenant_membership;
pub mod upstream_access;
pub mod usage_log;
pub mod user;
pub mod user_balance;
pub mod user_credential;
pub mod user_node_gateway_token;
pub mod user_referral;

// 重新导出常用模型
pub use account::{
    ACCOUNT_PRIORITY_MAX, ACCOUNT_PRIORITY_MIN, Account, CreateAccountRequest, UpdateAccountRequest,
};
pub use api_key::{CreateProduceAiKeyRequest, ProduceAiKey, ProduceAiKeyResponse};
pub use distribution_record::{
    CreateDistributionRecordRequest, DistributionLevelStats, DistributionRecord, DistributionStats,
};
pub use distribution_scope::{DistributionCurrencyStats, DistributionRecordReport};
pub use node::{
    CreateNodeRequest, NODE_STATUS_EXCLUDED, NODE_STATUS_OFFLINE, NODE_STATUS_ONLINE, Node,
};
pub use node_session::{CreateNodeSessionRequest, NodeSession, NodeSessionScope};
pub use node_task::{
    CreateNodeTaskRequest, NodeTask, TASK_STATUS_EXPIRED, TASK_STATUS_FAILED, TASK_STATUS_LEASED,
    TASK_STATUS_QUEUED, TASK_STATUS_SUCCEEDED,
};
pub use node_task_submission::{CreateNodeTaskSubmissionRequest, NodeTaskSubmission};
pub use node_tip::{NodeTip, NodeTipSummary};
pub use node_tip_withdrawal::{
    CompleteWithdrawal, NodeTipWithdrawal, ReviewWithdrawal, WITHDRAWAL_STATUS_APPROVED,
    WITHDRAWAL_STATUS_COMPLETED, WITHDRAWAL_STATUS_PENDING, WITHDRAWAL_STATUS_REJECTED,
    WITHDRAWAL_TYPE_ALIPAY, WITHDRAWAL_TYPE_BALANCE, WithdrawalFilter, WithdrawalIntent,
    WithdrawalReview, WithdrawalView,
};
pub use passthrough_binding::{
    AccountModelHealth, AccountModelHealthProbe, CreatePassthroughBindingRequest,
    PASSTHROUGH_BINDING_MAX_PAGE_SIZE, PassthroughBinding, PassthroughBindingCount,
    UpdatePassthroughBindingRequest,
};
pub use password_reset::{CreatePasswordResetRequest, PasswordReset};
pub use payment_order::{
    CreatePaymentOrderRequest, CreditPaidOrderError, PaymentMethod, PaymentOrder,
    PaymentOrderStats, PaymentOrderStatus,
};
pub use pending_registration::{PendingRegistration, UpsertPendingRegistrationRequest};
pub use pricing_model::{
    CreatePricingRequest, PricingModel, PricingScopeType, UpdatePricingRequest,
};
pub use response_affinity::{
    ResponseAffinity, SettlementClaimCursor, SettlementRecoveryCursor, SettlementRecoveryRow,
};
pub use responses_idempotency_claim::ResponsesIdempotencyClaim;
pub use system_setting::{
    BatchUpdateSettingsRequest, PublicSettings, SettingValueType, SystemSetting,
    SystemSettingResponse, UpdateSystemSettingRequest,
};
pub use tenant::{
    CreateTenantRequest, Tenant, TenantDeletionBlockers, TenantFinancialDeletionBlockers,
    UpdateTenantRequest,
};
pub use tenant_audit_event::{AuditContext, TenantAuditEvent};
pub use tenant_distribution_rule::{
    BeneficiaryScope, CreateDistributionRuleRequest, TenantDistributionRule,
    UpdateDistributionRuleRequest,
};
pub use tenant_invitation::{
    CreateTenantInvitationRequest, CreatedTenantInvitation, TenantInvitation,
};
pub use tenant_membership::{CreateTenantMembershipRequest, TenantMembership};
pub use usage_log::{CreateUsageLogRequest, UsageLog, UsageStats, UserUsageStats};
pub use user::{CreateUserRequest, UpdateUserRequest, User};
pub use user_balance::{
    BalanceReservation, BalanceReservationEvent, BalanceReservationPageCursor, BalanceTransaction,
    ManualBalanceCommand, ManualBalanceOperationDecision, ManualBalanceOperationKind,
    ManualBalanceOperationOutcome, ReleaseReservationCommand, TransactionType, UserBalance,
    UserBalanceBreakdown, UserBalanceBreakdownPage, UserBalanceDisplaySnapshot,
};
pub use user_credential::{
    CreateUserCredentialRequest, UpdateUserCredentialRequest, UserCredential,
};
pub use user_node_gateway_token::{
    PendingTokenWithUser, UserNodeGatewayToken, UserNodeGatewayTokenResponse,
};
pub use user_referral::{CreateUserReferralRequest, ReferralStats, UserReferral};

pub mod referral_display;

pub mod console_display;

pub mod tenant_control;

pub mod key_issuance;

pub mod node_tip_setting;

pub mod platform_operations;
