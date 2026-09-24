use super::types::*;
use client_api::api::tenant_reporting::{PaymentState, TenantPaymentRecord};
use serde_json::json;
use uuid::Uuid;
#[test]
fn report_windows_require_explicit_timezones_and_a_bounded_ordered_range() {
    assert!(window("2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z").is_ok());
    for (from, to) in [
        ("2026-01-01T00:00:00", "2026-01-02T00:00:00Z"),
        ("2026-01-02T00:00:00Z", "2026-01-01T00:00:00Z"),
        ("2026-01-01T00:00:00Z", "2026-03-01T00:00:00Z"),
        ("bad", "bad"),
    ] {
        assert!(window(from, to).is_err());
    }
    let q = window("2026-01-01T08:00:00+08:00", "2026-01-02T08:00:00+08:00").unwrap();
    assert_eq!(q.from, "2026-01-01T00:00:00+00:00");
}
#[test]
fn finance_selectors_are_explicit_and_cannot_widen_to_all_tenants() {
    assert_eq!(owner("").unwrap(), None);
    assert!(owner(&Uuid::nil().to_string()).is_err());
    assert!(owner("user&tenant_id=other").is_err());
    assert_eq!(status("paid").unwrap(), Some(PaymentState::Paid));
    assert_eq!(status("").unwrap(), None);
    assert!(status("all&owner=x").is_err());
}
#[test]
fn detail_identity_preserves_tenant_member_and_current_order_revision() {
    let row:TenantPaymentRecord=serde_json::from_value(json!({"id":Uuid::new_v4(),"tenant_id":Uuid::new_v4(),"user_id":Uuid::new_v4(),"amount":"7.0000000001","currency":"CNY","status":"paid","payment_method":"wechatpay","payment_scene":"native","paid_at":null,"closed_at":null,"expired_at":"later","created_at":"now","updated_at":"now"})).unwrap();
    let first = Detail::Order(row.clone());
    let mut another = row;
    another.user_id = Uuid::new_v4();
    assert_ne!(first.key(), Detail::Order(another.clone()).key());
    another.user_id = first.owner();
    another.updated_at = "new".into();
    assert_ne!(first.key(), Detail::Order(another).key());
}
#[test]
fn tenant_finance_route_does_not_reuse_platform_payment_administration() {
    use crate::router::Route;
    assert_eq!(
        "/tenant/finance".parse::<Route>().unwrap(),
        Route::TenantFinance {}
    );
    let q = Query::default();
    assert_eq!(q.tab, Tab::Usage);
    assert_eq!(q.page, 1);
    assert_eq!(q.report().owner_user_id, None);
    assert!(window(&q.window.from, &q.window.to).is_ok());
}
