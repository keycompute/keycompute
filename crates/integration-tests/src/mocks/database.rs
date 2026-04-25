//! 模拟数据库

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::RwLock;
use uuid::Uuid;

/// 模拟用户数据
#[derive(Debug, Clone)]
pub struct MockUser {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl MockUser {
    pub fn new(tenant_id: Uuid, email: impl Into<String>, role: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            tenant_id,
            email: email.into(),
            name: None,
            role: role.into(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_id(mut self, id: Uuid) -> Self {
        self.id = id;
        self
    }
}

/// 模拟租户数据
#[derive(Debug, Clone)]
pub struct MockTenant {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub status: String,
    pub default_rpm_limit: i32,
    pub default_tpm_limit: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl MockTenant {
    pub fn new(name: impl Into<String>, slug: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            slug: slug.into(),
            description: None,
            status: "active".to_string(),
            default_rpm_limit: 60,
            default_tpm_limit: 100000,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_id(mut self, id: Uuid) -> Self {
        self.id = id;
        self
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = status.into();
        self
    }

    pub fn with_limits(mut self, rpm: i32, tpm: i32) -> Self {
        self.default_rpm_limit = rpm;
        self.default_tpm_limit = tpm;
        self
    }

    pub fn is_active(&self) -> bool {
        self.status == "active"
    }
}

/// 模拟 Produce AI Key 数据
#[derive(Debug, Clone)]
pub struct MockProduceAiKey {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub produce_ai_key_hash: String,
    pub produce_ai_key_preview: String,
    pub revoked: bool,
    pub revoked_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl MockProduceAiKey {
    pub fn new(user_id: Uuid, tenant_id: Uuid, produce_ai_key_hash: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            tenant_id,
            user_id,
            name: "Test Key".to_string(),
            produce_ai_key_hash: produce_ai_key_hash.into(),
            produce_ai_key_preview: "sk-****".to_string(),
            revoked: false,
            revoked_at: None,
            expires_at: None,
            last_used_at: None,
            created_at: now,
        }
    }

    pub fn with_id(mut self, id: Uuid) -> Self {
        self.id = id;
        self
    }

    pub fn with_revoked(mut self, revoked: bool) -> Self {
        self.revoked = revoked;
        if revoked {
            self.revoked_at = Some(Utc::now());
        }
        self
    }

    pub fn with_expires_at(mut self, expires_at: DateTime<Utc>) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    pub fn is_valid(&self) -> bool {
        if self.revoked {
            return false;
        }
        if let Some(expires) = self.expires_at
            && expires < Utc::now()
        {
            return false;
        }
        true
    }
}

/// 模拟 UsageLog 记录
#[derive(Debug, Clone)]
pub struct MockUsageLog {
    pub id: Uuid,
    pub request_id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub produce_ai_key_id: Uuid,
    pub model_name: String,
    pub provider_name: String,
    pub account_id: Uuid,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub total_tokens: i32,
    pub input_unit_price_snapshot: Decimal,
    pub output_unit_price_snapshot: Decimal,
    pub user_amount: Decimal,
    pub currency: String,
    pub usage_source: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

impl MockUsageLog {
    pub fn new(ctx: &super::MockExecutionContext) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            request_id: ctx.request_id,
            tenant_id: ctx.tenant_id,
            user_id: ctx.user_id,
            produce_ai_key_id: ctx.produce_ai_key_id,
            model_name: ctx.model.clone(),
            provider_name: ctx.provider.clone(),
            account_id: ctx.account_id,
            input_tokens: 10,
            output_tokens: 4,
            total_tokens: 14,
            input_unit_price_snapshot: Decimal::from(1),
            output_unit_price_snapshot: Decimal::from(2),
            user_amount: Decimal::from(18) / Decimal::from(1000), // (10*1 + 4*2) / 1000
            currency: "CNY".to_string(),
            usage_source: "gateway_accumulated".to_string(),
            status: "success".to_string(),
            started_at: now,
            finished_at: now,
        }
    }

    pub fn with_tokens(mut self, input: i32, output: i32) -> Self {
        self.input_tokens = input;
        self.output_tokens = output;
        self.total_tokens = input + output;
        self.user_amount = self.calculate_amount();
        self
    }

    pub fn with_pricing(mut self, input_price: Decimal, output_price: Decimal) -> Self {
        self.input_unit_price_snapshot = input_price;
        self.output_unit_price_snapshot = output_price;
        self.user_amount = self.calculate_amount();
        self
    }

    fn calculate_amount(&self) -> Decimal {
        let input_cost =
            Decimal::from(self.input_tokens) * self.input_unit_price_snapshot / Decimal::from(1000);
        let output_cost = Decimal::from(self.output_tokens) * self.output_unit_price_snapshot
            / Decimal::from(1000);
        input_cost + output_cost
    }
}

/// 模拟 DistributionRecord
#[derive(Debug, Clone)]
pub struct MockDistributionRecord {
    pub id: Uuid,
    pub usage_log_id: Uuid,
    pub tenant_id: Uuid,
    pub beneficiary_id: Uuid,
    pub share_ratio: Decimal,
    pub share_amount: Decimal,
    pub currency: String,
    pub status: String,
}

impl MockDistributionRecord {
    pub fn new(usage_log: &MockUsageLog, beneficiary_id: Uuid, ratio: Decimal) -> Self {
        Self {
            id: Uuid::new_v4(),
            usage_log_id: usage_log.id,
            tenant_id: usage_log.tenant_id,
            beneficiary_id,
            share_ratio: ratio,
            share_amount: usage_log.user_amount * ratio,
            currency: usage_log.currency.clone(),
            status: "pending".to_string(),
        }
    }
}

/// 模拟用户凭证
#[derive(Debug, Clone)]
pub struct MockUserCredential {
    pub id: Uuid,
    pub user_id: Uuid,
    pub password_hash: String,
    pub email_verified: bool,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub failed_login_attempts: i32,
    pub locked_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl MockUserCredential {
    pub fn new(user_id: Uuid, password_hash: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            user_id,
            password_hash: password_hash.into(),
            email_verified: false,
            email_verified_at: None,
            failed_login_attempts: 0,
            locked_until: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_email_verified(mut self, verified: bool) -> Self {
        self.email_verified = verified;
        if verified {
            self.email_verified_at = Some(Utc::now());
        }
        self
    }
}

/// 模拟邮箱验证记录
#[derive(Debug, Clone)]
pub struct MockEmailVerification {
    pub id: Uuid,
    pub user_id: Uuid,
    pub email: String,
    pub token: String,
    pub used: bool,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

impl MockEmailVerification {
    pub fn new(user_id: Uuid, email: impl Into<String>, token: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            user_id,
            email: email.into(),
            token: token.into(),
            used: false,
            expires_at: now + chrono::Duration::hours(24),
            created_at: now,
        }
    }

    pub fn is_valid(&self) -> bool {
        !self.used && self.expires_at > Utc::now()
    }
}

/// 模拟密码重置记录
#[derive(Debug, Clone)]
pub struct MockPasswordReset {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token: String,
    pub used: bool,
    pub expires_at: DateTime<Utc>,
    pub requested_from_ip: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl MockPasswordReset {
    pub fn new(user_id: Uuid, token: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            user_id,
            token: token.into(),
            used: false,
            expires_at: now + chrono::Duration::hours(1),
            requested_from_ip: None,
            created_at: now,
        }
    }

    pub fn is_valid(&self) -> bool {
        !self.used && self.expires_at > Utc::now()
    }

    pub fn with_ip(mut self, ip: impl Into<String>) -> Self {
        self.requested_from_ip = Some(ip.into());
        self
    }
}

/// 内存数据库模拟
#[derive(Debug, Default)]
pub struct MockDatabase {
    usage_logs: std::sync::Mutex<Vec<MockUsageLog>>,
    distribution_records: std::sync::Mutex<Vec<MockDistributionRecord>>,
}

impl MockDatabase {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_usage_log(&self, log: MockUsageLog) {
        self.usage_logs.lock().unwrap().push(log);
    }

    pub fn insert_distribution_record(&self, record: MockDistributionRecord) {
        self.distribution_records.lock().unwrap().push(record);
    }

    pub fn get_usage_logs(&self) -> Vec<MockUsageLog> {
        self.usage_logs.lock().unwrap().clone()
    }

    pub fn get_distribution_records(&self) -> Vec<MockDistributionRecord> {
        self.distribution_records.lock().unwrap().clone()
    }

    pub fn get_usage_log_by_request(&self, request_id: Uuid) -> Option<MockUsageLog> {
        self.usage_logs
            .lock()
            .unwrap()
            .iter()
            .find(|log| log.request_id == request_id)
            .cloned()
    }

    pub fn clear(&self) {
        self.usage_logs.lock().unwrap().clear();
        self.distribution_records.lock().unwrap().clear();
    }
}

/// 用户/租户模拟数据库
#[derive(Debug)]
pub struct MockUserTenantDatabase {
    tenants: RwLock<HashMap<Uuid, MockTenant>>,
    users: RwLock<HashMap<Uuid, MockUser>>,
    produce_ai_keys: RwLock<HashMap<Uuid, MockProduceAiKey>>,
    produce_ai_keys_by_hash: RwLock<HashMap<String, Uuid>>,
    users_by_email: RwLock<HashMap<String, Uuid>>,
}

impl Default for MockUserTenantDatabase {
    fn default() -> Self {
        Self::new()
    }
}

impl MockUserTenantDatabase {
    pub fn new() -> Self {
        Self {
            tenants: RwLock::new(HashMap::new()),
            users: RwLock::new(HashMap::new()),
            produce_ai_keys: RwLock::new(HashMap::new()),
            produce_ai_keys_by_hash: RwLock::new(HashMap::new()),
            users_by_email: RwLock::new(HashMap::new()),
        }
    }

    // === 租户操作 ===

    pub fn insert_tenant(&self, tenant: MockTenant) {
        let id = tenant.id;
        self.tenants.write().unwrap().insert(id, tenant);
    }

    pub fn get_tenant(&self, id: Uuid) -> Option<MockTenant> {
        self.tenants.read().unwrap().get(&id).cloned()
    }

    pub fn get_tenant_by_slug(&self, slug: &str) -> Option<MockTenant> {
        self.tenants
            .read()
            .unwrap()
            .values()
            .find(|t| t.slug == slug)
            .cloned()
    }

    pub fn update_tenant_status(&self, id: Uuid, status: &str) {
        if let Some(tenant) = self.tenants.write().unwrap().get_mut(&id) {
            tenant.status = status.to_string();
            tenant.updated_at = Utc::now();
        }
    }

    // === 用户操作 ===

    pub fn insert_user(&self, user: MockUser) {
        let id = user.id;
        let email = user.email.clone();
        self.users.write().unwrap().insert(id, user);
        self.users_by_email.write().unwrap().insert(email, id);
    }

    pub fn get_user(&self, id: Uuid) -> Option<MockUser> {
        self.users.read().unwrap().get(&id).cloned()
    }

    pub fn get_user_by_email(&self, email: &str) -> Option<MockUser> {
        let id = self.users_by_email.read().unwrap().get(email).copied()?;
        self.get_user(id)
    }

    pub fn get_users_by_tenant(&self, tenant_id: Uuid) -> Vec<MockUser> {
        self.users
            .read()
            .unwrap()
            .values()
            .filter(|u| u.tenant_id == tenant_id)
            .cloned()
            .collect()
    }

    // === Produce AI Key 操作 ===

    pub fn insert_produce_ai_key(&self, produce_ai_key: MockProduceAiKey) {
        let id = produce_ai_key.id;
        let produce_ai_key_hash = produce_ai_key.produce_ai_key_hash.clone();
        self.produce_ai_keys
            .write()
            .unwrap()
            .insert(id, produce_ai_key);
        self.produce_ai_keys_by_hash
            .write()
            .unwrap()
            .insert(produce_ai_key_hash, id);
    }

    pub fn get_produce_ai_key(&self, id: Uuid) -> Option<MockProduceAiKey> {
        self.produce_ai_keys.read().unwrap().get(&id).cloned()
    }

    pub fn get_produce_ai_key_by_hash(
        &self,
        produce_ai_key_hash: &str,
    ) -> Option<MockProduceAiKey> {
        let id = self
            .produce_ai_keys_by_hash
            .read()
            .unwrap()
            .get(produce_ai_key_hash)
            .copied()?;
        self.get_produce_ai_key(id)
    }

    pub fn revoke_produce_ai_key(&self, id: Uuid) {
        if let Some(key) = self.produce_ai_keys.write().unwrap().get_mut(&id) {
            key.revoked = true;
            key.revoked_at = Some(Utc::now());
        }
    }

    pub fn update_produce_ai_key_last_used(&self, id: Uuid) {
        if let Some(key) = self.produce_ai_keys.write().unwrap().get_mut(&id) {
            key.last_used_at = Some(Utc::now());
        }
    }

    // === 辅助方法 ===

    /// 清空所有数据
    pub fn clear(&self) {
        self.tenants.write().unwrap().clear();
        self.users.write().unwrap().clear();
        self.produce_ai_keys.write().unwrap().clear();
        self.produce_ai_keys_by_hash.write().unwrap().clear();
        self.users_by_email.write().unwrap().clear();
    }

    /// 创建测试租户并返回
    pub fn create_test_tenant(&self) -> MockTenant {
        let tenant = MockTenant::new("Test Tenant", "test-tenant");
        self.insert_tenant(tenant.clone());
        tenant
    }

    /// 创建测试用户并返回
    pub fn create_test_user(&self, tenant_id: Uuid, role: &str) -> MockUser {
        let email = format!("test-{}@example.com", Uuid::new_v4().simple());
        let user = MockUser::new(tenant_id, email, role);
        self.insert_user(user.clone());
        user
    }

    /// 创建测试 Produce AI Key 并返回
    pub fn create_test_produce_ai_key(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> (MockProduceAiKey, String) {
        let raw_key = format!("sk-test-{}", Uuid::new_v4().simple());
        let produce_ai_key_hash = sha256_hash(&raw_key);
        let produce_ai_key = MockProduceAiKey::new(user_id, tenant_id, produce_ai_key_hash);
        self.insert_produce_ai_key(produce_ai_key.clone());
        (produce_ai_key, raw_key)
    }

    /// 获取统计信息
    pub fn stats(&self) -> MockDatabaseStats {
        MockDatabaseStats {
            tenant_count: self.tenants.read().unwrap().len(),
            user_count: self.users.read().unwrap().len(),
            produce_ai_key_count: self.produce_ai_keys.read().unwrap().len(),
        }
    }
}

/// 模拟数据库统计信息
#[derive(Debug, Clone)]
pub struct MockDatabaseStats {
    pub tenant_count: usize,
    pub user_count: usize,
    pub produce_ai_key_count: usize,
}

/// SHA256 哈希
fn sha256_hash(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_usage_log() {
        let ctx = super::super::MockExecutionContext::new();
        let log = MockUsageLog::new(&ctx);

        assert_eq!(log.request_id, ctx.request_id);
        assert_eq!(log.model_name, ctx.model);
        assert!(log.user_amount > Decimal::ZERO);
    }

    #[test]
    fn test_mock_database() {
        let db = MockDatabase::new();
        let ctx = super::super::MockExecutionContext::new();
        let log = MockUsageLog::new(&ctx);

        db.insert_usage_log(log.clone());

        let logs = db.get_usage_logs();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].request_id, ctx.request_id);
    }

    #[test]
    fn test_mock_tenant() {
        let tenant = MockTenant::new("Test Corp", "test-corp").with_limits(100, 50000);

        assert_eq!(tenant.name, "Test Corp");
        assert_eq!(tenant.slug, "test-corp");
        assert_eq!(tenant.default_rpm_limit, 100);
        assert_eq!(tenant.default_tpm_limit, 50000);
        assert!(tenant.is_active());
    }

    #[test]
    fn test_mock_tenant_status() {
        let tenant = MockTenant::new("Test", "test").with_status("suspended");
        assert!(!tenant.is_active());
    }

    #[test]
    fn test_mock_user() {
        let tenant_id = Uuid::new_v4();
        let user = MockUser::new(tenant_id, "test@example.com", "admin").with_name("Test User");

        assert_eq!(user.email, "test@example.com");
        assert_eq!(user.role, "admin");
        assert_eq!(user.name, Some("Test User".to_string()));
        assert_eq!(user.tenant_id, tenant_id);
    }

    #[test]
    fn test_mock_produce_ai_key() {
        let user_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();
        let key = MockProduceAiKey::new(user_id, tenant_id, "test-hash");

        assert_eq!(key.user_id, user_id);
        assert_eq!(key.tenant_id, tenant_id);
        assert!(key.is_valid());
    }

    #[test]
    fn test_mock_produce_ai_key_revoked() {
        let key = MockProduceAiKey::new(Uuid::new_v4(), Uuid::new_v4(), "hash").with_revoked(true);
        assert!(!key.is_valid());
    }

    #[test]
    fn test_mock_produce_ai_key_expired() {
        let key = MockProduceAiKey::new(Uuid::new_v4(), Uuid::new_v4(), "hash")
            .with_expires_at(Utc::now() - chrono::Duration::hours(1));
        assert!(!key.is_valid());
    }

    #[test]
    fn test_mock_user_tenant_database() {
        let db = MockUserTenantDatabase::new();

        // 创建租户
        let tenant = db.create_test_tenant();
        assert!(db.get_tenant(tenant.id).is_some());

        // 创建用户
        let user = db.create_test_user(tenant.id, "user");
        assert!(db.get_user(user.id).is_some());
        assert!(db.get_user_by_email(&user.email).is_some());

        // 创建 Produce AI Key
        let (produce_ai_key, raw_key) = db.create_test_produce_ai_key(user.id, tenant.id);
        assert!(db.get_produce_ai_key(produce_ai_key.id).is_some());
        assert!(raw_key.starts_with("sk-test-"));

        // 统计
        let stats = db.stats();
        assert_eq!(stats.tenant_count, 1);
        assert_eq!(stats.user_count, 1);
        assert_eq!(stats.produce_ai_key_count, 1);
    }

    #[test]
    fn test_mock_user_tenant_database_operations() {
        let db = MockUserTenantDatabase::new();

        // 创建多个用户
        let tenant = db.create_test_tenant();
        let user1 = db.create_test_user(tenant.id, "user");
        let _user2 = db.create_test_user(tenant.id, "admin");

        // 按租户查询
        let users = db.get_users_by_tenant(tenant.id);
        assert_eq!(users.len(), 2);

        // 更新租户状态
        db.update_tenant_status(tenant.id, "suspended");
        let updated_tenant = db.get_tenant(tenant.id).unwrap();
        assert_eq!(updated_tenant.status, "suspended");

        // 撤销 Produce AI Key
        let (produce_ai_key, _) = db.create_test_produce_ai_key(user1.id, tenant.id);
        assert!(db.get_produce_ai_key(produce_ai_key.id).unwrap().is_valid());

        db.revoke_produce_ai_key(produce_ai_key.id);
        assert!(!db.get_produce_ai_key(produce_ai_key.id).unwrap().is_valid());
    }
}
