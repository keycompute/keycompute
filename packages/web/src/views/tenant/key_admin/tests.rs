use super::types::*;
use chrono::{Duration, Utc};
use client_api::api::key_control::KeyMetadata;
use serde_json::json;
use uuid::Uuid;
fn key() -> KeyMetadata {
    serde_json::from_value(json!({"id":Uuid::new_v4(),"tenant_id":Uuid::new_v4(),"owner_user_id":Uuid::new_v4(),
        "name":"Original key","key_preview":"sk-fixt****","revoked":false,"revoked_at":null,
        "expires_at":(Utc::now()+Duration::days(180)).to_rfc3339(),"created_at":"2026-01-01T00:00:00Z",
        "updated_at":"2026-01-01T00:00:00.000001Z","last_used_at":null})).unwrap()
}
#[test]
fn key_expiration_editor_preserves_omitted_null_and_explicit_semantics() {
    let row = key();
    let mut draft = Draft::for_operation(&Operation::Edit(row.clone()));
    assert_eq!(draft.expiry, Expiry::Keep);
    assert!(draft.patch(&row).is_err());
    draft.name = "Renamed".into();
    let patch = draft.patch(&row).unwrap();
    assert_eq!(patch.expected_updated_at, row.updated_at);
    assert!(patch.expires_at.is_none());
    assert!(
        serde_json::to_value(patch)
            .unwrap()
            .get("expires_at")
            .is_none()
    );
    draft.expiry = Expiry::Never;
    assert_eq!(draft.patch(&row).unwrap().expires_at, Some(None));
    assert!(serde_json::to_value(draft.patch(&row).unwrap()).unwrap()["expires_at"].is_null());
    draft.expiry = Expiry::At;
    draft.date = (Utc::now() + Duration::days(365)).to_rfc3339();
    assert!(draft.patch(&row).unwrap().expires_at.flatten().is_some());
}
#[test]
fn metadata_edits_never_change_owner_or_invent_an_observed_revision() {
    let row = key();
    let mut draft = Draft::for_operation(&Operation::Edit(row.clone()));
    draft.name = "changed".into();
    draft.owner = Uuid::new_v4().to_string();
    assert!(draft.patch(&row).is_err());
    draft.owner = row.owner_user_id.to_string();
    let mut malformed = row.clone();
    malformed.updated_at.clear();
    assert!(draft.patch(&malformed).is_err());
    let mut expired = row.clone();
    expired.expires_at = Some("2000-01-01T00:00:00Z".into());
    let mut draft = Draft::for_operation(&Operation::Edit(expired.clone()));
    draft.name = "Rename expired metadata".into();
    assert!(draft.patch(&expired).is_ok());
    draft.expiry = Expiry::Never;
    assert!(draft.patch(&expired).is_err());
    assert!(!live_key(&expired));
    expired.revoked = true;
    expired.expires_at = None;
    assert!(!live_key(&expired));
    assert!(draft.rotate(&expired).is_err());
}
#[test]
fn new_issuance_requires_an_explicit_member_and_bounded_future_expiration() {
    let mut draft = Draft::for_operation(&Operation::Request);
    draft.name = "  New key  ".into();
    assert!(draft.request().is_err());
    draft.owner = Uuid::new_v4().to_string();
    let request = draft.request().unwrap();
    assert_eq!(request.name, "New key");
    assert!(request.expires_at.is_some());
    let wire = serde_json::to_value(request).unwrap();
    assert!(wire.get("tenant_id").is_none());
    assert!(wire.get("platform_role").is_none());
    for value in [
        "2000-01-01T00:00:00Z",
        "2099-01-01T00:00:00Z",
        "2030-01-01T00:00:00",
        "not-a-date",
    ] {
        draft.date = value.into();
        assert!(draft.request().is_err(), "{value}");
    }
    draft.expiry = Expiry::Never;
    assert!(draft.request().unwrap().expires_at.is_none());
    draft.expiry = Expiry::Keep;
    assert!(draft.request().is_err());
    draft.owner = Uuid::nil().to_string();
    assert!(draft.request().is_err());
    assert_eq!(owner_filter("").unwrap(), None);
    assert!(owner_filter(&Uuid::nil().to_string()).is_err());
}
#[test]
fn rotation_keeps_the_original_owner_and_dialog_identity_includes_the_revision() {
    let row = key();
    let draft = Draft::for_operation(&Operation::Rotate(row.clone()));
    let request = draft.rotate(&row).unwrap();
    let original = chrono::DateTime::parse_from_rfc3339(row.expires_at.as_ref().unwrap()).unwrap();
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(request.expires_at.as_ref().unwrap()).unwrap(),
        original
    );
    let key = Operation::Edit(row.clone()).key();
    assert_ne!(key, Operation::Delete(row.clone()).key());
    let mut changed = row.clone();
    changed.updated_at = "2026-01-01T00:00:00.000002Z".into();
    assert_ne!(key, Operation::Edit(changed).key());
    let mut changed = row;
    changed.owner_user_id = Uuid::new_v4();
    assert_ne!(key, Operation::Edit(changed).key());
}
#[test]
fn owner_and_administrator_key_routes_are_distinct_and_labels_exist() {
    use crate::{
        i18n::{EN, ZH},
        router::Route,
    };
    assert_eq!(
        "/tenant/keys".parse::<Route>().unwrap(),
        Route::TenantKeys {}
    );
    assert_eq!(
        "/api-keys/issuance".parse::<Route>().unwrap(),
        Route::OwnerKeyIssuance {}
    );
    assert_eq!(Route::ApiKeyList {}.to_string(), "/api-keys");
    for suffix in [
        "request",
        "edit",
        "rotate",
        "revoke",
        "delete",
        "cancel_request",
        "revoked",
        "expired",
        "active",
        "requested",
        "already_pending",
        "revoked_result",
        "deleted_result",
        "retained_result",
        "cancelled_result",
        "claim",
        "decline",
        "copied",
        "copy_failed",
    ] {
        let key = format!("tenant_keys.{suffix}");
        assert!(EN.contains_key(key.as_str()), "{key}");
        assert!(ZH.contains_key(key.as_str()), "{key}");
    }
}
