use super::{
    command::Draft,
    types::{Action, Filter, Kind, Pending, Row},
};
use client_api::api::node_control::{NodeInfo, RegistrationInfo, TaskInfo};
use serde_json::json;
use uuid::Uuid;
fn node() -> Row {
    Row::Node(serde_json::from_value::<NodeInfo>(json!({"id":Uuid::new_v4(),"tenant_id":Uuid::new_v4(),"owner_user_id":Uuid::new_v4(),"display_name":"Node","status":"online","consecutive_failure_count":0,"failure_threshold":3,"last_heartbeat_at":null,"created_at":"2026-09-23T00:00:00Z","updated_at":"2026-09-23T00:00:00.000001Z"})).unwrap())
}
#[test]
fn node_filters_are_tenant_local_typed_and_archival_is_task_only() {
    for kind in Kind::ALL {
        let query = Filter::initial().query(kind).unwrap();
        assert_eq!(query.page, Some(1));
        assert_eq!(query.page_size, Some(20));
        assert_eq!(query.owner_user_id, None);
        assert_eq!(query.archived, (kind == Kind::Tasks).then_some(false));
    }
    let mut f = Filter::initial();
    f.owner = Uuid::nil().to_string();
    assert!(f.query(Kind::Nodes).is_err());
    f.owner = "bad".into();
    assert!(f.query(Kind::Nodes).is_err());
    let owner = Uuid::new_v4();
    f.owner = owner.to_string();
    f.search = "%_&tenant_id=foreign".into();
    assert_eq!(f.query(Kind::Nodes).unwrap().owner_user_id, Some(owner));
    assert_eq!(f.query(Kind::Nodes).unwrap().search, Some(f.search.clone()));
    f.search = "bad\n".into();
    assert!(f.query(Kind::Nodes).is_err());
    f.search.clear();
    f.status = "consumed".into();
    assert!(f.query(Kind::Nodes).is_err());
    assert!(f.query(Kind::Registrations).is_ok());
}
#[test]
fn node_commands_require_an_observed_revision_bounded_reason_and_configuration() {
    let mut p = Pending {
        row: node(),
        action: Action::Configure,
    };
    let mut draft = Draft {
        reason: "operator review".into(),
        name: "Node 2".into(),
        threshold: "5".into(),
    };
    assert!(draft.valid(&p));
    draft.threshold = "101".into();
    assert!(!draft.valid(&p));
    draft.threshold = "5".into();
    draft.reason = " ".into();
    assert!(!draft.valid(&p));
    draft.reason = "bad\nreason".into();
    assert!(!draft.valid(&p));
    draft.reason = "case".into();
    draft.name = "x".repeat(201);
    assert!(!draft.valid(&p));
    draft.name = "新节点".into();
    assert!(draft.valid(&p));
    p.action = Action::Delete;
    assert!(
        !draft.valid(&p),
        "online node deletion requires exclusion first"
    );
    p.action = Action::Configure;
    if let Row::Node(r) = &mut p.row {
        r.updated_at = "not-a-revision".into();
    }
    assert!(!draft.valid(&p));
}
#[test]
fn task_actions_distinguish_running_cancellation_and_terminal_archival() {
    let task=serde_json::from_value::<TaskInfo>(json!({"id":Uuid::new_v4(),"request_id":Uuid::new_v4(),"tenant_id":Uuid::new_v4(),"user_id":Uuid::new_v4(),"model":"fixture","status":"leased","assigned_node_id":null,"failure_count":0,"failure_threshold":3,"queued_at":"now","claimed_at":null,"finished_at":null,"deadline_at":"later","created_at":"now","updated_at":"now","cancellation_requested_at":null,"archived_at":null})).unwrap();
    let mut task = task;
    assert_eq!(Row::Task(task.clone()).actions(), vec![Action::Cancel]);
    task.cancellation_requested_at = Some("now".into());
    assert!(Row::Task(task.clone()).actions().is_empty());
    task.status = "succeeded".into();
    assert_eq!(Row::Task(task.clone()).actions(), vec![Action::Archive]);
    task.archived_at = Some("now".into());
    assert!(Row::Task(task).actions().is_empty());
}
#[test]
fn registration_decisions_never_offer_owner_secret_retrieval_or_reapprove_history() {
    let mut row=serde_json::from_value::<RegistrationInfo>(json!({"id":Uuid::new_v4(),"tenant_id":Uuid::new_v4(),"user_id":Uuid::new_v4(),"token_preview":"preview","status":"pending","is_revealed":false,"approved_by":null,"actioned_at":null,"consumed_at":null,"consumed_node_id":null,"issued_at":"now","updated_at":"now"})).unwrap();
    assert_eq!(
        Row::Registration(row.clone()).actions(),
        vec![Action::Approve, Action::Reject, Action::RevokeRegistration]
    );
    for status in ["approved", "consumed"] {
        row.status = status.into();
        assert_eq!(
            Row::Registration(row.clone()).actions(),
            vec![Action::RevokeRegistration]
        );
    }
    row.status = "rejected".into();
    assert!(Row::Registration(row).actions().is_empty());
}
#[test]
fn node_admin_route_and_dynamic_labels_are_available_in_both_languages() {
    use crate::router::Route;
    use std::str::FromStr;
    assert_eq!(
        Route::from_str("/tenant/nodes").unwrap(),
        Route::TenantNodes {}
    );
    for &(key, _, _) in crate::i18n::tenant_nodes::TEXT {
        assert!(crate::i18n::EN.contains_key(key));
        assert!(crate::i18n::ZH.contains_key(key));
    }
}
