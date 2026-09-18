//! Disposable lab fixture only; never receives production endpoints or keys.
use anyhow::{Context, ensure};
use integration_tests::db::{create_test_tenant, create_test_user};
use keycompute_billing::balance::BalanceService;
use keycompute_db::{
    Account, CreateAccountRequest, CreateProduceAiKeyRequest, CreateUserRequest, DbRouter,
    ProduceAiKey, User,
};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{env, fs::OpenOptions, io::Write, path::PathBuf, time::Duration};
use uuid::Uuid;

fn setting(key: &str, default: usize, max: usize) -> anyhow::Result<usize> {
    let n = env::var(key)
        .ok()
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(default);
    ensure!(n > 0 && n <= max, "invalid lab setting {key}");
    Ok(n)
}
fn write_new(path: &PathBuf, data: &Value) -> anyhow::Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)?
        .write_all(serde_json::to_vec_pretty(data)?.as_slice())?;
    Ok(())
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ensure!(
        env::var("KC_LAB_ACK").as_deref() == Ok("1"),
        "explicit disposable-lab acknowledgement required"
    );
    let run = env::var("KC_LAB_RUN")?;
    let id = Uuid::parse_str(&run)?;
    let database = env::var("DATABASE_URL")?;
    let parsed = url::Url::parse(&database)?;
    ensure!(
        parsed.path() == format!("/kc_load_{}", id.simple()),
        "database must be this run's kc_load_ database"
    );
    let redis_url = env::var("REDIS_URL")?;
    let mut redis = tokio::time::timeout(
        Duration::from_secs(5),
        deadpool_redis::redis::Client::open(redis_url)?.get_multiplexed_async_connection(),
    )
    .await??;
    redis.set_response_timeout(Duration::from_secs(5));
    let marker: Option<String> = deadpool_redis::redis::cmd("GET")
        .arg("keycompute:capacity:run")
        .query_async(&mut redis)
        .await?;
    ensure!(
        marker.as_deref() == Some(run.as_str()),
        "Redis does not belong to this disposable run"
    );
    let path = PathBuf::from(env::var("KC_LAB_FIXTURE")?);
    let db = Database::connect(database)
        .await
        .context("lab database connection failed")?;
    match env::args().nth(1).as_deref() {
        Some("seed") => {
            ensure!(!path.exists(), "fixture already exists; never overwrite");
            keycompute_db::initialize_schema(&db).await?;
            keycompute_runtime::set_global_crypto(&env::var("KC__CRYPTO__SECRET_KEY")?)?;
            let tenants = setting("KC_LAB_TENANTS", 4, 32)?;
            let users = setting("KC_LAB_USERS", 2, 16)?;
            let accounts = setting("KC_LAB_ACCOUNTS", 8, 128)?;
            let balance = BalanceService::new(DbRouter::single(db.clone()));
            let mut tids = Vec::new();
            let mut keys = Vec::new();
            for t in 0..tenants {
                let tenant = create_test_tenant(&db, &format!("lab-{t}"), &run).await;
                db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE tenants SET default_rpm_limit=1000000,default_tpm_limit=1000000000 WHERE id=$1",[tenant.id.into()])).await?;
                tids.push(tenant.id);
                for u in 0..users {
                    let user =
                        create_test_user(&db, tenant.id, &format!("lab-{t}-{u}"), &run).await;
                    balance
                        .recharge(user.id, tenant.id, 100000.into(), None, None)
                        .await?;
                    let key = format!(
                        "sk-{}{}",
                        Uuid::new_v4().simple(),
                        &Uuid::new_v4().simple().to_string()[..16]
                    );
                    ProduceAiKey::create(
                        &db,
                        &CreateProduceAiKeyRequest {
                            tenant_id: tenant.id,
                            user_id: user.id,
                            name: "isolated lab".into(),
                            produce_ai_key_hash: hex::encode(Sha256::digest(key.as_bytes())),
                            produce_ai_key_preview: "sk-lab***".into(),
                            expires_at: None,
                        },
                    )
                    .await?;
                    keys.push(key);
                }
            }
            let admin = User::create(
                &db,
                &CreateUserRequest {
                    tenant_id: tids[0],
                    email: format!("lab-admin-{}@example.invalid", id.simple()),
                    name: Some("Lab diagnostics".into()),
                    role: Some(keycompute_types::UserRole::Admin),
                },
            )
            .await?;
            let jwt = keycompute_auth::jwt::JwtValidator::new(
                env::var("KC__AUTH__JWT_SECRET")?,
                "capacity-lab",
            );
            let token = jwt.generate_token(admin.id, tids[0], "admin")?;
            for n in 0..accounts {
                Account::create(
                    &db,
                    &CreateAccountRequest {
                        tenant_id: tids[0],
                        provider: "openai".into(),
                        name: format!("lab-{n}"),
                        endpoint: "http://model:8080/v1".into(),
                        upstream_api_key_encrypted: keycompute_runtime::encrypt_api_key(
                            "isolated-model-only",
                        )?
                        .into_inner(),
                        upstream_api_key_preview: "test***".into(),
                        rpm_limit: Some(1_000_000),
                        tpm_limit: Some(1_000_000_000),
                        priority: Some(0),
                        models_supported: vec!["gpt-4o".into()],
                        api_capabilities: vec!["chat_completions".into()],
                        visibility: Some("global".into()),
                    },
                )
                .await?;
            }
            write_new(
                &path,
                &json!({"run":run,"tenant_ids":tids,"keys":keys,"admin_token":token,"model":"gpt-4o"}),
            )?;
            println!("Disposable fixture written; credentials are not printed");
        }
        Some("verify") => {
            let fixture: Value = serde_json::from_slice(&std::fs::read(path)?)?;
            ensure!(
                fixture["run"].as_str() == Some(run.as_str()),
                "fixture belongs to another run"
            );
            let tids: Vec<Uuid> = serde_json::from_value(fixture["tenant_ids"].clone())?;
            let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
   "SELECT (SELECT COUNT(*) FROM usage_logs WHERE tenant_id=ANY($1))::BIGINT AS ledger, (SELECT COUNT(*) FROM balance_reservations WHERE tenant_id=ANY($1) AND status='active')::BIGINT AS active, (SELECT COUNT(*) FROM balance_reservations WHERE tenant_id=ANY($1) AND status='settled')::BIGINT AS settled, (SELECT COUNT(*) FROM user_balances WHERE tenant_id=ANY($1) AND (available_balance+frozen_balance+total_consumed<>total_recharged OR frozen_balance<>0))::BIGINT AS inconsistent",[tids.into()])).await?.context("verification row missing")?;
            println!(
                "{}",
                json!({"ledger":row.try_get::<i64>("","ledger")?,"active":row.try_get::<i64>("","active")?,"settled":row.try_get::<i64>("","settled")?,"inconsistent":row.try_get::<i64>("","inconsistent")?})
            );
        }
        _ => anyhow::bail!("expected seed or verify"),
    }
    db.close().await?;
    Ok(())
}
