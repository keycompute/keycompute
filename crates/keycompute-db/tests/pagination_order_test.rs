use async_trait::async_trait;
use keycompute_db::{DistributionRecord, ProduceAiKey};
use sea_orm::{
    Database, DatabaseConnection, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow,
    Statement, Value,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone, Debug)]
struct StatementCapture {
    statements: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ProxyDatabaseTrait for StatementCapture {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let is_count = statement.sql.contains("COUNT(*)");
        self.statements.lock().unwrap().push(statement.sql);
        if is_count {
            Ok(vec![ProxyRow::new(BTreeMap::from([(
                "count".to_string(),
                Value::BigInt(Some(0)),
            )]))])
        } else {
            Ok(Vec::new())
        }
    }

    async fn execute(&self, _statement: Statement) -> Result<ProxyExecResult, DbErr> {
        Ok(ProxyExecResult::default())
    }
}

async fn captured_connection() -> (DatabaseConnection, Arc<Mutex<Vec<String>>>) {
    let statements = Arc::new(Mutex::new(Vec::new()));
    let proxy = StatementCapture {
        statements: Arc::clone(&statements),
    };
    let connection = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(proxy)))
        .await
        .unwrap();
    (connection, statements)
}

#[tokio::test]
async fn paginated_queries_use_unique_created_at_tie_breakers() {
    let (db, statements) = captured_connection().await;

    ProduceAiKey::list_owned(
        &db,
        keycompute_types::TenantScope::checked(
            Uuid::new_v4(),
            Uuid::new_v4(),
            keycompute_types::TenantRole::Member,
        )
        .unwrap(),
        true,
        20,
        20,
    )
    .await
    .unwrap();
    keycompute_db::models::usage_log::UserUsageScope::new(
        keycompute_types::TenantScope::checked(
            Uuid::new_v4(),
            Uuid::new_v4(),
            keycompute_types::TenantRole::Member,
        )
        .unwrap(),
    )
    .list(&db, None, None, 20, 20)
    .await
    .unwrap();
    DistributionRecord::find_by_tenant_filtered(
        &db,
        Uuid::new_v4(),
        Some("pending"),
        Some("level1"),
        20,
        20,
    )
    .await
    .unwrap();
    DistributionRecord::find_by_beneficiary_filtered(
        &db,
        Uuid::new_v4(),
        Some("pending"),
        Some("level1"),
        20,
        20,
    )
    .await
    .unwrap();
    DistributionRecord::count_by_tenant_filtered(
        &db,
        Uuid::new_v4(),
        Some("pending"),
        Some("level1"),
    )
    .await
    .unwrap();
    DistributionRecord::count_by_beneficiary_filtered(
        &db,
        Uuid::new_v4(),
        Some("pending"),
        Some("level1"),
    )
    .await
    .unwrap();

    let statements = statements.lock().unwrap();
    assert_eq!(statements.len(), 6);
    assert!(statements[0].contains("ORDER BY k.created_at DESC, k.id DESC"));
    assert!(statements[1].contains("ORDER BY l.created_at DESC, l.id DESC"));
    assert!(statements[2].contains("ORDER BY created_at DESC, id DESC"));
    assert!(statements[3].contains("ORDER BY created_at DESC, id DESC"));
    for statement in &statements[4..] {
        assert!(statement.contains("status = $2"));
        assert!(statement.contains("level = $3"));
    }
}
