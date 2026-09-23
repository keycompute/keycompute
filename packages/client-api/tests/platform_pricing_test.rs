//! Wire contracts for explicit platform-owned or target-tenant pricing.
use client_api::{AdminApi, ApiClient, ClientConfig, api::admin::*};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};
fn client(s: &MockServer) -> ApiClient {
    ApiClient::new(
        ClientConfig::new(s.uri())
            .with_no_proxy(true)
            .with_console_display_cache(true),
    )
    .unwrap()
}
fn row(target: PricingTarget, id: Uuid) -> Value {
    let (scope, tenant) = match target {
        PricingTarget::Platform => ("platform", None),
        PricingTarget::Tenant { tenant_id } => ("tenant", Some(tenant_id)),
    };
    json!({"id":id,"scope_type":scope,"tenant_id":tenant,"model_name":"scoped-price","billing_dimension":"provideraccount","currency":"CNY","input_price_per_1k":"1E-10","output_price_per_1k":"9999999999.9999999999","is_default":false,"is_effective":true,"effective_from":"2026-01-01T00:00:00Z","effective_until":null,"created_at":"2026-01-01T00:00:00Z","version":7})
}
fn page(row: Value) -> Value {
    json!({"pricing":[row],"total":1,"page":1,"page_size":20,"total_pages":1})
}
#[tokio::test]
async fn fresh_platform_and_tenant_reads_never_inherit_the_selected_tenant() {
    let s = MockServer::start().await;
    let api = AdminApi::new(&client(&s));
    let tid = Uuid::new_v4();
    let id = Uuid::new_v4();
    for target in [
        PricingTarget::Platform,
        PricingTarget::Tenant { tenant_id: tid },
    ] {
        s.reset().await;
        let m = Mock::given(method("GET"))
            .and(path("/api/v1/platform/pricing"))
            .and(query_param(
                "scope_type",
                if target == PricingTarget::Platform {
                    "platform"
                } else {
                    "tenant"
                },
            ))
            .and(query_param("search", "x&tenant_id=other%_*"))
            .and(header("authorization", "Bearer scoped-console"));
        m.respond_with(ResponseTemplate::new(200).set_body_json(page(row(target, id))))
            .expect(2)
            .mount(&s)
            .await;
        let params = PricingQueryParams::new(target).with_search("x&tenant_id=other%_*");
        for _ in 0..2 {
            let out = api
                .list_pricing_page(&params, "scoped-console")
                .await
                .unwrap();
            assert_eq!(out.pricing[0].target().unwrap(), target);
            assert_eq!(out.pricing[0].input_price_per_1k, "1E-10");
        }
        for r in s.received_requests().await.unwrap() {
            let tenants = r
                .url
                .query_pairs()
                .filter(|(k, _)| k == "tenant_id")
                .map(|(_, v)| v.into_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                tenants,
                if target == PricingTarget::Platform {
                    vec![]
                } else {
                    vec![tid.to_string()]
                }
            );
        }
        s.verify().await;
    }
}
#[tokio::test]
async fn missing_scope_versions_nil_and_foreign_rows_fail_closed() {
    let s = MockServer::start().await;
    let api = AdminApi::new(&client(&s));
    let id = Uuid::new_v4();
    let a = PricingTarget::Tenant {
        tenant_id: Uuid::new_v4(),
    };
    let b = PricingTarget::Tenant {
        tenant_id: Uuid::new_v4(),
    };
    let good = row(a, id);
    let mut missing = good.clone();
    missing.as_object_mut().unwrap().remove("version");
    let mut no_scope = good.clone();
    no_scope.as_object_mut().unwrap().remove("scope_type");
    let mut nil = good.clone();
    nil["tenant_id"] = json!(Uuid::nil());
    let mut zero = good.clone();
    zero["version"] = json!(0);
    let mut contradictory = row(PricingTarget::Platform, id);
    contradictory["tenant_id"] = json!(Uuid::new_v4());
    for value in [
        missing,
        no_scope,
        nil,
        zero,
        contradictory,
        row(b, id),
        row(PricingTarget::Platform, id),
        row(a, Uuid::nil()),
    ] {
        s.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(value)))
            .expect(1)
            .mount(&s)
            .await;
        assert!(
            api.list_pricing_page(&PricingQueryParams::new(a), "root")
                .await
                .is_err()
        );
        s.verify().await;
    }
    s.reset().await;
    for params in [
        PricingQueryParams::new(PricingTarget::Tenant {
            tenant_id: Uuid::nil(),
        }),
        PricingQueryParams::new(a).with_page_size(0),
        PricingQueryParams::new(a).with_page(0),
    ] {
        assert!(api.list_pricing_page(&params, "root").await.is_err());
    }
    assert!(s.received_requests().await.unwrap().is_empty());
}
#[tokio::test]
async fn pricing_commands_use_explicit_targets_versions_and_verified_results() {
    let s = MockServer::start().await;
    let api = AdminApi::new(&client(&s));
    let tid = Uuid::new_v4();
    let id = Uuid::new_v4();
    let t = PricingTarget::Tenant { tenant_id: tid };
    let req = CreatePricingRequest::new(
        t,
        "scoped-price",
        "provideraccount",
        "0.0000000001",
        "9999999999.9999999999",
        "CNY",
    );
    Mock::given(method("POST")).and(path("/api/v1/platform/pricing")).and(body_json(serde_json::to_value(&req).unwrap()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success":true,"message":"created","pricing_id":id,"model_name":"scoped-price","billing_dimension":"provideraccount","input_price_per_1k":"1E-10","output_price_per_1k":"9999999999.9999999999","is_default":false,"version":7}))).expect(1).mount(&s).await;
    assert_eq!(
        api.create_pricing(&req, "root").await.unwrap().pricing_id,
        id.to_string()
    );
    let patch = UpdatePricingRequest::new()
        .with_input_price_per_1k("0.0000000002")
        .with_expected_version(7);
    let url = format!("/api/v1/platform/pricing/{id}");
    Mock::given(method("PUT"))
        .and(path(&url))
        .and(query_param("scope_type", "tenant"))
        .and(query_param("tenant_id", tid.to_string()))
        .and(body_json(
            json!({"input_price_per_1k":"0.0000000002","expected_version":7}),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"success":true,"message":"updated","pricing_id":id,"version":8}),
            ),
        )
        .expect(1)
        .mount(&s)
        .await;
    assert_eq!(
        api.update_pricing(t, &id.to_string(), &patch, "root")
            .await
            .unwrap()
            .version,
        8
    );
    Mock::given(method("POST"))
        .and(path(format!("{url}/make-default")))
        .and(query_param("scope_type", "tenant"))
        .and(query_param("tenant_id", tid.to_string()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"success":true,"message":"default","pricing_id":id,"version":9}),
            ),
        )
        .expect(1)
        .mount(&s)
        .await;
    assert_eq!(
        api.make_pricing_default(t, &id.to_string(), "root")
            .await
            .unwrap()
            .version,
        9
    );
    Mock::given(method("DELETE"))
        .and(path(url))
        .and(query_param("scope_type", "tenant"))
        .and(query_param("tenant_id", tid.to_string()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"success":true,"message":"deleted","pricing_id":id})),
        )
        .expect(1)
        .mount(&s)
        .await;
    assert!(
        api.delete_pricing(t, &id.to_string(), "root")
            .await
            .unwrap()
            .success
    );
    Mock::given(method("POST"))
        .and(path("/api/v1/platform/pricing/batch-defaults"))
        .and(query_param("scope_type", "tenant"))
        .and(query_param("tenant_id", tid.to_string()))
        .and(body_json(json!({"model_ids":[id]})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"success":true,"message":"default","pricing_ids":[id]})),
        )
        .expect(1)
        .mount(&s)
        .await;
    assert!(
        api.set_default_pricing(
            t,
            &SetDefaultPricingRequest {
                model_ids: vec![id.to_string()]
            },
            "root"
        )
        .await
        .unwrap()
        .success
    );
    assert_eq!(s.received_requests().await.unwrap().len(), 5);
}
#[tokio::test]
async fn malformed_success_never_becomes_a_confirmed_change() {
    let s = MockServer::start().await;
    let api = AdminApi::new(&client(&s));
    let id = Uuid::new_v4();
    let t = PricingTarget::Tenant {
        tenant_id: Uuid::new_v4(),
    };
    for value in [
        json!({"message":"missing fields"}),
        json!({"success":true,"message":"wrong id","pricing_id":Uuid::new_v4()}),
        json!({"success":false,"message":"failed","pricing_id":id}),
    ] {
        s.reset().await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(200).set_body_json(value))
            .expect(1)
            .mount(&s)
            .await;
        assert!(
            api.delete_pricing(t, &id.to_string(), "root")
                .await
                .is_err()
        );
        s.verify().await;
    }
    s.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"success":true,"message":"wrong ids","pricing_ids":[Uuid::new_v4()]}),
        ))
        .expect(1)
        .mount(&s)
        .await;
    assert!(
        api.set_default_pricing(
            t,
            &SetDefaultPricingRequest {
                model_ids: vec![id.to_string()]
            },
            "root"
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn invalid_inputs_and_uncertain_mutations_never_fallback_or_automatically_retry() {
    let s = MockServer::start().await;
    let api = AdminApi::new(&client(&s));
    let id = Uuid::new_v4();
    let t = PricingTarget::Tenant {
        tenant_id: Uuid::new_v4(),
    };
    for code in [401, 503] {
        s.reset().await;
        Mock::given(method("PUT"))
            .respond_with(
                ResponseTemplate::new(code).set_body_json(json!({"error":{"message":"uncertain"}})),
            )
            .expect(1)
            .mount(&s)
            .await;
        assert!(
            api.update_pricing(
                t,
                &id.to_string(),
                &UpdatePricingRequest::new()
                    .with_expected_version(7)
                    .with_input_price_per_1k("1"),
                "root"
            )
            .await
            .is_err()
        );
        s.verify().await;
    }
    s.reset().await;
    assert!(
        api.delete_pricing(PricingTarget::Platform, &id.to_string(), "root")
            .await
            .is_err()
    );
    assert!(
        api.update_pricing(t, &id.to_string(), &UpdatePricingRequest::default(), "root")
            .await
            .is_err()
    );
    for bad in ["NaN", "-1", "0.00000000001", "10000000000"] {
        assert!(
            api.create_pricing(
                &CreatePricingRequest::new(t, "m", "provideraccount", bad, "1", "CNY"),
                "root"
            )
            .await
            .is_err()
        );
    }
    assert!(s.received_requests().await.unwrap().is_empty());
}
