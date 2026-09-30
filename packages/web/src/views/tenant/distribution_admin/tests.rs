use super::types::*;
use client_api::api::distribution_policy::{BeneficiaryScope, DistributionPolicy};
use uuid::Uuid;
fn row() -> DistributionPolicy {
    DistributionPolicy {
        id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        beneficiary_scope: BeneficiaryScope::TenantMember,
        beneficiary_id: Some(Uuid::new_v4()),
        name: "member rule".into(),
        description: Some("future allocation".into()),
        commission_rate: "0.1250".into(),
        priority: 5,
        is_active: true,
        effective_from: "2026-10-01T00:00:00Z".into(),
        effective_until: Some("2026-11-01T00:00:00Z".into()),
        created_at: "2026-09-01T00:00:00Z".into(),
        updated_at: "2026-09-02T00:00:00.123456Z".into(),
    }
}
#[test]
fn create_requires_exact_beneficiary_and_plain_rate() {
    let mut d = Draft::for_operation(&Operation::Create);
    d.name = "x".into();
    d.reason = "future allocation".into();
    d.rate = "0.1250".into();
    assert!(d.create().is_ok());
    d.beneficiary_scope = BeneficiaryScope::TenantMember;
    assert!(d.create().is_err());
    d.beneficiary_id = Uuid::new_v4().to_string();
    assert!(d.create().is_ok());
    d.rate = "12.5%".into();
    assert!(d.create().is_err());
}
#[test]
fn edit_keeps_beneficiary_and_exact_revision_and_can_clear_optional_fields() {
    let r = row();
    let mut d = Draft::for_operation(&Operation::Edit(r.clone()));
    d.reason = "retire future description".into();
    d.description.clear();
    d.until.clear();
    let p = d.patch(&r).unwrap();
    assert_eq!(p.expected_updated_at, r.updated_at);
    assert_eq!(p.description, Some(None));
    assert_eq!(p.effective_until, Some(None));
    d.beneficiary_id = Uuid::new_v4().to_string();
    assert!(d.patch(&r).is_err());
}
#[test]
fn operation_identity_contains_the_observed_revision() {
    let r = row();
    let k = Operation::Edit(r.clone()).key();
    assert!(k.contains(&r.id.to_string()));
    assert!(k.contains(&r.updated_at));
    assert_ne!(Operation::Create.key(), Operation::Default.key());
}
#[test]
fn tenant_distribution_route_is_distinct_from_personal_distribution() {
    use crate::router::Route;
    assert_eq!(
        "/tenant/distribution".parse::<Route>().unwrap(),
        Route::TenantDistribution {}
    );
    assert_eq!(
        "/distribution".parse::<Route>().unwrap(),
        Route::DistributionOverview {}
    );
}
