//! Typed managed-resource contracts use mock content and no real upstreams.
use client_api::{ApiClient, ClientConfig, api::response_control::*};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, method, path, query_param},
};
fn client(s: &MockServer) -> ApiClient {
    ApiClient::new(
        ClientConfig::new(s.uri())
            .with_no_proxy(true)
            .with_console_display_cache(true),
    )
    .unwrap()
}
fn summary(tenant: Uuid, owner: Uuid, id: &str) -> Value {
    json!({"id":id,"tenant_id":tenant,"owner_user_id":owner,"mode":"passthrough","provider":null,"account_id":null,"model":"fixture","status":"completed","background":false,"store_response":true,"stream":false,"previous_response_id":null,"conversation_id":null,"revision":7,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","expires_at":"2030-01-01T00:00:00Z","deleted":false,"local_content_available":true,"native_content_available":false})
}
fn query(owner: Uuid) -> ResourceListQuery {
    ResourceListQuery {
        mode: Some(ResponseMode::Passthrough),
        owner_user_id: Some(owner),
        page: Some(1),
        page_size: Some(20),
        reason: None,
    }
}
#[tokio::test]
async fn managed_lists_are_fresh_and_reject_foreign_owner_mode_or_missing_revision() {
    let s = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let api = ResponseControlApi::tenant(&client(&s), tenant).unwrap();
    let q = query(owner);
    let good = summary(tenant, owner, "r/opaque?item=1");
    let wrap =
        |row: Value| json!({"items":[row],"total":1,"page":1,"page_size":20,"total_pages":1});
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/tenants/{tenant}/responses")))
        .respond_with(ResponseTemplate::new(200).set_body_json(wrap(good.clone())))
        .expect(2)
        .mount(&s)
        .await;
    for _ in 0..2 {
        assert_eq!(
            api.responses(&q, "console").await.unwrap().items[0].owner_user_id,
            owner
        );
    }
    s.verify().await;
    for (field, value) in [
        ("tenant_id", json!(Uuid::new_v4())),
        ("owner_user_id", json!(Uuid::new_v4())),
        ("mode", json!("node_dispatch")),
        ("revision", Value::Null),
        ("revision", json!(0)),
    ] {
        s.reset().await;
        let mut row = good.clone();
        row[field] = value;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(wrap(row)))
            .expect(1)
            .mount(&s)
            .await;
        assert!(api.responses(&q, "console").await.is_err());
        s.verify().await;
    }
}
#[tokio::test]
async fn typed_item_pages_keep_cursor_inside_one_resource_path() {
    let s = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let api = ResponseControlApi::tenant(&client(&s), tenant).unwrap();
    let address = ResourceAddress {
        mode: ResponseMode::NodeDispatch,
        owner,
        id: "conv_fixture".into(),
    };
    let cursor = "item?tenant_id=other&order=desc";
    let item = json!({"id":"item-next","role":"user","content":"private-fixture-body"});
    Mock::given(method("GET")).and(path(format!("/api/v1/tenants/{tenant}/conversations/node_dispatch/{owner}/conv_fixture/items"))).and(query_param("after",cursor)).and(query_param("order","asc")).and(query_param("limit","1"))
  .respond_with(ResponseTemplate::new(200).set_body_json(json!({"object":"list","data":[item],"first_id":"item-next","last_id":"item-next","has_more":true}))).expect(1).mount(&s).await;
    let result = api
        .conversation_items_page(
            &address,
            &ItemQuery {
                after: Some(cursor.into()),
                limit: 1,
                order: ItemOrder::Asc,
            },
            None,
            "console",
        )
        .await
        .unwrap();
    assert_eq!(result.last_id.as_deref(), Some("item-next"));
    assert!(!format!("{result:?}").contains("private-fixture-body"));
    let reqs = s.received_requests().await.unwrap();
    assert!(reqs[0].url.query_pairs().all(|(k, _)| k != "tenant_id"));
    assert!(
        api.conversation_items_page(
            &address,
            &ItemQuery {
                limit: 101,
                ..Default::default()
            },
            None,
            "console"
        )
        .await
        .is_err()
    );
    assert_eq!(s.received_requests().await.unwrap().len(), 1);
}
#[tokio::test]
async fn malformed_content_results_do_not_become_a_resource_operation_success() {
    let s = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let api = ResponseControlApi::tenant(&client(&s), tenant).unwrap();
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"summary":summary(tenant,owner,"resp_fixture"),"response":{"id":"other-response","object":"response"}}))).expect(1).mount(&s).await;
    assert!(
        api.response(
            ResponseMode::Passthrough,
            owner,
            "resp_fixture",
            None,
            "console"
        )
        .await
        .is_err()
    );
    s.verify().await;
    s.reset().await;
    Mock::given(method("DELETE"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id":"wrong-resource","object":"response","deleted":true})),
        )
        .expect(1)
        .mount(&s)
        .await;
    assert!(
        api.delete_response(
            ResponseMode::Passthrough,
            owner,
            "resp_fixture",
            &RevisionCommand {
                expected_revision: 7,
                reason: None
            },
            "console"
        )
        .await
        .is_err()
    );
    s.verify().await;
    s.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"object":"list","data":[],"first_id":null,"last_id":null,"has_more":true}),
        ))
        .expect(1)
        .mount(&s)
        .await;
    assert!(
        api.response_input_items_page(
            &ResourceAddress {
                mode: ResponseMode::Passthrough,
                owner,
                id: "resp_fixture".into()
            },
            &ItemQuery::default(),
            None,
            "console"
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn revisions_reasons_and_uncertain_commands_do_not_invent_authority_or_replay() {
    let s = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let api = ResponseControlApi::tenant(&client(&s), tenant).unwrap();
    let bad = RevisionCommand {
        expected_revision: 0,
        reason: None,
    };
    assert!(
        api.delete_response(ResponseMode::Passthrough, owner, "r", &bad, "console")
            .await
            .is_err()
    );
    let root = ResponseControlApi::platform_tenant(&client(&s), tenant).unwrap();
    assert!(root.responses(&query(owner), "root").await.is_err());
    assert!(
        api.responses(
            &ResourceListQuery {
                page: Some(0),
                ..query(owner)
            },
            "console"
        )
        .await
        .is_err()
    );
    assert!(s.received_requests().await.unwrap().is_empty());
    for status in [401, 503] {
        s.reset().await;
        Mock::given(method("POST"))
            .and(body_json(json!({"expected_revision":7})))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_json(json!({"error":{"message":"uncertain resource change"}})),
            )
            .expect(1)
            .mount(&s)
            .await;
        assert!(
            api.cancel_response(
                ResponseMode::Passthrough,
                owner,
                "resp_fixture",
                &RevisionCommand {
                    expected_revision: 7,
                    reason: None
                },
                "console"
            )
            .await
            .is_err()
        );
        s.verify().await;
    }
}

#[tokio::test]
async fn equal_ids_from_different_owners_are_valid_but_duplicate_logical_rows_are_rejected() {
    let s = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let api = ResponseControlApi::tenant(&client(&s), tenant).unwrap();
    let q = ResourceListQuery {
        owner_user_id: None,
        ..query(a)
    };
    for collection in ["responses", "conversations"] {
        for (other, valid) in [(b, true), (a, false)] {
            s.reset().await;
            let row = |owner| {
                if collection == "responses" {
                    summary(tenant, owner, "shared-id")
                } else {
                    json!({
                        "id":"shared-id","tenant_id":tenant,"owner_user_id":owner,"mode":"passthrough","account_id":null,"model":null,"metadata":{},"active_response_id":null,"revision":1,"created_at":"now","updated_at":"now","expires_at":"later","deleted":false
                    })
                }
            };
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "items":[row(a),row(other)],"total":2,"page":1,"page_size":20,"total_pages":1
                })))
                .expect(1)
                .mount(&s)
                .await;
            let result = if collection == "responses" {
                api.responses(&q, "fixture").await.map(|p| p.items.len())
            } else {
                api.conversations(&q, "fixture")
                    .await
                    .map(|p| p.items.len())
            };
            assert_eq!(result.is_ok(), valid, "{collection}");
            s.verify().await;
        }
    }
}

#[tokio::test]
async fn repeated_item_cursors_and_duplicate_items_cannot_loop_the_inspector() {
    let s = MockServer::start().await;
    let api = ResponseControlApi::tenant(&client(&s), Uuid::new_v4()).unwrap();
    let address = ResourceAddress {
        mode: ResponseMode::Passthrough,
        owner: Uuid::new_v4(),
        id: "conversation".into(),
    };
    let q = ItemQuery {
        after: Some("previous".into()),
        ..Default::default()
    };
    for ids in [vec!["previous"], vec!["next", "next"], vec!["valid"]] {
        s.reset().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object":"list","data":ids.iter().map(|id|json!({"id":id,"content":"fixture"})).collect::<Vec<_>>(),
            "first_id":ids.first(),"last_id":ids.last(),"has_more":true
        }))).expect(1).mount(&s).await;
        assert_eq!(
            api.conversation_items_page(&address, &q, None, "fixture")
                .await
                .is_ok(),
            ids == vec!["valid"]
        );
        s.verify().await;
    }
}

#[tokio::test]
async fn local_details_require_the_actual_matching_local_response_envelope() {
    let s = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let api = ResponseControlApi::tenant(&client(&s), tenant).unwrap();
    for value in [
        json!({"summary":summary(tenant,owner,"r")}),
        json!({"summary":summary(tenant,owner,"r"),"response":{"id":"r","object":"conversation"}}),
        json!({"summary":summary(tenant,owner,"r"),"response":{"id":"r","object":"response"},"native_body":{"id":"r","object":"response"}}),
    ] {
        s.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(value))
            .expect(1)
            .mount(&s)
            .await;
        assert!(
            api.response(ResponseMode::Passthrough, owner, "r", None, "fixture")
                .await
                .is_err()
        );
        s.verify().await;
    }
}

#[tokio::test]
async fn conversation_details_reject_other_scopes_and_legacy_item_helpers_share_validation() {
    let s = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let api = ResponseControlApi::tenant(&client(&s), tenant).unwrap();
    let good = json!({"id":"c","tenant_id":tenant,"owner_user_id":owner,"mode":"passthrough",
        "account_id":null,"model":null,"metadata":{},"active_response_id":null,"revision":1,
        "created_at":"now","updated_at":"now","expires_at":"later","deleted":false});
    for (field, value) in [
        ("tenant_id", json!(Uuid::new_v4())),
        ("owner_user_id", json!(Uuid::new_v4())),
        ("id", json!("other")),
        ("mode", json!("node_dispatch")),
        ("revision", Value::Null),
    ] {
        s.reset().await;
        let mut row = good.clone();
        row[field] = value;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"summary":row,"conversation":{"id":"c","object":"conversation"}}),
            ))
            .expect(1)
            .mount(&s)
            .await;
        assert!(
            api.conversation(ResponseMode::Passthrough, owner, "c", None, "fixture")
                .await
                .is_err()
        );
        s.verify().await;
    }
    s.reset().await;
    Mock::given(method("GET"))
        .and(query_param("limit", "20"))
        .and(query_param("order", "desc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"object":"list","data":[],"first_id":null,"last_id":null,"has_more":true}),
        ))
        .expect(2)
        .mount(&s)
        .await;
    assert!(
        api.conversation_items(ResponseMode::Passthrough, owner, "c", None, "fixture")
            .await
            .is_err()
    );
    assert!(
        api.response_input_items(ResponseMode::Passthrough, owner, "r", None, "fixture")
            .await
            .is_err()
    );
    s.verify().await;
}
