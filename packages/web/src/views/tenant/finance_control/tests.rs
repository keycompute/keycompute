use super::*;
#[test]
fn owner_filter_requires_a_real_uuid() {
    assert!(owner("").is_err());
    assert!(owner(&Uuid::nil().to_string()).is_err());
    assert!(owner("bad&tenant_id=other").is_err());
    assert!(owner(&Uuid::new_v4().to_string()).is_ok());
}
#[test]
fn expiry_button_is_fail_closed_for_bad_dates() {
    assert!(!expired("bad"));
    assert!(expired("2000-01-01T00:00:00Z"));
}
#[test]
fn pagination_is_bounded_and_stable() {
    assert_eq!(pages(0), 1);
    assert_eq!(pages(1), 1);
    assert_eq!(pages(20), 1);
    assert_eq!(pages(21), 2);
    assert_eq!(pages(1_000_020), 50_001);
    assert_eq!(pages(i64::MAX), 50_001);
}
#[test]
fn tenant_financial_control_route_is_distinct_from_read_only_finance() {
    use crate::router::Route;
    assert_eq!(
        "/tenant/finance/controls".parse::<Route>().unwrap(),
        Route::TenantFinancialControls {}
    );
    assert_ne!(
        Route::TenantFinancialControls {}.to_string(),
        Route::TenantFinance {}.to_string()
    );
}

#[test]
fn withdrawal_status_and_type_labels_are_localized_with_unknown_fallbacks() {
    use crate::i18n::{I18n, Lang};

    let zh = I18n::new(Lang::Zh);
    let en = I18n::new(Lang::En);
    assert_eq!(withdrawal_status_label(zh, "pending"), "待审核");
    assert_eq!(withdrawal_status_label(en, "approved"), "Approved");
    assert_eq!(withdrawal_type_label(zh, "balance"), "转入余额");
    assert_eq!(withdrawal_type_label(en, "alipay"), "Alipay payout");
    assert_eq!(withdrawal_status_label(zh, "future_state"), "future_state");
    assert_eq!(withdrawal_type_label(en, "future_type"), "future_type");
}
