use super::types::*;
use client_api::api::tenant_pricing::{BillingDimension, PricingScope, TenantPrice};
use uuid::Uuid;
fn row() -> TenantPrice {
    TenantPrice {
        id: Uuid::new_v4(),
        scope_type: PricingScope::Tenant,
        tenant_id: Uuid::new_v4(),
        model_name: "fixture".into(),
        billing_dimension: BillingDimension::ProviderAccount,
        currency: "CNY".into(),
        input_price_per_1k: "1E-10".into(),
        output_price_per_1k: "9999999999.9999999999".into(),
        is_default: false,
        is_effective: true,
        effective_from: "2026-01-01T00:00:00Z".into(),
        effective_until: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        version: 7,
    }
}
#[test]
fn editor_preserves_exact_prices_and_displayed_version() {
    let original = row();
    let draft = Draft::for_operation(&Operation::Edit(original.clone()));
    assert_eq!(draft.input, "0.0000000001");
    assert_eq!(draft.output, "9999999999.9999999999");
    let update = draft.update(&original).unwrap();
    assert_eq!(update.expected_version, 7);
    assert_eq!(update.input_price_per_1k.as_deref(), Some("0.0000000001"));
}
#[test]
fn editor_never_changes_pricing_ownership_identity_or_invents_a_version() {
    let mut original = row();
    let mut draft = Draft::for_operation(&Operation::Edit(original.clone()));
    draft.currency = "USD".into();
    assert!(draft.update(&original).is_err());
    draft.currency = "CNY".into();
    original.version = 0;
    assert!(draft.update(&original).is_err());
}
#[test]
fn create_has_no_platform_or_tenant_payload_selector() {
    let mut draft = Draft::for_operation(&Operation::Create);
    draft.model = "model".into();
    draft.input = "0.0000000001".into();
    draft.output = "1.0000000000".into();
    let value = serde_json::to_value(draft.create().unwrap()).unwrap();
    assert!(value.get("tenant_id").is_none());
    assert!(value.get("scope_type").is_none());
    assert_eq!(value["input_price_per_1k"], "0.0000000001");
    for bad in ["-1", "NaN", "0.00000000001", "10000000000"] {
        draft.input = bad.into();
        assert!(draft.create().is_err());
    }
}
#[test]
fn end_date_is_never_cleared_implicitly_and_windows_require_a_timezone() {
    let original = row();
    let mut draft = Draft::for_operation(&Operation::Edit(original.clone()));
    assert!(draft.update(&original).unwrap().effective_until.is_none());
    draft.until = "2025-01-01T00:00:00Z".into();
    assert!(draft.update(&original).is_err());
    draft.until = "2030-01-01T00:00:00".into();
    assert!(draft.update(&original).is_err());
    draft.until = "2030-01-01T00:00:00+08:00".into();
    assert!(draft.update(&original).is_ok());
}
#[test]
fn all_dynamic_pricing_labels_are_translated() {
    use crate::i18n::{EN, ZH};
    for suffix in [
        "effective",
        "ineffective",
        "default_badge",
        "create_times",
        "edit_times",
        "delete_hint",
        "default_hint",
        "create",
        "edit",
        "delete",
        "default",
    ] {
        let key = format!("tenant_pricing.{suffix}");
        assert!(EN.contains_key(key.as_str()));
        assert!(ZH.contains_key(key.as_str()));
    }
}

#[test]
fn tenant_pricing_has_a_canonical_route_separate_from_platform_pricing() {
    let route: crate::router::Route = "/tenant/pricing".parse().unwrap();
    assert_eq!(route, crate::router::Route::TenantPricing {});
    assert_eq!(route.to_string(), "/tenant/pricing");
}
