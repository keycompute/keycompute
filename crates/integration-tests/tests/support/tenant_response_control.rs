//! Independent tenant control tests sharing the real scoped-resource fixture.
use super::*;
use sea_orm::{ConnectOptions, Database, TransactionTrait};

async fn tenant_owner_console_token(f: &Fixture) -> String {
    let tenant = keycompute_db::Tenant::find_by_id(&f.db, f.user.tenant_id)
        .await
        .unwrap()
        .unwrap();
    let owner = keycompute_db::User::find_by_id(&f.db, tenant.owner_user_id)
        .await
        .unwrap()
        .unwrap();
    let raw = f
        .state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(owner.id, None, owner.token_version, None, None, 3600)
        .unwrap();
    let global = f.state.auth.verify_token(&raw).await.unwrap();
    f.state
        .auth
        .select_tenant(&global, Some(tenant.id))
        .await
        .unwrap()
        .access_token
}

#[tokio::test]
async fn tenant_response_control_revalidates_authority_and_preserves_owner_scope() {
    let mut f = Fixture::new().await;
    let mut payload = f.body(Op::Responses);
    payload["store"] = true.into();
    let created = expect(
        f.request(Method::POST, "/pt/v1/responses", Some(payload))
            .await,
        StatusCode::OK,
    );
    let response_id = created["id"].as_str().unwrap().to_owned();
    let tenant_id = f.user.tenant_id;
    let owner_id = f.user.id;
    let admin = tenant_owner_console_token(&f).await;
    let base = format!("/api/v1/tenants/{tenant_id}/responses");

    let member = scoped_jwt(&f.state, &f.user).await;
    expect(
        http(
            f.app.clone(),
            Method::GET,
            &format!("{base}/count?mode=passthrough&owner_user_id={owner_id}"),
            Some(&member),
            None,
        )
        .await,
        StatusCode::FORBIDDEN,
    );
    expect(
        http(
            f.app.clone(),
            Method::GET,
            &format!("{base}/count?mode=passthrough&owner_user_id={owner_id}"),
            Some(&f.key),
            None,
        )
        .await,
        StatusCode::FORBIDDEN,
    );

    let foreign_tenant = create_test_tenant(&f.db, "response-control-foreign", &f.run).await;
    let foreign_owner = keycompute_db::User::find_by_id(&f.db, foreign_tenant.owner_user_id)
        .await
        .unwrap()
        .unwrap();
    let raw_foreign = f
        .state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(
            foreign_owner.id,
            None,
            foreign_owner.token_version,
            None,
            None,
            3600,
        )
        .unwrap();
    let foreign_ctx = f.state.auth.verify_token(&raw_foreign).await.unwrap();
    let foreign_admin = f
        .state
        .auth
        .select_tenant(&foreign_ctx, Some(foreign_tenant.id))
        .await
        .unwrap()
        .access_token;
    expect(
        http(
            f.app.clone(),
            Method::GET,
            &format!("{base}/count?mode=passthrough&owner_user_id={owner_id}"),
            Some(&foreign_admin),
            None,
        )
        .await,
        StatusCode::FORBIDDEN,
    );

    let count = expect(
        http(
            f.app.clone(),
            Method::GET,
            &format!("{base}/count?mode=passthrough&owner_user_id={owner_id}"),
            Some(&admin),
            None,
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(count["total"], 1);
    let list = expect(
        http(
            f.app.clone(),
            Method::GET,
            &format!("{base}?mode=passthrough&owner_user_id={owner_id}&page=1&page_size=10"),
            Some(&admin),
            None,
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(list["total"], count["total"]);
    assert_eq!(list["items"][0]["id"], response_id);
    assert!(list["items"][0].get("response").is_none());
    let revision = list["items"][0]["revision"].as_i64().unwrap();

    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET status='removed' WHERE tenant_id=$1 AND user_id=$2",
        [tenant_id.into(), owner_id.into()],
    ))
    .await
    .unwrap();
    let detail_path = format!("{base}/passthrough/{owner_id}/{response_id}");
    let detail = http(f.app.clone(), Method::GET, &detail_path, Some(&admin), None).await;
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.body);
    assert_eq!(detail.body["response"]["id"], response_id);
    assert!(
        detail
            .headers
            .get("cache-control")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("no-store")
    );

    let deleted = expect(
        http(
            f.app.clone(),
            Method::DELETE,
            &detail_path,
            Some(&admin),
            Some(json!({"expected_revision":revision})),
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(deleted["deleted"], true);
    let deleted_again = expect(
        http(
            f.app.clone(),
            Method::DELETE,
            &detail_path,
            Some(&admin),
            Some(json!({"expected_revision":revision + 1})),
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(deleted_again["deleted"], true);

    let root_user = create_test_user(&f.db, tenant_id, "response-control-root", &f.run).await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET platform_role='root' WHERE id=$1",
        [root_user.id.into()],
    ))
    .await
    .unwrap();
    let root = keycompute_db::User::find_by_id(&f.db, root_user.id)
        .await
        .unwrap()
        .unwrap();
    let root_token = f
        .state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(root.id, None, root.token_version, None, None, 3600)
        .unwrap();
    expect(
        http(
            f.app.clone(),
            Method::GET,
            &format!("/api/v1/platform/tenants/{tenant_id}/responses/count?mode=passthrough&owner_user_id={owner_id}&reason=incident%20review"),
            Some(&root_token),
            None,
        )
        .await,
        StatusCode::OK,
    );

    let stale_admin = tenant_owner_console_token(&f).await;
    let tenant = keycompute_db::Tenant::find_by_id(&f.db, tenant_id)
        .await
        .unwrap()
        .unwrap();
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET token_version=token_version+1 WHERE id=$1",
        [tenant.owner_user_id.into()],
    ))
    .await
    .unwrap();
    let stale = http(
        f.app.clone(),
        Method::GET,
        &format!("{base}/count?mode=passthrough&owner_user_id={owner_id}"),
        Some(&stale_admin),
        None,
    )
    .await;
    assert!(
        matches!(
            stale.status,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ),
        "{}",
        stale.body
    );

    // Regrant a delegated administrator, never the tenant owner. The owner
    // invariant is independently enforced and must not be weakened by this test.
    let delegated = create_test_user(&f.db, tenant_id, "response-control-delegated", &f.run).await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
        [tenant_id.into(), delegated.id.into()],
    ))
    .await
    .unwrap();
    let selected = scoped_jwt(&f.state, &delegated).await;
    for role in ["member", "admin"] {
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET tenant_role=$3 WHERE tenant_id=$1 AND user_id=$2",
            [tenant_id.into(), delegated.id.into(), role.into()],
        ))
        .await
        .unwrap();
    }
    let stale_membership = http(
        f.app.clone(),
        Method::GET,
        &format!("{base}/count?mode=passthrough&owner_user_id={owner_id}"),
        Some(&selected),
        None,
    )
    .await;
    assert!(
        matches!(
            stale_membership.status,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::CONFLICT
        ),
        "{}",
        stale_membership.body
    );

    f.finish().await;
}

async fn token_for(
    f: &Fixture,
    state: &AppState,
    user: Uuid,
    tenant: Option<Uuid>,
    ttl: i64,
) -> String {
    let user = keycompute_db::User::find_by_id(&f.db, user)
        .await
        .unwrap()
        .unwrap();
    let versions = if let Some(t) = tenant {
        let member = keycompute_db::TenantMembership::find(&f.db, t, user.id)
            .await
            .unwrap()
            .unwrap();
        let tenant = keycompute_db::Tenant::find_by_id(&f.db, t)
            .await
            .unwrap()
            .unwrap();
        (Some(tenant.authz_version), Some(member.authz_version))
    } else {
        (None, None)
    };
    state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(
            user.id,
            tenant,
            user.token_version,
            versions.0,
            versions.1,
            ttl,
        )
        .unwrap()
}
async fn delegated_admin(f: &Fixture, name: &str) -> TenantActor {
    let actor = create_test_user(&f.db, f.user.tenant_id, name, &f.run).await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
        [actor.tenant_id.into(), actor.id.into()],
    ))
    .await
    .unwrap();
    actor
}
async fn revision(f: &Fixture, kind: &str, id: &str) -> i64 {
    let table = match kind {
        "response" => "scoped_responses",
        "conversation" => "scoped_conversations",
        _ => panic!("fixture table"),
    };
    f.db.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!("SELECT revision FROM {table} WHERE tenant_id=$1 AND user_id=$2 AND id=$3"),
        [f.user.tenant_id.into(), f.user.id.into(), id.into()],
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "revision")
    .unwrap()
}
fn control_path(f: &Fixture, kind: &str, mode: &str, id: &str) -> String {
    format!(
        "/api/v1/tenants/{}/{kind}/{mode}/{}/{id}",
        f.user.tenant_id, f.user.id
    )
}
async fn stored_response(f: &Fixture, mode: &str) -> String {
    let mut body = f.body(Op::Responses);
    body["store"] = true.into();
    let worker = (mode == "node_dispatch").then(|| f.worker(200));
    let url = if mode == "node_dispatch" {
        "/nt/v1/responses"
    } else {
        "/pt/v1/responses"
    };
    let created = expect(
        f.request(Method::POST, url, Some(body)).await,
        StatusCode::OK,
    );
    if let Some(worker) = worker {
        worker.await.unwrap();
    }
    created["id"].as_str().unwrap().into()
}

#[tokio::test]
async fn local_control_covers_both_families_and_preserves_personal_resource_boundaries() {
    let mut f = Fixture::new().await;
    let admin = tenant_owner_console_token(&f).await;
    let member = scoped_jwt(&f.state, &f.user).await;
    let root = create_test_user(&f.db, f.user.tenant_id, "local-resource-root", &f.run).await;
    let operator =
        create_test_user(&f.db, f.user.tenant_id, "local-resource-operator", &f.run).await;
    for (id, role) in [(root.id, "root"), (operator.id, "operator")] {
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role=$2 WHERE id=$1",
            [id.into(), role.into()],
        ))
        .await
        .unwrap();
    }
    let root_token = token_for(&f, &f.state, root.id, None, 3600).await;
    let op_token = token_for(&f, &f.state, operator.id, None, 3600).await;
    for (mode, prefix) in [("passthrough", "/pt/v1"), ("node_dispatch", "/nt/v1")] {
        let id = stored_response(&f, mode).await;
        let path = control_path(&f, "responses", mode, &id);
        let rev = revision(&f, "response", &id).await;
        let detail = http(f.app.clone(), Method::GET, &path, Some(&admin), None).await;
        assert_eq!(detail.status, StatusCode::OK, "{}", detail.body);
        assert_eq!(
            detail.body["summary"]["owner_user_id"],
            f.user.id.to_string()
        );
        assert_eq!(detail.headers["cache-control"], "private, no-store");
        for token in [&member, &f.key, &op_token] {
            let denied = http(
                f.app.clone(),
                Method::DELETE,
                &path,
                Some(token),
                Some(json!({"expected_revision":rev})),
            )
            .await;
            assert!(
                matches!(
                    denied.status,
                    StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
                ),
                "{}",
                denied.body
            );
        }
        let root_path = path.replacen("/api/v1/tenants/", "/api/v1/platform/tenants/", 1);
        expect(
            http(
                f.app.clone(),
                Method::GET,
                &root_path,
                Some(&root_token),
                None,
            )
            .await,
            StatusCode::BAD_REQUEST,
        );
        expect(
            http(
                f.app.clone(),
                Method::GET,
                &format!("{root_path}?reason=incident"),
                Some(&op_token),
                None,
            )
            .await,
            StatusCode::FORBIDDEN,
        );
        expect(
            http(
                f.app.clone(),
                Method::GET,
                &format!("{root_path}?reason=incident"),
                Some(&root_token),
                None,
            )
            .await,
            StatusCode::OK,
        );
        expect(
            http(
                f.app.clone(),
                Method::GET,
                &format!("{prefix}/responses/{id}"),
                Some(&admin),
                None,
            )
            .await,
            StatusCode::NOT_FOUND,
        );
        expect(
            http(
                f.app.clone(),
                Method::GET,
                &path.replace(&format!("/{mode}/"), "/invalid/"),
                Some(&admin),
                None,
            )
            .await,
            StatusCode::BAD_REQUEST,
        );
        expect(
            http(
                f.app.clone(),
                Method::GET,
                &path.replace(&f.user.id.to_string(), &root.id.to_string()),
                Some(&admin),
                None,
            )
            .await,
            StatusCode::NOT_FOUND,
        );
        let conv=expect(f.request(Method::POST,&format!("{prefix}/conversations"),Some(json!({"metadata":{"case":"original"},"items":[{"role":"user","content":"initial private text"}]}))).await,StatusCode::OK);
        let cid = conv["id"].as_str().unwrap();
        let cp = control_path(&f, "conversations", mode, cid);
        let first = revision(&f, "conversation", cid).await;
        expect(http(f.app.clone(),Method::PATCH,&cp,Some(&admin),Some(json!({"expected_revision":first,"metadata":{"case":"reviewed"},"owner_user_id":root.id}))).await,StatusCode::UNPROCESSABLE_ENTITY);
        expect(
            http(
                f.app.clone(),
                Method::PATCH,
                &cp,
                Some(&admin),
                Some(json!({"expected_revision":first,"metadata":{"case":"reviewed"}})),
            )
            .await,
            StatusCode::OK,
        );
        expect(
            http(
                f.app.clone(),
                Method::PATCH,
                &cp,
                Some(&admin),
                Some(json!({"expected_revision":first,"metadata":{"case":"stale"}})),
            )
            .await,
            StatusCode::CONFLICT,
        );
        let next = revision(&f, "conversation", cid).await;
        expect(http(f.app.clone(),Method::POST,&format!("{cp}/items"),Some(&admin),Some(json!({"expected_revision":next,"items":[{"role":"assistant","content":"reviewed item"}]}))).await,StatusCode::OK);
        let items = expect(
            http(
                f.app.clone(),
                Method::GET,
                &format!("{cp}/items?order=asc"),
                Some(&admin),
                None,
            )
            .await,
            StatusCode::OK,
        );
        assert_eq!(items["data"].as_array().unwrap().len(), 2);
        let item = items["data"][1]["id"].as_str().unwrap();
        let next = revision(&f, "conversation", cid).await;
        expect(
            http(
                f.app.clone(),
                Method::DELETE,
                &format!("{cp}/items/{item}"),
                Some(&admin),
                Some(json!({"expected_revision":next})),
            )
            .await,
            StatusCode::OK,
        );
        let items = expect(
            f.request(
                Method::GET,
                &format!("{prefix}/conversations/{cid}/items"),
                None,
            )
            .await,
            StatusCode::OK,
        );
        assert_eq!(items["data"].as_array().unwrap().len(), 1);
        let next = revision(&f, "conversation", cid).await;
        expect(
            http(
                f.app.clone(),
                Method::DELETE,
                &cp,
                Some(&admin),
                Some(json!({"expected_revision":next})),
            )
            .await,
            StatusCode::OK,
        );
        expect(
            f.request(Method::GET, &format!("{prefix}/conversations/{cid}"), None)
                .await,
            StatusCode::NOT_FOUND,
        );
    }
    let events=f.db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT metadata FROM tenant_audit_events WHERE tenant_id=$1 OR metadata->>'tenant_id'=$2",
        [f.user.tenant_id.into(),f.user.tenant_id.to_string().into()])).await.unwrap();
    for event in events {
        let text = event.try_get::<Value>("", "metadata").unwrap().to_string();
        assert!(!text.contains("initial private text") && !text.contains("reviewed item"));
    }
    f.finish().await;
}

async fn single_connection_state() -> (AppState, DatabaseConnection, i32) {
    let mut options = ConnectOptions::new(integration_tests::common::resolve_database_url());
    options.max_connections(1).min_connections(1);
    let db = Database::connect(options).await.unwrap();
    let pid = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT pg_backend_pid() AS pid",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i32>("", "pid")
        .unwrap();
    let state =
        AppState::try_with_pool_and_config(DbRouter::single(db.clone()), AppStateConfig::default())
            .await
            .unwrap();
    (state, db, pid)
}
async fn wait_for_owner_lock(f: &Fixture, pid: i32) {
    tokio::time::timeout(Duration::from_millis(800),async {
        loop {
            let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT EXISTS(SELECT 1 FROM pg_locks WHERE pid=$1 AND locktype='advisory' AND NOT granted) AS waiting",
                [pid.into()])).await.unwrap().unwrap();
            if row.try_get::<bool>("","waiting").unwrap(){break;}
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("the exact request connection never reached its owner advisory lock");
}
async fn queued_authority_case(change: &str, root: bool) {
    let mut f = Fixture::new().await;
    let id = stored_response(&f, "passthrough").await;
    let rev = revision(&f, "response", &id).await;
    let (state, connection, pid) = single_connection_state().await;
    let actor = if root {
        let origin = create_test_tenant(&f.db, "response-root-origin", &f.run).await;
        let actor = create_test_user(&f.db, origin.id, "response-queued-root", &f.run).await;
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='root' WHERE id=$1",
            [actor.id.into()],
        ))
        .await
        .unwrap();
        actor
    } else {
        delegated_admin(&f, "response-queued-admin").await
    };
    let origin = if root && change == "root_global" {
        None
    } else {
        Some(actor.tenant_id)
    };
    if change == "expiry" {
        // A lower bound alone also accepted 999ms, leaving just 1ms for
        // authentication. Enter a bounded EARLY window so a one-second JWT
        // reaches the real owner lock before expiring inside its 1s timeout.
        while !(200..=300).contains(&Utc::now().timestamp_subsec_millis()) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    let token = token_for(
        &f,
        &state,
        actor.id,
        origin,
        if change == "expiry" { 1 } else { 3600 },
    )
    .await;
    let expires = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .validate_claims(&token)
        .unwrap()
        .exp;
    let blocker = f.db.begin().await.unwrap();
    blocker
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended($1,7201))",
            [format!("{}:{}:passthrough", f.user.tenant_id, f.user.id).into()],
        ))
        .await
        .unwrap();
    let mut path = control_path(&f, "responses", "passthrough", &id);
    if root {
        path = path.replacen("/api/v1/tenants/", "/api/v1/platform/tenants/", 1);
    }
    let app = create_router(state.clone());
    let pending = tokio::spawn(async move {
        http(
            app,
            Method::DELETE,
            &path,
            Some(&token),
            Some(json!({"expected_revision":rev,"reason":"lock-wait regression"})),
        )
        .await
    });
    wait_for_owner_lock(&f, pid).await;
    match change {
        "token" | "root_global" => {
            f.db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE users SET token_version=token_version+1 WHERE id=$1",
                [actor.id.into()],
            ))
            .await
            .unwrap();
        }
        "member" => {
            f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "UPDATE tenant_memberships SET status='suspended' WHERE tenant_id=$1 AND user_id=$2",
                [actor.tenant_id.into(),actor.id.into()])).await.unwrap();
        }
        "tenant" | "origin" => {
            f.db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE tenants SET authz_version=authz_version+1 WHERE id=$1",
                [actor.tenant_id.into()],
            ))
            .await
            .unwrap();
        }
        "regrant" => {
            for role in ["member", "admin"] {
                f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                    "UPDATE tenant_memberships SET tenant_role=$3 WHERE tenant_id=$1 AND user_id=$2",
                    [actor.tenant_id.into(),actor.id.into(),role.into()])).await.unwrap();
            }
        }
        "expiry" => {
            while Utc::now().timestamp() < expires {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        _ => panic!("unknown fixture change"),
    }
    blocker.rollback().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            result.status,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ),
        "{change} root={root}: {} {}",
        result.status,
        result.body
    );
    assert_eq!(
        revision(&f, "response", &id).await,
        rev,
        "denied control changed the resource"
    );
    let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::bigint AS n FROM tenant_audit_events WHERE actor_user_id=$1 AND action='response.delete'",
        [actor.id.into()])).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i64>("", "n").unwrap(), 0);
    drop(state);
    connection.close().await.unwrap();
    f.finish().await;
}
#[tokio::test]
async fn waiting_controls_reject_changed_or_regranted_memberships() {
    for change in ["token", "tenant", "member", "regrant"] {
        queued_authority_case(change, false).await;
    }
}
#[tokio::test]
async fn waiting_root_controls_revalidate_global_and_selected_origin_authority() {
    for change in ["root_global", "origin"] {
        queued_authority_case(change, true).await;
    }
}
#[tokio::test]
async fn queued_control_credentials_expire_before_resource_mutation() {
    queued_authority_case("expiry", false).await;
    queued_authority_case("expiry", true).await;
}

#[tokio::test]
async fn local_control_audit_failure_rolls_back_content_and_prevents_unaudited_reads() {
    let mut f = Fixture::new().await;
    let admin = tenant_owner_console_token(&f).await;
    let id = stored_response(&f, "passthrough").await;
    let rev = revision(&f, "response", &id).await;
    let path = control_path(&f, "responses", "passthrough", &id);
    let conversation = expect(f.request(Method::POST, "/pt/v1/conversations",
        Some(json!({"metadata":{"case":"before"},"items":[{"role":"user","content":"retained content"}]}))).await, StatusCode::OK);
    let cid = conversation["id"].as_str().unwrap();
    let cp = control_path(&f, "conversations", "passthrough", cid);
    let cr = revision(&f, "conversation", cid).await;
    let before = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT status,revision,deleted_at,response_json,owner_id,request_id FROM scoped_responses WHERE tenant_id=$1 AND user_id=$2 AND id=$3",
        [f.user.tenant_id.into(),f.user.id.into(),id.as_str().into()])).await.unwrap().unwrap();
    let before_body: Value = before.try_get("", "response_json").unwrap();
    let function = format!("audit_response_{}", Uuid::new_v4().simple());
    let trigger = format!("tr_{function}");
    let tx = f.db.begin().await.unwrap();
    // DDL follows the production fence order, avoiding the historical
    // audit-table/identity-fence inversion in parallel fault injection.
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    tx.execute_unprepared(&format!(
        "CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant_id='{}'::uuid AND NEW.action IN ('response.delete','response.detail','conversation.mutate') THEN RAISE EXCEPTION 'isolated response audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER {trigger} BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION {function}();",
        f.user.tenant_id)).await.unwrap();
    tx.commit().await.unwrap();
    let denied_read = http(f.app.clone(), Method::GET, &path, Some(&admin), None).await;
    let denied_delete = http(
        f.app.clone(),
        Method::DELETE,
        &path,
        Some(&admin),
        Some(json!({"expected_revision":rev})),
    )
    .await;
    let denied_edit = http(
        f.app.clone(),
        Method::PATCH,
        &cp,
        Some(&admin),
        Some(json!({"expected_revision":cr,"metadata":{"case":"must roll back"}})),
    )
    .await;
    let after = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT status,revision,deleted_at,response_json,owner_id,request_id FROM scoped_responses WHERE tenant_id=$1 AND user_id=$2 AND id=$3",
        [f.user.tenant_id.into(),f.user.id.into(),id.as_str().into()])).await.unwrap().unwrap();
    let saved_conversation = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT revision,metadata_json FROM scoped_conversations WHERE tenant_id=$1 AND user_id=$2 AND id=$3",
        [f.user.tenant_id.into(),f.user.id.into(),cid.into()])).await.unwrap().unwrap();
    let tx = f.db.begin().await.unwrap();
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    tx.execute_unprepared(&format!(
        "DROP TRIGGER {trigger} ON tenant_audit_events; DROP FUNCTION {function}();"
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    for denied in [denied_read, denied_delete, denied_edit] {
        assert!(
            denied.status.is_server_error(),
            "{} {}",
            denied.status,
            denied.body
        );
        assert!(!denied.body.to_string().contains("retained content"));
    }
    assert_eq!(after.try_get::<i64>("", "revision").unwrap(), rev);
    assert!(
        after
            .try_get::<Option<chrono::DateTime<Utc>>>("", "deleted_at")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        after.try_get::<Value>("", "response_json").unwrap(),
        before_body
    );
    assert_eq!(
        after.try_get::<Uuid>("", "owner_id").unwrap(),
        before.try_get::<Uuid>("", "owner_id").unwrap()
    );
    assert_eq!(
        after.try_get::<Uuid>("", "request_id").unwrap(),
        before.try_get::<Uuid>("", "request_id").unwrap()
    );
    assert_eq!(
        saved_conversation.try_get::<i64>("", "revision").unwrap(),
        cr
    );
    assert_eq!(
        saved_conversation
            .try_get::<Value>("", "metadata_json")
            .unwrap(),
        json!({"case":"before"})
    );
    expect(
        http(
            f.app.clone(),
            Method::DELETE,
            &path,
            Some(&admin),
            Some(json!({"expected_revision":rev})),
        )
        .await,
        StatusCode::OK,
    );
    expect(
        http(
            f.app.clone(),
            Method::PATCH,
            &cp,
            Some(&admin),
            Some(json!({"expected_revision":cr,"metadata":{"case":"after"}})),
        )
        .await,
        StatusCode::OK,
    );
    f.finish().await;
}

#[tokio::test]
async fn local_resource_request_and_execution_family_cannot_be_reassigned() {
    let mut f = Fixture::new().await;
    let id = stored_response(&f, "passthrough").await;
    let conversation = expect(
        f.request(Method::POST, "/pt/v1/conversations", Some(json!({})))
            .await,
        StatusCode::OK,
    );
    for statement in [
        Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE scoped_responses SET request_id=$2 WHERE tenant_id=$1 AND id=$3",
            [
                f.user.tenant_id.into(),
                Uuid::new_v4().into(),
                id.as_str().into(),
            ],
        ),
        Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE scoped_responses SET access_mode='node_dispatch' WHERE tenant_id=$1 AND id=$2",
            [f.user.tenant_id.into(), id.as_str().into()],
        ),
        Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE scoped_conversations SET access_mode='node_dispatch' WHERE tenant_id=$1 AND id=$2",
            [
                f.user.tenant_id.into(),
                conversation["id"].as_str().unwrap().into(),
            ],
        ),
    ] {
        let error =
            f.db.execute(statement)
                .await
                .expect_err("resource identity transfer must fail");
        assert!(
            error.to_string().contains("identity is immutable"),
            "{error}"
        );
    }
    f.finish().await;
}

/// Every case uses its own database: an intentional audit delay must not hold
/// the shared identity fence and interfere with unrelated parallel tests.
#[tokio::test]
async fn control_reads_reject_post_audit_expiry_without_releasing_private_content() {
    post_audit_expiry_case(false).await;
}

#[tokio::test]
async fn root_control_reads_reject_post_audit_expiry_without_releasing_private_content() {
    post_audit_expiry_case(true).await;
}

async fn post_audit_expiry_case(platform: bool) {
    assert!(
        std::env::var("KC_TENANT_TEST_ACK_ISOLATED").as_deref() == Ok("1")
            || std::env::var_os("CI").is_some()
    );
    let url = integration_tests::common::resolve_database_url();
    let endpoint = url::Url::parse(&url).expect("valid isolated database URL");
    assert!(matches!(
        endpoint.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    ));
    let parent = create_test_pool().await;
    let name = format!("kc_response_expiry_{}", Uuid::new_v4().simple());
    parent
        .execute_unprepared(&format!("CREATE DATABASE {name}"))
        .await
        .unwrap();
    let db = Database::connect(format!("{}/{}", url.rsplit_once('/').unwrap().0, name))
        .await
        .unwrap();
    let owned = db.clone();
    let outcome = tokio::spawn(async move {
        keycompute_db::initialize_schema(&owned).await.unwrap();
        let bootstrap = owned.begin().await.unwrap();
        let root = keycompute_db::User::bootstrap_root(&bootstrap, "response-expiry-root@fixture.invalid", None)
            .await.unwrap();
        bootstrap.commit().await.unwrap();
        let mut f = Fixture::with_pool(owned, false).await;
        let response = stored_response(&f, "passthrough").await;
        let conversation = expect(f.request(Method::POST, "/pt/v1/conversations",
            Some(json!({"metadata":{"private":"post-audit-expiry-marker"},"items":[{"role":"user","content":"private conversation"}]}))).await, StatusCode::OK);
        let cid = conversation["id"].as_str().unwrap().to_owned();
        let tenant = keycompute_db::Tenant::find_by_id(&f.db, f.user.tenant_id).await.unwrap().unwrap();
        let actor = keycompute_db::User::find_by_id(&f.db, tenant.owner_user_id).await.unwrap().unwrap();
        let member = keycompute_db::TenantMembership::find(&f.db,tenant.id,actor.id).await.unwrap().unwrap();
        // nextval is intentionally nontransactional: it proves the audit trigger
        // was actually reached even when the audit INSERT is later rolled back.
        f.db.execute_unprepared(&format!(r#"
            CREATE SEQUENCE response_expiry_marker;
            CREATE SEQUENCE response_expiry_entered_ms;
            CREATE TABLE response_expiry_deadline (expires_at BIGINT NOT NULL, action TEXT NOT NULL);
            INSERT INTO response_expiry_deadline VALUES(0,'none');
            CREATE FUNCTION response_expiry_delay() RETURNS trigger LANGUAGE plpgsql AS $$
            DECLARE deadline BIGINT; target_action TEXT;
            BEGIN
              SELECT expires_at,action INTO deadline,target_action FROM response_expiry_deadline;
              IF (NEW.tenant_id='{0}'::uuid OR NEW.metadata->>'tenant_id'='{0}') AND NEW.action=target_action THEN
                PERFORM nextval('response_expiry_marker');
                PERFORM setval('response_expiry_entered_ms',floor(EXTRACT(EPOCH FROM clock_timestamp())*1000)::BIGINT,TRUE);
                PERFORM pg_sleep(GREATEST(0::double precision,deadline::double precision-EXTRACT(EPOCH FROM clock_timestamp())::double precision)+0.05);
              END IF;
              RETURN NEW;
            END $$;
            CREATE TRIGGER response_expiry_delay BEFORE INSERT ON tenant_audit_events
                FOR EACH ROW EXECUTE FUNCTION response_expiry_delay();
        "#, tenant.id)).await.unwrap();
        let before_calls = f.upstream.calls.lock().unwrap().len();
        let response_revision = revision(&f,"response",&response).await;
        let conversation_revision = revision(&f,"conversation",&cid).await;
        let base = format!("/api/v1/tenants/{}", tenant.id);
        let paths = [
            (control_path(&f,"responses","passthrough",&response), "response.detail"),
            (format!("{base}/responses/count?mode=passthrough"), "response.count"),
            (control_path(&f,"conversations","passthrough",&cid), "conversation.detail"),
            (format!("{base}/conversations/count?mode=passthrough"), "conversation.count"),
            (format!("{base}/responses?mode=passthrough"), "response.count"),
            (format!("{base}/conversations?mode=passthrough"), "conversation.count"),
        ];
        for (case_index, (path, action)) in paths.into_iter().enumerate() {
            let path = if platform {
                let separator = if path.contains('?') { '&' } else { '?' };
                format!("{}{separator}reason=post-audit-expiry-regression",
                    path.replacen("/api/v1/tenants/", "/api/v1/platform/tenants/", 1))
            } else { path };
            f.db.execute_unprepared("SELECT setval('response_expiry_marker',1,FALSE)").await.unwrap();
            // Authenticate with a scheduling budget, then approach the signed
            // deadline before beginning the bounded (2500ms) audit statement.
            // The test uses the same verified context cache as console admission;
            // it neither invents roles nor changes the signed expiration.
            let validator=f.state.auth.get_jwt_validator().unwrap();
            let token=validator.generate_identity_token(
                if platform { root.id } else { actor.id },
                if platform { None } else { Some(tenant.id) },
                if platform { root.token_version } else { actor.token_version },
                if platform { None } else { Some(tenant.authz_version) },
                if platform { None } else { Some(member.authz_version) },6).unwrap();
            let expiry=validator.validate_claims(&token).unwrap().exp;
            f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "UPDATE response_expiry_deadline SET expires_at=$1,action=$2",[expiry.into(),action.into()])).await.unwrap();
            // Separate actual authentication latency from the short audit wait.
            // Only this isolated test router adds a scheduler barrier. The complete
            // production router and its transaction/session checks remain in use.
            let service=f.state.auth.clone();
            let app=f.app.clone().layer(axum::middleware::from_fn(
                move |mut request: Request<Body>, next: axum::middleware::Next| {
                    let service=service.clone();
                    async move {
                        let raw=request.headers()["authorization"].to_str().unwrap()
                            .strip_prefix("Bearer ").unwrap().to_owned();
                        let verified=service.verify_token(&raw).await.expect("authenticate the actual signed test JWT");
                        assert_eq!(verified.credential_expires_at,Some(expiry));
                        request.extensions_mut().insert(verified);
                        if case_index==0 {
                            tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
                        }
                        let remaining=expiry*1000-chrono::Utc::now().timestamp_millis();
                        if remaining>1500 {
                            tokio::time::sleep(std::time::Duration::from_millis((remaining-1500) as u64)).await;
                        }
                        assert!(chrono::Utc::now().timestamp()<expiry,"test scheduler exhausted the signed admission budget");
                        next.run(request).await
                    }
                }
            ));
            let denied=http(app,Method::GET,&path,Some(&token),None).await;
            let marker=f.db.query_one(Statement::from_string(DbBackend::Postgres,
                "SELECT is_called,last_value FROM response_expiry_marker")).await.unwrap().unwrap();
            assert!(marker.try_get::<bool>("","is_called").unwrap(),"request must reach delayed audit, not fail before authentication: {path}");
            assert_eq!(marker.try_get::<i64>("","last_value").unwrap(),1,"one delayed audit only");
            let entered=f.db.query_one(Statement::from_string(DbBackend::Postgres,
                "SELECT last_value FROM response_expiry_entered_ms")).await.unwrap().unwrap()
                .try_get::<i64>("","last_value").unwrap();
            assert!(entered < expiry*1000, "audit must begin while the signed JWT remains valid: {path}");
            assert!(chrono::Utc::now().timestamp() >= expiry, "rejection must occur after the signed deadline");
            assert_eq!(denied.status,StatusCode::UNAUTHORIZED,"{path}: {}",denied.body);
            assert!(!denied.body.to_string().contains("post-audit-expiry-marker"));
            let request_id:Uuid=denied.headers["x-request-id"].to_str().unwrap().parse().unwrap();
            let audit=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT COUNT(*)::BIGINT n FROM tenant_audit_events WHERE request_id=$1 AND action=$2",
                [request_id.into(),action.into()])).await.unwrap().unwrap();
            assert_eq!(audit.try_get::<i64>("","n").unwrap(),0,"delayed audit must roll back");
        }
        assert_eq!(f.upstream.calls.lock().unwrap().len(),before_calls,"reads do not start more inference");
        assert_eq!(revision(&f,"response",&response).await,response_revision);
        assert_eq!(revision(&f,"conversation",&cid).await,conversation_revision);
        f.db.execute_unprepared("DROP TRIGGER response_expiry_delay ON tenant_audit_events; DROP FUNCTION response_expiry_delay(); DROP TABLE response_expiry_deadline; DROP SEQUENCE response_expiry_marker; DROP SEQUENCE response_expiry_entered_ms;").await.unwrap();
        f.finish().await;
    }).await;
    db.close().await.unwrap();
    parent
        .execute_unprepared(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .await
        .unwrap();
    outcome.unwrap();
}

#[tokio::test]
async fn real_resource_control_client_preserves_owner_and_item_cursor_contracts() {
    use client_api::{
        ApiClient, ClientConfig, ClientError,
        api::response_control::{
            AppendItemsCommand, ItemOrder, ItemQuery, MetadataCommand, ResourceAddress,
            ResourceListQuery, ResponseControlApi, ResponseMode, RevisionCommand,
        },
    };
    struct HttpServer(tokio::task::JoinHandle<()>);
    impl Drop for HttpServer {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let mut f = Fixture::new().await;
    let admin = tenant_owner_console_token(&f).await;
    let member = scoped_jwt(&f.state, &f.user).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = f.app.clone();
    let server = HttpServer(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    let client =
        ApiClient::new(ClientConfig::new(format!("http://{address}")).with_no_proxy(true)).unwrap();
    let api = ResponseControlApi::tenant(&client, f.user.tenant_id).unwrap();
    for (mode, prefix) in [
        (ResponseMode::Passthrough, "/pt/v1"),
        (ResponseMode::NodeDispatch, "/nt/v1"),
    ] {
        let response_id = stored_response(&f, mode.as_str()).await;
        let query = ResourceListQuery {
            mode: Some(mode),
            owner_user_id: Some(f.user.id),
            page: Some(1),
            page_size: Some(20),
            reason: None,
        };
        for denied in [&member, &f.key] {
            assert!(matches!(
                api.responses(&query, denied).await,
                Err(ClientError::Forbidden(_) | ClientError::Unauthorized(_))
            ));
        }
        let list = api.responses(&query, &admin).await.unwrap();
        assert!(list.items.iter().any(|r| r.id == response_id));
        assert_eq!(
            api.response_count(&query, &admin).await.unwrap().total,
            list.total
        );
        let detail = api
            .response(mode, f.user.id, &response_id, None, &admin)
            .await
            .unwrap();
        assert_eq!(detail.summary.owner_user_id, f.user.id);
        assert_eq!(detail.summary.tenant_id, f.user.tenant_id);
        assert!(
            api.response(mode, Uuid::new_v4(), &response_id, None, &admin)
                .await
                .is_err()
        );
        let input = api
            .response_input_items_page(
                &ResourceAddress {
                    mode,
                    owner: f.user.id,
                    id: response_id.clone(),
                },
                &ItemQuery {
                    limit: 1,
                    order: ItemOrder::Asc,
                    ..Default::default()
                },
                None,
                &admin,
            )
            .await
            .unwrap();
        assert_eq!(input.data.len(), 1);
        let created=expect(f.request(Method::POST,&format!("{prefix}/conversations"),Some(json!({"metadata":{"case":"before"},"items":[{"role":"user","content":"first fixture item"},{"role":"user","content":"second fixture item"}]}))).await,StatusCode::OK);
        let conversation_id = created["id"].as_str().unwrap().to_owned();
        let address = ResourceAddress {
            mode,
            owner: f.user.id,
            id: conversation_id.clone(),
        };
        let listed = api.conversations(&query, &admin).await.unwrap();
        assert!(listed.items.iter().any(|r| r.id == conversation_id));
        assert_eq!(
            api.conversation_count(&query, &admin).await.unwrap().total,
            listed.total
        );
        let original = api
            .conversation(mode, f.user.id, &conversation_id, None, &admin)
            .await
            .unwrap();
        let rev = original.summary.revision.unwrap();
        let first = api
            .conversation_items_page(
                &address,
                &ItemQuery {
                    limit: 1,
                    order: ItemOrder::Asc,
                    ..Default::default()
                },
                None,
                &admin,
            )
            .await
            .unwrap();
        assert!(first.has_more);
        assert_eq!(first.data[0]["content"], "first fixture item");
        let second = api
            .conversation_items_page(
                &address,
                &ItemQuery {
                    after: first.last_id.clone(),
                    limit: 1,
                    order: ItemOrder::Asc,
                },
                None,
                &admin,
            )
            .await
            .unwrap();
        assert!(!second.has_more);
        assert_eq!(second.data[0]["content"], "second fixture item");
        let changed = api
            .update_conversation(
                mode,
                f.user.id,
                &conversation_id,
                &MetadataCommand {
                    expected_revision: rev,
                    metadata: json!({"case":"after"}),
                    reason: None,
                },
                &admin,
            )
            .await
            .unwrap();
        assert_eq!(changed["metadata"]["case"], "after");
        assert!(
            api.update_conversation(
                mode,
                f.user.id,
                &conversation_id,
                &MetadataCommand {
                    expected_revision: rev,
                    metadata: json!({"case":"stale"}),
                    reason: None
                },
                &admin
            )
            .await
            .is_err()
        );
        let changed = api
            .conversation(mode, f.user.id, &conversation_id, None, &admin)
            .await
            .unwrap();
        api.append_conversation_items(
            mode,
            f.user.id,
            &conversation_id,
            &AppendItemsCommand {
                expected_revision: changed.summary.revision.unwrap(),
                items: vec![json!({"role":"assistant","content":"third fixture item"})],
                reason: None,
            },
            &admin,
        )
        .await
        .unwrap();
        let changed = api
            .conversation(mode, f.user.id, &conversation_id, None, &admin)
            .await
            .unwrap();
        api.remove_conversation_item(
            mode,
            f.user.id,
            &conversation_id,
            first.first_id.as_deref().unwrap(),
            &RevisionCommand {
                expected_revision: changed.summary.revision.unwrap(),
                reason: None,
            },
            &admin,
        )
        .await
        .unwrap();
        let remaining = api
            .conversation_items_page(
                &address,
                &ItemQuery {
                    limit: 100,
                    order: ItemOrder::Asc,
                    ..Default::default()
                },
                None,
                &admin,
            )
            .await
            .unwrap();
        assert_eq!(remaining.data.len(), 2);
        assert_eq!(remaining.data[0]["content"], "second fixture item");
        let changed = api
            .conversation(mode, f.user.id, &conversation_id, None, &admin)
            .await
            .unwrap();
        assert!(
            api.delete_conversation(
                mode,
                f.user.id,
                &conversation_id,
                &RevisionCommand {
                    expected_revision: changed.summary.revision.unwrap(),
                    reason: None
                },
                &admin
            )
            .await
            .unwrap()
            .deleted
        );
        let calls = f.upstream.calls.lock().unwrap().len();
        assert!(
            api.delete_response(
                mode,
                f.user.id,
                &response_id,
                &RevisionCommand {
                    expected_revision: detail.summary.revision.unwrap(),
                    reason: None
                },
                &admin
            )
            .await
            .unwrap()
            .deleted
        );
        assert_eq!(
            f.upstream.calls.lock().unwrap().len(),
            calls,
            "resource deletion cannot invoke upstream inference"
        );
        let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT tenant_id,user_id,deleted_at FROM scoped_responses WHERE tenant_id=$1 AND user_id=$2 AND access_mode=$3 AND id=$4",[f.user.tenant_id.into(),f.user.id.into(),mode.as_str().into(),response_id.into()])).await.unwrap().unwrap();
        assert_eq!(
            row.try_get::<Uuid>("", "tenant_id").unwrap(),
            f.user.tenant_id
        );
        assert_eq!(row.try_get::<Uuid>("", "user_id").unwrap(), f.user.id);
        assert!(
            row.try_get::<Option<chrono::DateTime<Utc>>>("", "deleted_at")
                .unwrap()
                .is_some()
        );
    }
    drop(server);
    f.finish().await;
}
