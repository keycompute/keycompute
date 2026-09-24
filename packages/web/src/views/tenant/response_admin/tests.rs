use super::types::*;
use client_api::api::response_control::{ResponseMode, ResponseSummary};
use serde_json::{Value, json};
use uuid::Uuid;
#[test]
fn resource_mode_and_owner_selectors_never_invent_a_global_or_native_scope() {
    assert_eq!(mode("passthrough"), Some(ResponseMode::Passthrough));
    assert_eq!(mode("node_dispatch"), Some(ResponseMode::NodeDispatch));
    for value in ["account_pool", "all", "", "node:"] {
        assert!(mode(value).is_none());
    }
    assert_eq!(owner(" ").unwrap(), None);
    assert!(owner("00000000-0000-0000-0000-000000000000").is_err());
    assert!(owner("a&tenant_id=other").is_err());
}
#[test]
fn metadata_matches_the_existing_string_pair_contract() {
    assert_eq!(metadata("null").unwrap(), json!({}));
    assert_eq!(
        metadata(r#"{"case":"updated"}"#).unwrap(),
        json!({"case":"updated"})
    );
    for raw in [r#"{"key":5}"#, "[]", "{broken}"] {
        assert!(metadata(raw).is_err());
    }
    let big = json!({"key":"x".repeat(513)});
    assert!(metadata(&big.to_string()).is_err());
    let many = (0..17)
        .map(|i| (format!("key{i}"), json!("value")))
        .collect::<serde_json::Map<String, Value>>();
    assert!(metadata(&Value::Object(many).to_string()).is_err());
}
#[test]
fn item_editor_rejects_silent_truncation_and_non_objects() {
    assert_eq!(
        items(r#"[{"role":"user","content":"hi"}]"#).unwrap().len(),
        1
    );
    for raw in ["[]", "[1]", "{}", "not JSON"] {
        assert!(items(raw).is_err());
    }
    assert!(items(&json!(vec![json!({}); 513]).to_string()).is_err());
    assert!(items(&" ".repeat(2 * 1024 * 1024 + 1)).is_err());
}
#[test]
fn commands_keep_the_original_owner_mode_and_revision() {
    let owner = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let row:ResponseSummary=serde_json::from_value(json!({"id":"resp_fixture","tenant_id":tenant,"owner_user_id":owner,"mode":"passthrough","provider":null,"account_id":null,"model":"fixture","status":"in_progress","background":true,"store_response":true,"stream":false,"previous_response_id":null,"conversation_id":null,"revision":7,"created_at":"now","updated_at":"now","expires_at":"later","deleted":false,"local_content_available":true,"native_content_available":false})).unwrap();
    let item = Row::Response(row.clone());
    let cmd = Mutation::Cancel(item);
    assert_eq!(cmd.row().address().unwrap().owner, owner);
    assert_eq!(cmd.row().address().unwrap().mode, ResponseMode::Passthrough);
    assert_eq!(cmd.row().revision().unwrap(), 7);
    assert!(cmd.row().can_cancel());
    let mut completed = row.clone();
    completed.status = "completed".into();
    assert!(!Row::Response(completed).can_cancel());
    let mut bad = row;
    bad.revision = None;
    assert!(Row::Response(bad).revision().is_err());
}

fn identity_fixture() -> Row {
    Row::Conversation(serde_json::from_value(json!({
        "id":"opaque/shared?id=1", "tenant_id":Uuid::new_v4(), "owner_user_id":Uuid::new_v4(),
        "mode":"passthrough", "account_id":null, "model":"fixture", "metadata":{"private":"not-a-state-key"},
        "active_response_id":null, "revision":7, "created_at":"now", "updated_at":"now", "expires_at":"later", "deleted":false
    })).unwrap())
}

#[test]
fn row_and_dialog_keys_cover_complete_identity_without_embedding_content() {
    let original = identity_fixture();
    let mut variants = vec![original.clone()];
    for dimension in ["owner", "tenant", "mode", "id"] {
        let mut changed = original.clone();
        let Row::Conversation(row) = &mut changed else {
            unreachable!()
        };
        match dimension {
            "owner" => row.owner_user_id = Uuid::new_v4(),
            "tenant" => row.tenant_id = Uuid::new_v4(),
            "mode" => row.mode = "node_dispatch".into(),
            "id" => row.id.push_str(":another"),
            _ => unreachable!(),
        }
        assert_ne!(original.identity(), changed.identity());
        variants.push(changed);
    }
    let Row::Conversation(c) = &original else {
        unreachable!()
    };
    let mut response = json!({"id":c.id,"tenant_id":c.tenant_id,"owner_user_id":c.owner_user_id,"mode":c.mode,
        "account_id":null,"provider":null,"model":"fixture","status":"completed","background":false,
        "store_response":true,"stream":false,"previous_response_id":null,"conversation_id":null,
        "revision":7,"created_at":"now","updated_at":"now","expires_at":"later","deleted":false,
        "local_content_available":true,"native_content_available":false});
    variants.push(Row::Response(
        serde_json::from_value(response.take()).unwrap(),
    ));
    let keys: std::collections::HashSet<_> = variants.iter().map(Row::key).collect();
    assert_eq!(keys.len(), variants.len());
    let inspector_keys: std::collections::HashSet<_> = variants
        .iter()
        .map(|row| {
            Inspection {
                row: row.clone(),
                read: ReadKind::Detail,
            }
            .key()
        })
        .collect();
    let editor_keys: std::collections::HashSet<_> = variants
        .iter()
        .map(|row| Mutation::Delete(row.clone()).key())
        .collect();
    assert_eq!(inspector_keys.len(), variants.len());
    assert_eq!(editor_keys.len(), variants.len());
    for key in keys
        .iter()
        .chain(inspector_keys.iter())
        .chain(editor_keys.iter())
    {
        assert!(!key.contains("not-a-state-key"));
    }
}

#[test]
fn revisions_and_item_selectors_recreate_only_their_own_editor_state() {
    let original = identity_fixture();
    let mut newer = original.clone();
    let Row::Conversation(c) = &mut newer else {
        unreachable!()
    };
    c.revision = Some(8);
    assert_eq!(original.key(), newer.key());
    assert_ne!(original.state_key(), newer.state_key());
    assert_ne!(
        Inspection {
            row: original.clone(),
            read: ReadKind::Detail
        }
        .key(),
        Inspection {
            row: original.clone(),
            read: ReadKind::Items
        }
        .key()
    );
    assert_ne!(
        Mutation::RemoveItem(original.clone(), "item:a".into()).key(),
        Mutation::RemoveItem(original.clone(), "item:b".into()).key()
    );
    assert_ne!(
        Mutation::Metadata(original.clone()).key(),
        Mutation::Append(original.clone()).key()
    );
    let first = Mutation::Metadata(original).key();
    assert_ne!(first, Mutation::Metadata(newer).key());
}
