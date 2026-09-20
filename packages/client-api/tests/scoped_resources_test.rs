//! Resource helpers preserve the selected URL family and bearer scope.
use client_api::{
    api::{
        admin::ModelAccessMode,
        openai::{OpenAiApi, ResourceListQuery, ResourceOrder},
    },
    client::OpenAiClient,
    config::ClientConfig,
};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};
async fn fixture() -> (OpenAiApi, MockServer) {
    let server = MockServer::start().await;
    let client = OpenAiClient::new(ClientConfig::new(server.uri()).with_no_proxy(true)).unwrap();
    (OpenAiApi::new(&client), server)
}
#[tokio::test]
async fn retrieval_and_pagination_are_mode_scoped_and_ids_are_encoded() {
    for (mode, base) in [
        (ModelAccessMode::AccountPool, "/v1"),
        (ModelAccessMode::Passthrough, "/pt/v1"),
        (ModelAccessMode::NodeDispatch, "/nt/v1"),
    ] {
        let (api, server) = fixture().await;
        let body = json!({"id":"resp:one","object":"response","output":[{"type":"function_call","arguments":"{\"x\":1}"}],"vendor":{"preserve":null}});
        Mock::given(method("GET"))
            .and(path(format!("{base}/responses/resp%3Aone")))
            .and(header("authorization", "Bearer scope-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            api.retrieve_response_in_mode(mode, "resp:one", "scope-key")
                .await
                .unwrap(),
            body
        );
        let page = json!({"object":"list","data":[],"has_more":false});
        Mock::given(method("GET"))
            .and(path(format!("{base}/responses/resp%3Aone/input_items")))
            .and(query_param("after", "item one"))
            .and(query_param("limit", "7"))
            .and(query_param("order", "asc"))
            .and(header("authorization", "Bearer scope-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&page))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            api.response_input_items_in_mode(
                mode,
                "resp:one",
                &ResourceListQuery {
                    after: Some("item one".into()),
                    limit: Some(7),
                    order: Some(ResourceOrder::Asc)
                },
                "scope-key"
            )
            .await
            .unwrap(),
            page
        );
    }
}

#[tokio::test]
async fn conversation_mutations_preserve_native_data_and_use_post_not_put() {
    let (api, server) = fixture().await;
    let mode = ModelAccessMode::NodeDispatch;
    let seed = json!({"metadata":{"tag":"test"},"items":[{"type":"message","role":"user","content":[{"type":"input_text","text":"Hello"}]}]});
    let conv = json!({"id":"conv-one","object":"conversation","metadata":{"tag":"test"}});
    Mock::given(method("POST"))
        .and(path("/nt/v1/conversations"))
        .and(body_json(&seed))
        .and(header("authorization", "Bearer key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&conv))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        api.create_conversation_in_mode(mode, &seed, "key")
            .await
            .unwrap(),
        conv
    );
    let metadata = json!({"metadata":{"tag":"changed"}});
    Mock::given(method("POST"))
        .and(path("/nt/v1/conversations/conv-one"))
        .and(body_json(&metadata))
        .respond_with(ResponseTemplate::new(200).set_body_json(&conv))
        .expect(1)
        .mount(&server)
        .await;
    api.update_conversation_in_mode(mode, "conv-one", &metadata, "key")
        .await
        .unwrap();
    let items = json!({"items":[{"type":"function_call_output","call_id":"call-x","output":{"unknown":[null,1]}}]});
    Mock::given(method("POST"))
        .and(path("/nt/v1/conversations/conv-one/items"))
        .and(body_json(&items))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"object":"list","data":[]})))
        .expect(1)
        .mount(&server)
        .await;
    api.append_conversation_items_in_mode(mode, "conv-one", &items, "key")
        .await
        .unwrap();
    Mock::given(method("DELETE"))
        .and(path("/nt/v1/conversations/conv-one/items/item%3Aone"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id":"item:one","deleted":true})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let deleted = api
        .delete_conversation_item_in_mode(mode, "conv-one", "item:one", "key")
        .await
        .unwrap();
    assert_eq!(deleted["deleted"], true);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    assert!(
        requests
            .iter()
            .all(|r| r.headers.get("authorization").unwrap() == "Bearer key")
    );
}

#[tokio::test]
async fn generation_and_cancellation_never_add_hidden_automatic_retries() {
    let (api, server) = fixture().await;
    Mock::given(method("POST"))
        .and(path("/pt/v1/responses"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_json(json!({"error":{"message":"uncertain execution"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert!(
        api.responses_in_mode(
            ModelAccessMode::Passthrough,
            &json!({"model":"m","input":"hello","background":true}),
            "key"
        )
        .await
        .is_err()
    );
    Mock::given(method("POST"))
        .and(path("/pt/v1/responses/resp-one/cancel"))
        .and(body_json(json!({})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id":"resp-one","status":"cancelled"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        api.cancel_response_in_mode(ModelAccessMode::Passthrough, "resp-one", "key")
            .await
            .unwrap()["status"],
        "cancelled"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}
