use client_api::{
    AdminApi, AssignableUserRole, ClientError, UserRole,
    api::admin::{
        ReleaseBalanceReservationRequest, UpdateBalanceRequest, UpdateUserRequest,
        UserBalanceReservationsResponse, UserDetail, UserQueryParams,
    },
};
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
#[cfg(any(target_arch = "wasm32", test))]
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use ui::{
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, PageHeader, Pagination, Table,
    TableHead,
};

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::api_client::{get_client, user_error_message, with_auto_refresh};
use crate::stores::auth_store::AuthStore;
use crate::stores::ui_store::UiStore;
use crate::stores::user_store::UserStore;
use crate::utils::display::{short_id, user_role_label};
use crate::utils::resource::{KeyedResourceValue, current_keyed_value};
use crate::utils::time::format_time;

const PAGE_SIZE: usize = 20;
const BALANCE_RESERVATION_PAGE_SIZE: u64 = 20;
const SEARCH_DEBOUNCE_MS: u32 = 300;
#[cfg(any(target_arch = "wasm32", test))]
const MANUAL_BALANCE_OPERATION_STORAGE_PREFIX: &str = "keyc_admin_balance_operations_v3:";
const MANUAL_BALANCE_OPERATION_STORAGE_VERSION: u8 = 3;
const JAVASCRIPT_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

// Web Storage does not provide compare-and-set semantics across browser agent
// clusters. Keep every read/choose/write/verify and terminalize/rotate sequence
// under the same Web Lock so two tabs cannot mint different keys for one
// logical balance mutation or replace another generation.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
const KEYCOMPUTE_BALANCE_BINDING_VERSION = 3;
const KEYCOMPUTE_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const KEYCOMPUTE_BINDING_PENDING = "pending";
const KEYCOMPUTE_BINDING_TERMINAL = "terminal";

function keycomputeLockManager() {
    const manager = globalThis.navigator && globalThis.navigator.locks;
    if (!manager || typeof manager.request !== "function") {
        throw new Error("web_locks_unavailable");
    }
    return manager;
}

function keycomputeStorage() {
    const storage = globalThis.localStorage;
    if (!storage) {
        throw new Error("local_storage_unavailable");
    }
    return storage;
}

function keycomputeParseBalanceBinding(raw) {
    if (raw === null) {
        return null;
    }
    let parsed;
    try {
        parsed = JSON.parse(raw);
    } catch (_) {
        throw new Error("balance_binding_corrupted");
    }
    if (!parsed || parsed.version !== KEYCOMPUTE_BALANCE_BINDING_VERSION ||
        typeof parsed.idempotency_key !== "string" ||
        !KEYCOMPUTE_UUID.test(parsed.idempotency_key) ||
        !Number.isSafeInteger(parsed.revision) || parsed.revision < 1 ||
        (parsed.state !== KEYCOMPUTE_BINDING_PENDING &&
         parsed.state !== KEYCOMPUTE_BINDING_TERMINAL)) {
        throw new Error("balance_binding_corrupted");
    }
    return parsed;
}

export function keycomputePersistBalanceBinding(lockName, storageKey, proposedKey) {
    if (!KEYCOMPUTE_UUID.test(proposedKey)) {
        return Promise.reject(new Error("balance_binding_corrupted"));
    }
    let manager;
    try {
        manager = keycomputeLockManager();
    } catch (error) {
        return Promise.reject(error);
    }
    return manager.request(lockName, { mode: "exclusive" }, () => {
        const storage = keycomputeStorage();
        const existing = keycomputeParseBalanceBinding(storage.getItem(storageKey));
        const binding = existing === null ? {
            version: KEYCOMPUTE_BALANCE_BINDING_VERSION,
            idempotency_key: proposedKey,
            revision: 1,
            state: KEYCOMPUTE_BINDING_PENDING,
        } : existing;
        if (existing === null) {
            storage.setItem(storageKey, JSON.stringify(binding));
        }
        const verified = keycomputeParseBalanceBinding(storage.getItem(storageKey));
        if (verified === null || verified.idempotency_key !== binding.idempotency_key ||
            verified.revision !== binding.revision || verified.state !== binding.state) {
            throw new Error("balance_binding_verification_failed");
        }
        return JSON.stringify(binding);
    });
}

export function keycomputeFinalizeBalanceBinding(lockName, storageKey, expectedKey, expectedRevision) {
    let manager;
    try {
        manager = keycomputeLockManager();
    } catch (error) {
        return Promise.reject(error);
    }
    return manager.request(lockName, { mode: "exclusive" }, () => {
        const storage = keycomputeStorage();
        let existing = keycomputeParseBalanceBinding(storage.getItem(storageKey));
        if (existing !== null && existing.idempotency_key !== expectedKey) {
            throw new Error("balance_binding_changed");
        }
        if (existing === null) {
            const revision = Number(expectedRevision);
            if (!Number.isSafeInteger(revision) || revision < 1 ||
                revision === Number.MAX_SAFE_INTEGER || !KEYCOMPUTE_UUID.test(expectedKey)) {
                throw new Error("balance_binding_corrupted");
            }
            existing = {
                version: KEYCOMPUTE_BALANCE_BINDING_VERSION,
                idempotency_key: expectedKey,
                revision: revision + 1,
                state: KEYCOMPUTE_BINDING_TERMINAL,
            };
        } else if (existing.state === KEYCOMPUTE_BINDING_PENDING) {
            if (String(existing.revision) !== expectedRevision) {
                throw new Error("balance_binding_changed");
            }
            if (existing.revision === Number.MAX_SAFE_INTEGER) {
                throw new Error("balance_binding_revision_exhausted");
            }
            existing = {
                ...existing,
                revision: existing.revision + 1,
                state: KEYCOMPUTE_BINDING_TERMINAL,
            };
        }
        storage.setItem(storageKey, JSON.stringify(existing));
        const verified = keycomputeParseBalanceBinding(storage.getItem(storageKey));
        if (verified === null || verified.idempotency_key !== expectedKey ||
            verified.state !== KEYCOMPUTE_BINDING_TERMINAL) {
            throw new Error("balance_binding_verification_failed");
        }
    });
}

export function keycomputeRotateBalanceBinding(
    lockName, storageKey, expectedKey, expectedRevision, proposedKey
) {
    if (!KEYCOMPUTE_UUID.test(proposedKey) || proposedKey === expectedKey) {
        return Promise.reject(new Error("balance_binding_corrupted"));
    }
    let manager;
    try {
        manager = keycomputeLockManager();
    } catch (error) {
        return Promise.reject(error);
    }
    return manager.request(lockName, { mode: "exclusive" }, () => {
        const storage = keycomputeStorage();
        const existing = keycomputeParseBalanceBinding(storage.getItem(storageKey));
        if (existing === null || existing.idempotency_key !== expectedKey ||
            String(existing.revision) !== expectedRevision ||
            existing.state !== KEYCOMPUTE_BINDING_TERMINAL) {
            throw new Error("balance_binding_changed");
        }
        if (existing.revision === Number.MAX_SAFE_INTEGER) {
            throw new Error("balance_binding_revision_exhausted");
        }
        const binding = {
            version: KEYCOMPUTE_BALANCE_BINDING_VERSION,
            idempotency_key: proposedKey,
            revision: existing.revision + 1,
            state: KEYCOMPUTE_BINDING_PENDING,
        };
        storage.setItem(storageKey, JSON.stringify(binding));
        const verified = keycomputeParseBalanceBinding(storage.getItem(storageKey));
        if (verified === null || verified.idempotency_key !== binding.idempotency_key ||
            verified.revision !== binding.revision || verified.state !== binding.state) {
            throw new Error("balance_binding_verification_failed");
        }
        return JSON.stringify(binding);
    });
}
"#)]
extern "C" {
    #[wasm_bindgen::prelude::wasm_bindgen(catch, js_name = keycomputePersistBalanceBinding)]
    fn persist_balance_binding_with_lock(
        lock_name: &str,
        storage_key: &str,
        proposed_key: &str,
    ) -> Result<js_sys::Promise, wasm_bindgen::JsValue>;

    #[wasm_bindgen::prelude::wasm_bindgen(catch, js_name = keycomputeFinalizeBalanceBinding)]
    fn finalize_balance_binding_with_lock(
        lock_name: &str,
        storage_key: &str,
        expected_key: &str,
        expected_revision: &str,
    ) -> Result<js_sys::Promise, wasm_bindgen::JsValue>;

    #[wasm_bindgen::prelude::wasm_bindgen(catch, js_name = keycomputeRotateBalanceBinding)]
    fn rotate_balance_binding_with_lock(
        lock_name: &str,
        storage_key: &str,
        expected_key: &str,
        expected_revision: &str,
        proposed_key: &str,
    ) -> Result<js_sys::Promise, wasm_bindgen::JsValue>;
}

fn is_conflict_error(error: &ClientError) -> bool {
    matches!(error, ClientError::Http(message) if message.starts_with("HTTP 409:"))
}

fn normalize_manual_balance_reason(reason: &str) -> String {
    reason.trim().to_string()
}

/// Namespace retry bindings by the exact URL prefix used for administrator
/// requests. Display helpers intentionally normalize `/v1`, `/api/v1`, and
/// `/auth`; using one of those here would merge distinct configured backends.
fn manual_balance_operation_api_namespace(config: &client_api::ClientConfig) -> String {
    config.build_url("/api/v1")
}

type BalanceDetailsLoadResult = Result<Option<UserBalanceReservationsResponse>, ClientError>;
type BalanceDetailsRequestKey = (Option<String>, Option<String>);

#[derive(Clone, Debug, Eq, PartialEq)]
struct BalanceReservationPagination {
    cursors: Vec<Option<String>>,
    current_page: usize,
}

impl Default for BalanceReservationPagination {
    fn default() -> Self {
        Self {
            cursors: vec![None],
            current_page: 0,
        }
    }
}

impl BalanceReservationPagination {
    fn current_cursor(&self) -> Option<String> {
        self.cursors.get(self.current_page).cloned().flatten()
    }

    fn page_number(&self) -> usize {
        self.current_page + 1
    }

    fn can_go_back(&self) -> bool {
        self.current_page > 0
    }

    fn advance(&mut self, expected_page: usize, next_cursor: String) -> bool {
        if self.page_number() != expected_page {
            return false;
        }
        self.cursors.truncate(self.current_page + 1);
        self.cursors.push(Some(next_cursor));
        self.current_page += 1;
        true
    }

    fn go_back(&mut self, expected_page: usize) -> bool {
        if self.page_number() != expected_page || !self.can_go_back() {
            return false;
        }
        self.current_page = self.current_page.saturating_sub(1);
        true
    }

    fn reset(&mut self) {
        self.cursors.clear();
        self.cursors.push(None);
        self.current_page = 0;
    }
}

/// Returns only a completed balance response for the currently selected user.
///
/// Dioxus retains a resource's previous value while the next load is pending,
/// and the dependency watcher may not have marked it pending during the render
/// that changes the selected user. The request key rejects both stale states;
/// the response ID is a final guard against a mismatched server response.
fn current_balance_details_result(
    current_request: &BalanceDetailsRequestKey,
    state: UseResourceState,
    loaded: Option<KeyedResourceValue<BalanceDetailsRequestKey, BalanceDetailsLoadResult>>,
) -> Option<Result<UserBalanceReservationsResponse, ClientError>> {
    match current_keyed_value(current_request, state, loaded)? {
        Ok(Some(details)) if current_request.0.as_deref() == Some(details.user_id.as_str()) => {
            Some(Ok(details))
        }
        Ok(Some(details)) => Some(Err(ClientError::InvalidResponse(format!(
            "balance details user mismatch: expected {}, got {}",
            current_request.0.as_deref().unwrap_or("none"),
            details.user_id,
        )))),
        Ok(None) => None,
        Err(error) => Some(Err(error)),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UserListQuery {
    search: String,
    page: u32,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(any(target_arch = "wasm32", test), derive(Deserialize, Serialize))]
struct ManualBalanceOperationIdentity {
    user_id: String,
    action: String,
    amount: String,
    reason: String,
}

/// Browser-persistence namespace for unresolved financial mutations.
///
/// User and tenant IDs prevent one administrator session from inheriting
/// another session's retry keys. The API root additionally separates apps
/// hosted on the same browser origin but connected to different backends.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(any(target_arch = "wasm32", test), derive(Deserialize, Serialize))]
struct ManualBalanceOperationScope {
    api_root: String,
    administrator_user_id: String,
    administrator_tenant_id: String,
}

impl ManualBalanceOperationScope {
    fn new(
        api_root: impl Into<String>,
        administrator_user_id: impl Into<String>,
        administrator_tenant_id: impl Into<String>,
    ) -> Self {
        Self {
            api_root: api_root.into().trim_end_matches('/').to_string(),
            administrator_user_id: administrator_user_id.into(),
            administrator_tenant_id: administrator_tenant_id.into(),
        }
    }

    #[cfg(any(target_arch = "wasm32", test))]
    fn is_persistable(&self) -> bool {
        !self.api_root.is_empty()
            && !self.administrator_user_id.is_empty()
            && !self.administrator_tenant_id.is_empty()
    }

    #[cfg(any(target_arch = "wasm32", test))]
    fn storage_key(
        &self,
        identity: &ManualBalanceOperationIdentity,
    ) -> Result<String, ManualBalanceOperationStorageError> {
        if !self.is_persistable() {
            return Err(ManualBalanceOperationStorageError::IncompleteScope);
        }
        let encoded = serde_json::to_string(&(self, identity))
            .map_err(|_| ManualBalanceOperationStorageError::SerializationFailed)?;
        Ok(format!(
            "{MANUAL_BALANCE_OPERATION_STORAGE_PREFIX}{encoded}"
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(any(target_arch = "wasm32", test), derive(Deserialize, Serialize))]
#[cfg_attr(any(target_arch = "wasm32", test), serde(rename_all = "snake_case"))]
enum ManualBalanceBindingState {
    Pending,
    Terminal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(any(target_arch = "wasm32", test), derive(Deserialize, Serialize))]
struct PersistedManualBalanceBinding {
    version: u8,
    idempotency_key: String,
    revision: u64,
    state: ManualBalanceBindingState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ManualBalanceOperationStorageError {
    #[cfg(any(target_arch = "wasm32", test))]
    IncompleteScope,
    Unavailable,
    #[cfg(test)]
    ReadFailed,
    CorruptedData,
    #[cfg(test)]
    UnsupportedVersion,
    #[cfg(any(target_arch = "wasm32", test))]
    SerializationFailed,
    #[cfg(any(target_arch = "wasm32", test))]
    WriteFailed,
    VerificationFailed,
    BindingChanged,
    TerminalConfirmationRequired,
}

#[cfg(target_arch = "wasm32")]
fn manual_balance_storage_promise_error(
    error: wasm_bindgen::JsValue,
    fallback: ManualBalanceOperationStorageError,
) -> ManualBalanceOperationStorageError {
    let message = js_sys::Reflect::get(&error, &wasm_bindgen::JsValue::from_str("message"))
        .ok()
        .and_then(|message| message.as_string());
    if message.as_deref() == Some("balance_binding_changed") {
        ManualBalanceOperationStorageError::BindingChanged
    } else {
        fallback
    }
}

#[cfg(test)]
fn manual_balance_binding_from_snapshot(
    raw: Option<&str>,
) -> Result<Option<PersistedManualBalanceBinding>, ManualBalanceOperationStorageError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let stored = serde_json::from_str::<PersistedManualBalanceBinding>(raw)
        .map_err(|_| ManualBalanceOperationStorageError::CorruptedData)?;
    if stored.version != MANUAL_BALANCE_OPERATION_STORAGE_VERSION {
        return Err(ManualBalanceOperationStorageError::UnsupportedVersion);
    }
    if stored.revision == 0
        || stored.revision > JAVASCRIPT_MAX_SAFE_INTEGER
        || uuid::Uuid::parse_str(&stored.idempotency_key).is_err()
    {
        return Err(ManualBalanceOperationStorageError::CorruptedData);
    }
    Ok(Some(stored))
}

#[cfg(test)]
fn persist_manual_balance_binding_in_snapshot(
    raw: Option<&str>,
    proposed_key: &str,
) -> Result<(PersistedManualBalanceBinding, String), ManualBalanceOperationStorageError> {
    uuid::Uuid::parse_str(proposed_key)
        .map_err(|_| ManualBalanceOperationStorageError::CorruptedData)?;
    let stored = match manual_balance_binding_from_snapshot(raw)? {
        Some(existing) => existing,
        None => PersistedManualBalanceBinding {
            version: MANUAL_BALANCE_OPERATION_STORAGE_VERSION,
            idempotency_key: proposed_key.to_string(),
            revision: 1,
            state: ManualBalanceBindingState::Pending,
        },
    };
    let serialized = serde_json::to_string(&stored)
        .map_err(|_| ManualBalanceOperationStorageError::SerializationFailed)?;
    Ok((stored, serialized))
}

#[cfg(test)]
fn finalize_manual_balance_binding_in_snapshot(
    raw: Option<&str>,
    expected_key: &str,
    expected_revision: u64,
) -> Result<(PersistedManualBalanceBinding, String), ManualBalanceOperationStorageError> {
    uuid::Uuid::parse_str(expected_key)
        .map_err(|_| ManualBalanceOperationStorageError::CorruptedData)?;
    if expected_revision == 0 || expected_revision >= JAVASCRIPT_MAX_SAFE_INTEGER {
        return Err(ManualBalanceOperationStorageError::CorruptedData);
    }
    let binding = match manual_balance_binding_from_snapshot(raw)? {
        Some(current) if current.idempotency_key != expected_key => {
            return Err(ManualBalanceOperationStorageError::BindingChanged);
        }
        Some(current) if current.state == ManualBalanceBindingState::Terminal => current,
        Some(mut current) if current.revision == expected_revision => {
            current.revision = current
                .revision
                .checked_add(1)
                .ok_or(ManualBalanceOperationStorageError::VerificationFailed)?;
            current.state = ManualBalanceBindingState::Terminal;
            current
        }
        Some(_) => return Err(ManualBalanceOperationStorageError::BindingChanged),
        None => PersistedManualBalanceBinding {
            version: MANUAL_BALANCE_OPERATION_STORAGE_VERSION,
            idempotency_key: expected_key.to_string(),
            revision: expected_revision
                .checked_add(1)
                .ok_or(ManualBalanceOperationStorageError::VerificationFailed)?,
            state: ManualBalanceBindingState::Terminal,
        },
    };
    let serialized = serde_json::to_string(&binding)
        .map_err(|_| ManualBalanceOperationStorageError::SerializationFailed)?;
    Ok((binding, serialized))
}

#[cfg(test)]
fn rotate_manual_balance_binding_in_snapshot(
    raw: Option<&str>,
    expected_key: &str,
    expected_revision: u64,
    proposed_key: &str,
) -> Result<(PersistedManualBalanceBinding, String), ManualBalanceOperationStorageError> {
    uuid::Uuid::parse_str(proposed_key)
        .map_err(|_| ManualBalanceOperationStorageError::CorruptedData)?;
    if proposed_key == expected_key {
        return Err(ManualBalanceOperationStorageError::BindingChanged);
    }
    let Some(current) = manual_balance_binding_from_snapshot(raw)? else {
        return Err(ManualBalanceOperationStorageError::BindingChanged);
    };
    if current.idempotency_key != expected_key
        || current.revision != expected_revision
        || current.state != ManualBalanceBindingState::Terminal
    {
        return Err(ManualBalanceOperationStorageError::BindingChanged);
    }
    if current.revision >= JAVASCRIPT_MAX_SAFE_INTEGER {
        return Err(ManualBalanceOperationStorageError::VerificationFailed);
    }
    let binding = PersistedManualBalanceBinding {
        version: MANUAL_BALANCE_OPERATION_STORAGE_VERSION,
        idempotency_key: proposed_key.to_string(),
        revision: current
            .revision
            .checked_add(1)
            .ok_or(ManualBalanceOperationStorageError::VerificationFailed)?,
        state: ManualBalanceBindingState::Pending,
    };
    let serialized = serde_json::to_string(&binding)
        .map_err(|_| ManualBalanceOperationStorageError::SerializationFailed)?;
    Ok((binding, serialized))
}

trait ManualBalanceOperationPersistence {
    async fn persist_binding(
        &self,
        scope: &ManualBalanceOperationScope,
        identity: &ManualBalanceOperationIdentity,
        proposed_key: &str,
    ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError>;

    async fn finalize_binding(
        &self,
        scope: &ManualBalanceOperationScope,
        identity: &ManualBalanceOperationIdentity,
        expected_key: &str,
        expected_revision: u64,
    ) -> Result<(), ManualBalanceOperationStorageError>;

    async fn rotate_binding(
        &self,
        scope: &ManualBalanceOperationScope,
        identity: &ManualBalanceOperationIdentity,
        expected_key: &str,
        expected_revision: u64,
        proposed_key: &str,
    ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError>;
}

/// Minimal localStorage adapter. Native builds fail closed instead of pretending
/// persistence succeeded; native unit tests inject an in-memory implementation.
#[derive(Clone, Copy)]
struct ManualBalanceOperationStorage;

impl ManualBalanceOperationPersistence for ManualBalanceOperationStorage {
    async fn persist_binding(
        &self,
        scope: &ManualBalanceOperationScope,
        identity: &ManualBalanceOperationIdentity,
        proposed_key: &str,
    ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError> {
        #[cfg(target_arch = "wasm32")]
        {
            let storage_key = scope.storage_key(identity)?;
            uuid::Uuid::parse_str(proposed_key)
                .map_err(|_| ManualBalanceOperationStorageError::CorruptedData)?;
            let promise =
                persist_balance_binding_with_lock(&storage_key, &storage_key, proposed_key)
                    .map_err(|_| ManualBalanceOperationStorageError::Unavailable)?;
            let serialized = wasm_bindgen_futures::JsFuture::from(promise)
                .await
                .map_err(|_| ManualBalanceOperationStorageError::WriteFailed)?
                .as_string()
                .ok_or(ManualBalanceOperationStorageError::VerificationFailed)?;
            let binding: PersistedManualBalanceBinding = serde_json::from_str(&serialized)
                .map_err(|_| ManualBalanceOperationStorageError::VerificationFailed)?;
            if binding.version != MANUAL_BALANCE_OPERATION_STORAGE_VERSION
                || binding.revision == 0
                || uuid::Uuid::parse_str(&binding.idempotency_key).is_err()
            {
                return Err(ManualBalanceOperationStorageError::VerificationFailed);
            }
            Ok(binding)
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (scope, identity, proposed_key);
            Err(ManualBalanceOperationStorageError::Unavailable)
        }
    }

    async fn finalize_binding(
        &self,
        scope: &ManualBalanceOperationScope,
        identity: &ManualBalanceOperationIdentity,
        expected_key: &str,
        expected_revision: u64,
    ) -> Result<(), ManualBalanceOperationStorageError> {
        #[cfg(target_arch = "wasm32")]
        {
            let storage_key = scope.storage_key(identity)?;
            let expected_revision = expected_revision.to_string();
            let promise = finalize_balance_binding_with_lock(
                &storage_key,
                &storage_key,
                expected_key,
                &expected_revision,
            )
            .map_err(|_| ManualBalanceOperationStorageError::Unavailable)?;
            wasm_bindgen_futures::JsFuture::from(promise)
                .await
                .map_err(|error| {
                    manual_balance_storage_promise_error(
                        error,
                        ManualBalanceOperationStorageError::WriteFailed,
                    )
                })?;
            Ok(())
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (scope, identity, expected_key, expected_revision);
            Err(ManualBalanceOperationStorageError::Unavailable)
        }
    }

    async fn rotate_binding(
        &self,
        scope: &ManualBalanceOperationScope,
        identity: &ManualBalanceOperationIdentity,
        expected_key: &str,
        expected_revision: u64,
        proposed_key: &str,
    ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError> {
        #[cfg(target_arch = "wasm32")]
        {
            let storage_key = scope.storage_key(identity)?;
            uuid::Uuid::parse_str(proposed_key)
                .map_err(|_| ManualBalanceOperationStorageError::CorruptedData)?;
            let expected_revision = expected_revision.to_string();
            let promise = rotate_balance_binding_with_lock(
                &storage_key,
                &storage_key,
                expected_key,
                &expected_revision,
                proposed_key,
            )
            .map_err(|_| ManualBalanceOperationStorageError::Unavailable)?;
            let serialized = wasm_bindgen_futures::JsFuture::from(promise)
                .await
                .map_err(|error| {
                    manual_balance_storage_promise_error(
                        error,
                        ManualBalanceOperationStorageError::WriteFailed,
                    )
                })?
                .as_string()
                .ok_or(ManualBalanceOperationStorageError::VerificationFailed)?;
            let binding: PersistedManualBalanceBinding = serde_json::from_str(&serialized)
                .map_err(|_| ManualBalanceOperationStorageError::VerificationFailed)?;
            if binding.version != MANUAL_BALANCE_OPERATION_STORAGE_VERSION
                || binding.revision == 0
                || binding.state != ManualBalanceBindingState::Pending
                || uuid::Uuid::parse_str(&binding.idempotency_key).is_err()
            {
                return Err(ManualBalanceOperationStorageError::VerificationFailed);
            }
            Ok(binding)
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (
                scope,
                identity,
                expected_key,
                expected_revision,
                proposed_key,
            );
            Err(ManualBalanceOperationStorageError::Unavailable)
        }
    }
}

impl ManualBalanceOperationIdentity {
    fn new(user_id: &str, action: &str, request: &UpdateBalanceRequest) -> Self {
        Self {
            user_id: user_id.to_string(),
            action: action.to_string(),
            amount: request.amount.clone(),
            reason: normalize_manual_balance_reason(&request.reason),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActiveManualBalanceSubmission {
    submission_id: u64,
    modal_epoch: u64,
    scope: ManualBalanceOperationScope,
    identity: ManualBalanceOperationIdentity,
    idempotency_key: String,
    binding_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManualBalanceTerminalConfirmation {
    modal_epoch: u64,
    scope: ManualBalanceOperationScope,
    identity: ManualBalanceOperationIdentity,
    idempotency_key: String,
    binding_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ManualBalanceOperationPreparationKind {
    CreateOrResume,
    RotateTerminal {
        expected_key: String,
        expected_revision: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManualBalanceOperationPreparation {
    submission_id: u64,
    modal_epoch: u64,
    scope: ManualBalanceOperationScope,
    identity: ManualBalanceOperationIdentity,
    proposed_key: String,
    had_pending_binding: bool,
    kind: ManualBalanceOperationPreparationKind,
}

/// Tracks manual balance mutations independently of the modal's presentation
/// state. An ambiguous result keeps its key even if the modal is closed and
/// reopened, the component remounts, or the page reloads. A definitive
/// success/conflict turns the matching binding into a durable tombstone. An
/// identical future operation can rotate that tombstone only after an explicit
/// second confirmation; neither pending bindings nor tombstones are evicted
/// automatically because doing so could turn a retry into a duplicate
/// financial operation.
#[derive(Clone, Debug, Default)]
struct ManualBalanceOperationTracker {
    scope: ManualBalanceOperationScope,
    modal_epoch: u64,
    next_submission_id: u64,
    pending: HashMap<ManualBalanceOperationIdentity, String>,
    preparing: Option<ManualBalanceOperationPreparation>,
    active: Option<ActiveManualBalanceSubmission>,
    terminal_confirmation: Option<ManualBalanceTerminalConfirmation>,
}

#[derive(Debug)]
struct ManualBalanceOperationFinish {
    owns_modal: bool,
    storage_error: Option<ManualBalanceOperationStorageError>,
}

impl ManualBalanceOperationTracker {
    fn new(scope: ManualBalanceOperationScope) -> Self {
        Self {
            scope,
            ..Self::default()
        }
    }

    fn advance_modal(&mut self) {
        self.modal_epoch = self.modal_epoch.wrapping_add(1);
        self.terminal_confirmation = None;
    }

    fn is_active(&self) -> bool {
        self.preparing.is_some() || self.active.is_some()
    }

    fn activate_scope(&mut self, scope: &ManualBalanceOperationScope) -> bool {
        if &self.scope == scope {
            return true;
        }
        if self.is_active() {
            return false;
        }
        self.scope = scope.clone();
        self.pending.clear();
        self.terminal_confirmation = None;
        self.advance_modal();
        true
    }

    fn has_terminal_confirmation(&self) -> bool {
        self.terminal_confirmation
            .as_ref()
            .is_some_and(|confirmation| confirmation.modal_epoch == self.modal_epoch)
    }

    fn clear_terminal_confirmation(&mut self) {
        self.terminal_confirmation = None;
    }

    fn prepare_submission(
        &mut self,
        identity: ManualBalanceOperationIdentity,
        create_idempotency_key: impl FnOnce() -> String,
    ) -> Result<Option<ManualBalanceOperationPreparation>, ManualBalanceOperationStorageError> {
        if self.is_active() {
            return Ok(None);
        }

        if self
            .terminal_confirmation
            .as_ref()
            .is_some_and(|confirmation| confirmation.identity != identity)
        {
            self.terminal_confirmation = None;
        }
        let repeat_confirmation = self
            .terminal_confirmation
            .as_ref()
            .filter(|confirmation| {
                confirmation.modal_epoch == self.modal_epoch && confirmation.identity == identity
            })
            .cloned();
        let had_pending_binding = self.pending.contains_key(&identity);
        let proposed_key = if repeat_confirmation.is_some() {
            create_idempotency_key()
        } else {
            self.pending.get(&identity).cloned().unwrap_or_else(|| {
                let key = create_idempotency_key();
                self.pending.insert(identity.clone(), key.clone());
                key
            })
        };
        if uuid::Uuid::parse_str(&proposed_key).is_err() {
            if repeat_confirmation.is_none() && !had_pending_binding {
                self.pending.remove(&identity);
            }
            return Err(ManualBalanceOperationStorageError::CorruptedData);
        }
        self.next_submission_id = self.next_submission_id.wrapping_add(1);
        let preparation = ManualBalanceOperationPreparation {
            submission_id: self.next_submission_id,
            modal_epoch: self.modal_epoch,
            scope: self.scope.clone(),
            identity,
            proposed_key,
            had_pending_binding,
            kind: repeat_confirmation.map_or(
                ManualBalanceOperationPreparationKind::CreateOrResume,
                |confirmation| ManualBalanceOperationPreparationKind::RotateTerminal {
                    expected_key: confirmation.idempotency_key,
                    expected_revision: confirmation.binding_revision,
                },
            ),
        };
        self.preparing = Some(preparation.clone());
        Ok(Some(preparation))
    }

    fn confirm_submission(
        &mut self,
        preparation: &ManualBalanceOperationPreparation,
        persisted: Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError>,
    ) -> Result<Option<ActiveManualBalanceSubmission>, ManualBalanceOperationStorageError> {
        let is_current = self
            .preparing
            .as_ref()
            .is_some_and(|current| current.submission_id == preparation.submission_id);
        if !is_current {
            return Ok(None);
        }
        self.preparing = None;
        if self.modal_epoch != preparation.modal_epoch || self.scope != preparation.scope {
            // Persistence may finish after a route/modal replacement. Leave
            // its durable state intact, but never dispatch from a stale UI;
            // the next visible submission will re-read it under the Web Lock.
            return Ok(None);
        }

        let binding = match persisted {
            Err(ManualBalanceOperationStorageError::BindingChanged)
                if matches!(
                    preparation.kind,
                    ManualBalanceOperationPreparationKind::RotateTerminal { .. }
                ) =>
            {
                // A concurrent explicit repeat already advanced this identity.
                // Drop the stale confirmation so the next ordinary click can
                // safely adopt that pending successor instead of retrying an
                // obsolete rotation forever.
                self.terminal_confirmation = None;
                return Err(ManualBalanceOperationStorageError::BindingChanged);
            }
            result => result?,
        };
        if binding.version != MANUAL_BALANCE_OPERATION_STORAGE_VERSION
            || binding.revision == 0
            || binding.revision > JAVASCRIPT_MAX_SAFE_INTEGER
        {
            return Err(ManualBalanceOperationStorageError::VerificationFailed);
        }
        let durable_key = binding.idempotency_key;
        if uuid::Uuid::parse_str(&durable_key).is_err() {
            return Err(ManualBalanceOperationStorageError::VerificationFailed);
        }
        match &preparation.kind {
            ManualBalanceOperationPreparationKind::CreateOrResume => {
                if preparation.had_pending_binding && durable_key != preparation.proposed_key {
                    // Another tab rotated an identity for which this component
                    // still has an ambiguous request. Adopting the new key
                    // could duplicate the old operation.
                    return Err(ManualBalanceOperationStorageError::BindingChanged);
                }
                if binding.state == ManualBalanceBindingState::Terminal {
                    self.pending.remove(&preparation.identity);
                    self.terminal_confirmation = Some(ManualBalanceTerminalConfirmation {
                        modal_epoch: preparation.modal_epoch,
                        scope: preparation.scope.clone(),
                        identity: preparation.identity.clone(),
                        idempotency_key: durable_key,
                        binding_revision: binding.revision,
                    });
                    return Err(ManualBalanceOperationStorageError::TerminalConfirmationRequired);
                }
            }
            ManualBalanceOperationPreparationKind::RotateTerminal {
                expected_key,
                expected_revision,
            } => {
                let Some(successor_revision) = expected_revision.checked_add(1) else {
                    return Err(ManualBalanceOperationStorageError::VerificationFailed);
                };
                if binding.state != ManualBalanceBindingState::Pending
                    || durable_key != preparation.proposed_key
                    || binding.revision != successor_revision
                {
                    return Err(ManualBalanceOperationStorageError::BindingChanged);
                }
                let is_matching_confirmation =
                    self.terminal_confirmation
                        .as_ref()
                        .is_some_and(|confirmation| {
                            confirmation.modal_epoch == preparation.modal_epoch
                                && confirmation.scope == preparation.scope
                                && confirmation.identity == preparation.identity
                                && confirmation.idempotency_key == *expected_key
                                && confirmation.binding_revision == *expected_revision
                        });
                if !is_matching_confirmation {
                    return Err(ManualBalanceOperationStorageError::BindingChanged);
                }
                self.terminal_confirmation = None;
            }
        }
        self.pending
            .insert(preparation.identity.clone(), durable_key.clone());
        let submission = ActiveManualBalanceSubmission {
            submission_id: preparation.submission_id,
            modal_epoch: preparation.modal_epoch,
            scope: preparation.scope.clone(),
            identity: preparation.identity.clone(),
            idempotency_key: durable_key,
            binding_revision: binding.revision,
        };
        self.active = Some(submission.clone());
        Ok(Some(submission))
    }

    #[cfg(test)]
    fn begin_submission(
        &mut self,
        identity: ManualBalanceOperationIdentity,
        create_idempotency_key: impl FnOnce() -> String,
        storage: &impl ManualBalanceOperationPersistence,
    ) -> Result<Option<ActiveManualBalanceSubmission>, ManualBalanceOperationStorageError> {
        let Some(preparation) = self.prepare_submission(identity, create_idempotency_key)? else {
            return Ok(None);
        };
        let persisted = match &preparation.kind {
            ManualBalanceOperationPreparationKind::CreateOrResume => {
                poll_ready_test_future(storage.persist_binding(
                    &preparation.scope,
                    &preparation.identity,
                    &preparation.proposed_key,
                ))
            }
            ManualBalanceOperationPreparationKind::RotateTerminal {
                expected_key,
                expected_revision,
            } => poll_ready_test_future(storage.rotate_binding(
                &preparation.scope,
                &preparation.identity,
                expected_key,
                *expected_revision,
                &preparation.proposed_key,
            )),
        };
        self.confirm_submission(&preparation, persisted)
    }

    /// Finish one invocation and report whether it still owns the modal epoch
    /// from which it was started. A stale invocation may update durable key
    /// bookkeeping, but callers must not let it close or overwrite a new modal.
    fn should_finalize_binding(
        &self,
        submission: &ActiveManualBalanceSubmission,
        terminal: bool,
    ) -> bool {
        terminal
            && self
                .active
                .as_ref()
                .is_some_and(|active| active.submission_id == submission.submission_id)
            && self
                .pending
                .get(&submission.identity)
                .is_some_and(|key| key == &submission.idempotency_key)
    }

    fn complete_submission(
        &mut self,
        submission: &ActiveManualBalanceSubmission,
        terminal: bool,
        finalization: Option<Result<(), ManualBalanceOperationStorageError>>,
    ) -> ManualBalanceOperationFinish {
        let is_current = self
            .active
            .as_ref()
            .is_some_and(|active| active.submission_id == submission.submission_id);
        if !is_current {
            return ManualBalanceOperationFinish {
                owns_modal: false,
                storage_error: None,
            };
        }
        self.active = None;

        let mut storage_error = None;
        if terminal
            && self
                .pending
                .get(&submission.identity)
                .is_some_and(|key| key == &submission.idempotency_key)
        {
            // Keep the in-memory binding if the terminal tombstone cannot be
            // verified. Retrying a completed server operation with the same
            // key is safe; minting a replacement is not.
            match finalization
                .unwrap_or(Err(ManualBalanceOperationStorageError::VerificationFailed))
            {
                Ok(()) => {
                    self.pending.remove(&submission.identity);
                }
                Err(ManualBalanceOperationStorageError::BindingChanged) => {
                    // Another tab explicitly rotated the already-terminal
                    // generation. Its binding must remain, but this terminal
                    // invocation no longer needs a local retry.
                    self.pending.remove(&submission.identity);
                }
                Err(error) => storage_error = Some(error),
            }
        }

        ManualBalanceOperationFinish {
            owns_modal: self.modal_epoch == submission.modal_epoch,
            storage_error,
        }
    }

    #[cfg(test)]
    fn finish_submission(
        &mut self,
        submission: &ActiveManualBalanceSubmission,
        terminal: bool,
        storage: &impl ManualBalanceOperationPersistence,
    ) -> ManualBalanceOperationFinish {
        let finalization = if self.should_finalize_binding(submission, terminal) {
            Some(poll_ready_test_future(storage.finalize_binding(
                &submission.scope,
                &submission.identity,
                &submission.idempotency_key,
                submission.binding_revision,
            )))
        } else {
            None
        };
        self.complete_submission(submission, terminal, finalization)
    }
}

#[cfg(test)]
fn poll_ready_test_future<F: std::future::Future>(future: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(output) => output,
        std::task::Poll::Pending => panic!("test persistence future unexpectedly suspended"),
    }
}

impl Default for UserListQuery {
    fn default() -> Self {
        Self {
            search: String::new(),
            page: 1,
        }
    }
}

impl UserListQuery {
    /// 提交新的搜索词时必须同时回到第一页，避免防抖窗口内的分页操作
    /// 被带入新查询。搜索词与页码放在同一状态中也只会触发一次资源重载。
    fn commit_search(&mut self, search: String) {
        self.search = search;
        self.page = 1;
    }
}

#[component]
pub fn Users() -> Element {
    let user_store = use_context::<UserStore>();
    let is_admin = user_store
        .info
        .read()
        .as_ref()
        .map(|u| u.is_admin())
        .unwrap_or(false);

    if is_admin {
        rsx! { AdminUsersView {} }
    } else {
        rsx! { UserSelfView {} }
    }
}

// ── Admin 视图 ────────────────────────────────────────────────────────

#[component]
fn AdminUsersView() -> Element {
    let i18n = use_i18n();
    let user_store = use_context::<UserStore>();
    let auth_store = use_context::<AuthStore>();
    let mut ui_store = use_context::<UiStore>();
    let mut search = use_signal(String::new);
    let mut query = use_signal(UserListQuery::default);
    let current_user = user_store.info.read().clone();
    let can_current_user_manage_roles = current_user
        .as_ref()
        .map(|u| u.role == UserRole::System.as_str())
        .unwrap_or(false);
    let current_user_id = current_user
        .as_ref()
        .map(|u| u.id.clone())
        .unwrap_or_default();
    let current_user_tenant_id = current_user
        .as_ref()
        .map(|u| u.tenant_id.clone())
        .unwrap_or_default();
    let api_client = get_client();
    let balance_operation_scope = ManualBalanceOperationScope::new(
        manual_balance_operation_api_namespace(api_client.config()),
        current_user_id.clone(),
        current_user_tenant_id,
    );
    let initial_balance_operation_scope = balance_operation_scope.clone();
    let balance_operation_storage = ManualBalanceOperationStorage;
    let current_user_id_for_edit = current_user_id.clone();
    let can_current_user_manage_roles_for_edit = can_current_user_manage_roles;
    let current_user_id_for_delete = current_user_id.clone();
    let can_current_user_delete_admins = can_current_user_manage_roles;

    // 编辑弹窗状态
    let mut edit_user = use_signal(|| Option::<UserDetail>::None);
    let mut edit_name = use_signal(String::new);
    let mut edit_role = use_signal(String::new);
    let mut edit_saving = use_signal(|| false);

    // 删除确认状态
    let mut delete_user = use_signal(|| Option::<UserDetail>::None);
    let mut delete_saving = use_signal(|| false);

    // 余额管理弹窗状态
    let mut balance_user = use_signal(|| Option::<UserDetail>::None);
    let mut balance_action = use_signal(|| "recharge".to_string());
    let mut balance_amount = use_signal(String::new);
    let mut balance_reason = use_signal(String::new);
    let mut balance_error = use_signal(String::new);
    let mut balance_operation_tracker = use_signal(move || {
        ManualBalanceOperationTracker::new(initial_balance_operation_scope.clone())
    });
    let mut release_request_id = use_signal(|| Option::<String>::None);
    let mut release_expected_version = use_signal(|| Option::<String>::None);
    let mut release_reason = use_signal(String::new);
    let mut release_saving = use_signal(|| false);
    let mut balance_reservation_pagination = use_signal(BalanceReservationPagination::default);

    use_effect(move || {
        let next_search = search();
        spawn(async move {
            TimeoutFuture::new(SEARCH_DEBOUNCE_MS).await;
            if search() == next_search && query.read().search != next_search {
                query.write().commit_search(next_search);
            }
        });
    });

    let mut users_resource = use_resource(move || {
        let current_query = query();
        async move {
            let params = UserQueryParams::new()
                .with_page_size(PAGE_SIZE as i64)
                .with_page(current_query.page as i64);
            let params = if !current_query.search.is_empty() {
                params.with_search(current_query.search)
            } else {
                params
            };
            with_auto_refresh(auth_store, move |token| {
                let params = params.clone();
                async move {
                    let client = get_client();
                    AdminApi::new(&client)
                        .list_all_users(Some(&params), &token)
                        .await
                }
            })
            .await
        }
    });

    // 弹窗打开后重新从写库语义的余额服务读取拆分，避免使用用户列表的旧快照。
    let mut balance_details_resource = use_resource(move || {
        let request_key = (
            balance_user().map(|user| user.id),
            balance_reservation_pagination().current_cursor(),
        );
        let selected_user_id = request_key.0.clone();
        let selected_cursor = request_key.1.clone();
        let auth = auth_store.clone();
        async move {
            let result = if let Some(user_id) = selected_user_id {
                with_auto_refresh(auth, move |token| {
                    let user_id = user_id.clone();
                    let cursor = selected_cursor.clone();
                    async move {
                        let client = get_client();
                        AdminApi::new(&client)
                            .list_user_balance_reservations_page(
                                &user_id,
                                cursor.as_deref(),
                                Some(BALANCE_RESERVATION_PAGE_SIZE),
                                &token,
                            )
                            .await
                            .map(Some)
                    }
                })
                .await
            } else {
                Ok(None)
            };
            KeyedResourceValue::new(request_key, result)
        }
    });

    let paged_users = move || -> Vec<UserDetail> {
        match users_resource() {
            Some(Ok(ref resp)) => resp.users.clone(),
            _ => vec![],
        }
    };

    let total_items = move || -> i64 {
        match users_resource() {
            Some(Ok(ref resp)) => resp.total,
            _ => 0,
        }
    };

    let total_pages = move || -> u32 {
        match users_resource() {
            Some(Ok(ref resp)) => resp.total_pages.max(1) as u32,
            _ => 1,
        }
    };

    let current_balance_details = move || {
        let request_key = (
            balance_user().map(|user| user.id),
            balance_reservation_pagination().current_cursor(),
        );
        current_balance_details_result(
            &request_key,
            balance_details_resource.state().cloned(),
            balance_details_resource(),
        )
    };

    // 提交编辑
    let on_edit_save = move |_| {
        let Some(u) = edit_user() else { return };
        let name_val = edit_name();
        let role_val = edit_role();
        let can_edit_role = can_current_user_manage_roles_for_edit
            && u.id != current_user_id_for_edit
            && u.role != "system";
        let role = if !can_edit_role || role_val.trim().is_empty() {
            None
        } else {
            match role_val.parse::<AssignableUserRole>() {
                Ok(role) => Some(role),
                Err(err) => {
                    ui_store.show_error(err);
                    return;
                }
            }
        };
        let id = u.id.clone();
        edit_saving.set(true);
        spawn(async move {
            let token = auth_store.token().unwrap_or_default();
            let client = get_client();
            let req = UpdateUserRequest {
                name: if name_val.trim().is_empty() {
                    None
                } else {
                    Some(name_val)
                },
                role,
            };
            match AdminApi::new(&client).update_user(&id, &req, &token).await {
                Ok(_) => {
                    ui_store.show_success(i18n.t("users.updated"));
                    edit_user.set(None);
                    users_resource.restart();
                }
                Err(e) => {
                    ui_store.show_error(format!("{}: {e}", i18n.t("users.update_failed")));
                }
            }
            edit_saving.set(false);
        });
    };

    // 确认删除
    let on_delete_confirm = move |_| {
        let Some(u) = delete_user() else { return };
        if u.id == current_user_id_for_delete
            || u.role == UserRole::System.as_str()
            || (u.role == UserRole::Admin.as_str() && !can_current_user_delete_admins)
        {
            // 区分不同类型的禁止删除原因
            let msg = if u.id == current_user_id_for_delete {
                i18n.t("users.delete_self_forbidden")
            } else if u.role == UserRole::System.as_str() {
                i18n.t("users.cannot_modify_system")
            } else {
                i18n.t("users.delete_admin_forbidden")
            };
            ui_store.show_error(msg.to_string());
            delete_user.set(None);
            return;
        }
        let id = u.id.clone();
        delete_saving.set(true);
        spawn(async move {
            let token = auth_store.token().unwrap_or_default();
            let client = get_client();
            match AdminApi::new(&client).delete_user(&id, &token).await {
                Ok(_) => {
                    ui_store.show_success(i18n.t("users.deleted"));
                    delete_user.set(None);
                    users_resource.restart();
                }
                Err(e) => {
                    ui_store.show_error(format!("{}: {e}", i18n.t("users.delete_failed")));
                }
            }
            delete_saving.set(false);
        });
    };

    // 提交余额操作
    let on_balance_save = move |_| {
        // The signal update below is synchronous, so this also closes the
        // double-click window before the loading state is rendered.
        if balance_operation_tracker.read().is_active() || release_saving() {
            return;
        }
        let Some(u) = balance_user() else { return };
        let action = balance_action();
        let amount_str = balance_amount();
        let reason = normalize_manual_balance_reason(&balance_reason());

        if amount_str.trim().is_empty() {
            balance_error.set(i18n.t("users.balance_amount_required").to_string());
            return;
        }
        let amount: f64 = match amount_str.trim().parse() {
            Ok(v) if v > 0.0 => v,
            _ => {
                balance_error.set(i18n.t("users.balance_amount_invalid").to_string());
                return;
            }
        };
        // 校验小数位不超过两位
        let trimmed = amount_str.trim();
        let valid_precision = match trimmed.find('.') {
            Some(pos) => trimmed.len() - pos - 1 <= 2,
            None => true,
        };
        if !valid_precision {
            balance_error.set(i18n.t("users.balance_amount_precision").to_string());
            return;
        }
        if reason.is_empty() {
            balance_error.set(i18n.t("users.balance_reason_required").to_string());
            return;
        }
        if action == "unfreeze" {
            let Some(Ok(details)) = current_balance_details() else {
                balance_error.set(i18n.t("users.balance_details_unavailable").to_string());
                return;
            };
            let requested = amount_str
                .trim()
                .parse::<rust_decimal::Decimal>()
                .unwrap_or_default();
            let releasable = details
                .manually_frozen_balance
                .parse::<rust_decimal::Decimal>()
                .unwrap_or_default();
            if requested > releasable {
                balance_error.set(i18n.t("users.balance_unfreeze_exceeds_releasable").replace(
                    "{amount}",
                    &crate::utils::format_precise_money_str(&details.manually_frozen_balance),
                ));
                return;
            }
        }

        let id = u.id.clone();
        let req = match action.as_str() {
            "recharge" => UpdateBalanceRequest::add(amount, &reason),
            "deduct" => UpdateBalanceRequest::subtract(amount, &reason),
            "freeze" | "unfreeze" => UpdateBalanceRequest::new(amount, &reason),
            _ => {
                balance_error.set(i18n.t("users.balance_action_invalid").to_string());
                return;
            }
        };
        // An exact wire payload keeps one key until the server gives a
        // definitive success/conflict. Closing and reopening the modal after
        // an ambiguous transport/server failure must not mint a second key.
        let identity = ManualBalanceOperationIdentity::new(&id, &action, &req);
        let preparation_result = {
            let mut tracker = balance_operation_tracker.write();
            if !tracker.activate_scope(&balance_operation_scope) {
                return;
            }
            tracker.prepare_submission(identity, || uuid::Uuid::new_v4().to_string())
        };
        let preparation = match preparation_result {
            Ok(Some(preparation)) => preparation,
            Ok(None) => return,
            Err(_) => {
                // Fail closed: without a durable, verified key the request
                // must not be sent because a reload could duplicate it.
                balance_error.set(
                    i18n.t("users.balance_idempotency_prepare_failed")
                        .to_string(),
                );
                return;
            }
        };
        spawn(async move {
            let persisted = match &preparation.kind {
                ManualBalanceOperationPreparationKind::CreateOrResume => {
                    balance_operation_storage
                        .persist_binding(
                            &preparation.scope,
                            &preparation.identity,
                            &preparation.proposed_key,
                        )
                        .await
                }
                ManualBalanceOperationPreparationKind::RotateTerminal {
                    expected_key,
                    expected_revision,
                } => {
                    balance_operation_storage
                        .rotate_binding(
                            &preparation.scope,
                            &preparation.identity,
                            expected_key,
                            *expected_revision,
                            &preparation.proposed_key,
                        )
                        .await
                }
            };
            let submission = match balance_operation_tracker
                .write()
                .confirm_submission(&preparation, persisted)
            {
                Ok(Some(submission)) => submission,
                Ok(None) => return,
                Err(ManualBalanceOperationStorageError::TerminalConfirmationRequired) => {
                    // A durable tombstone proves that this exact operation key
                    // reached a definitive server result in this or another
                    // tab. Require a second, explicit action before rotating
                    // to a fresh financial operation.
                    if balance_user()
                        .as_ref()
                        .is_some_and(|selected| selected.id == preparation.identity.user_id)
                    {
                        balance_error.set(String::new());
                    }
                    return;
                }
                Err(_) => {
                    // No financial request was sent. The tracker has already
                    // left its preparing state, so the administrator can fix
                    // storage support and retry safely.
                    if balance_user()
                        .as_ref()
                        .is_some_and(|selected| selected.id == preparation.identity.user_id)
                    {
                        balance_error.set(
                            i18n.t("users.balance_idempotency_prepare_failed")
                                .to_string(),
                        );
                    }
                    return;
                }
            };
            let idempotency_key = submission.idempotency_key.clone();
            let result = with_auto_refresh(auth_store, move |token| {
                let client = get_client();
                let id = id.clone();
                let req = req.clone();
                let action = action.clone();
                let idempotency_key = idempotency_key.clone();
                async move {
                    match action.as_str() {
                        "recharge" | "deduct" => {
                            AdminApi::new(&client)
                                .update_user_balance(&id, &req, &idempotency_key, &token)
                                .await
                        }
                        "freeze" => {
                            AdminApi::new(&client)
                                .freeze_user_balance(&id, &req, &idempotency_key, &token)
                                .await
                        }
                        "unfreeze" => {
                            AdminApi::new(&client)
                                .unfreeze_user_balance(&id, &req, &idempotency_key, &token)
                                .await
                        }
                        _ => unreachable!("balance action was validated before spawning"),
                    }
                }
            })
            .await;
            let conflict = result.as_ref().err().is_some_and(is_conflict_error);
            let terminal = result.is_ok() || conflict;
            let finalization = if balance_operation_tracker
                .read()
                .should_finalize_binding(&submission, terminal)
            {
                Some(
                    balance_operation_storage
                        .finalize_binding(
                            &submission.scope,
                            &submission.identity,
                            &submission.idempotency_key,
                            submission.binding_revision,
                        )
                        .await,
                )
            } else {
                None
            };
            let finish = balance_operation_tracker.write().complete_submission(
                &submission,
                terminal,
                finalization,
            );
            let owns_selected_user = finish.owns_modal
                && balance_user()
                    .as_ref()
                    .is_some_and(|selected| selected.id == submission.identity.user_id);
            if finish.storage_error.is_some() {
                // The server result is terminal, but its durable tombstone was
                // not verified. Keep the modal/key so a retry remains
                // idempotent and cannot silently mint another operation.
                let message = i18n
                    .t("users.balance_idempotency_cleanup_failed")
                    .to_string();
                if owns_selected_user {
                    balance_error.set(message);
                    balance_details_resource.restart();
                } else {
                    ui_store.show_error(message);
                }
                users_resource.restart();
                return;
            }
            match result {
                Ok(_) => {
                    if owns_selected_user {
                        ui_store.show_success(i18n.t("users.balance_updated"));
                        release_request_id.set(None);
                        release_expected_version.set(None);
                        release_reason.set(String::new());
                        balance_operation_tracker.write().advance_modal();
                        balance_reservation_pagination.write().reset();
                        balance_user.set(None);
                    }
                    users_resource.restart();
                }
                Err(e) => {
                    // Stale completions are deliberately silent: they may
                    // retire their own exact key, but cannot overwrite a newer
                    // modal's error or close it.
                    if owns_selected_user {
                        balance_error.set(format!(
                            "{}: {}",
                            i18n.t("users.balance_update_failed"),
                            user_error_message(&e)
                        ));
                    }
                }
            }
        });
    };

    let on_release_reservation = move |_| {
        if balance_operation_tracker.read().is_active() || release_saving() {
            return;
        }
        let Some(user) = balance_user() else { return };
        let Some(request_id) = release_request_id() else {
            return;
        };
        let Some(expected_version) = release_expected_version() else {
            balance_error.set(i18n.t("users.balance_reservation_changed").to_string());
            balance_reservation_pagination.write().reset();
            balance_details_resource.restart();
            return;
        };
        let reason = release_reason();
        if reason.trim().is_empty() {
            balance_error.set(i18n.t("users.balance_release_reason_required").to_string());
            return;
        }

        let user_id = user.id;
        release_saving.set(true);
        spawn(async move {
            let req = ReleaseBalanceReservationRequest::new(expected_version, reason.trim());
            let result = with_auto_refresh(auth_store, move |token| {
                let client = get_client();
                let user_id = user_id.clone();
                let request_id = request_id.clone();
                let req = req.clone();
                async move {
                    AdminApi::new(&client)
                        .release_user_balance_reservation(&user_id, &request_id, &req, &token)
                        .await
                }
            })
            .await;
            match result {
                Ok(_) => {
                    ui_store.show_success(i18n.t("users.balance_reservation_released"));
                    release_request_id.set(None);
                    release_expected_version.set(None);
                    release_reason.set(String::new());
                    balance_error.set(String::new());
                    balance_reservation_pagination.write().reset();
                    balance_details_resource.restart();
                    users_resource.restart();
                }
                Err(e) => {
                    if is_conflict_error(&e) {
                        balance_error.set(i18n.t("users.balance_reservation_changed").to_string());
                        release_request_id.set(None);
                        release_expected_version.set(None);
                        release_reason.set(String::new());
                        balance_reservation_pagination.write().reset();
                        balance_details_resource.restart();
                        users_resource.restart();
                    } else {
                        balance_error.set(format!(
                            "{}: {}",
                            i18n.t("users.balance_reservation_release_failed"),
                            user_error_message(&e)
                        ));
                    }
                }
            }
            release_saving.set(false);
        });
    };

    let edit_save_label = if edit_saving() {
        i18n.t("form.saving")
    } else {
        i18n.t("form.save")
    };
    let delete_button_label = if delete_saving() {
        i18n.t("users.deleting")
    } else {
        i18n.t("users.confirm_delete")
    };
    let can_edit_selected_role = edit_user()
        .as_ref()
        .map(|u| can_current_user_manage_roles && u.id != current_user_id && u.role != "system")
        .unwrap_or(false);
    let balance_saving = balance_operation_tracker.read().is_active();
    let balance_repeat_confirmation = balance_operation_tracker.read().has_terminal_confirmation();
    let balance_modal_busy = balance_saving || release_saving();
    let balance_save_label = if balance_saving {
        i18n.t("form.saving")
    } else if balance_repeat_confirmation {
        i18n.t("users.balance_repeat_operation_confirm")
    } else {
        i18n.t("form.confirm")
    };
    let displayed_balance_details = current_balance_details();
    let fmt_balance = |v: f64| crate::utils::format_money(v);

    rsx! {
        div { class: "page-container users-page",
        PageHeader {
            title: i18n.t("page.users").to_string(),
            description: i18n.t("users.subtitle").to_string(),
        }

        div { class: "toolbar",
            div { class: "toolbar-left",
                div { class: "input-wrapper search-input-wrapper",
                    input {
                        class: "input-field",
                        r#type: "search",
                        aria_label: i18n.t("users.search_placeholder"),
                        placeholder: "{i18n.t(\"users.search_placeholder\")}",
                        value: "{search}",
                        oninput: move |e| {
                            *search.write() = e.value();
                        },
                    }
                    if !search().is_empty() {
                        button {
                            class: "btn btn-ghost btn-sm input-clear-button",
                            r#type: "button",
                            aria_label: i18n.t("common.clear"),
                            onclick: move |_| {
                                search.set(String::new());
                                query.write().commit_search(String::new());
                            },
                            "×"
                        }
                    }
                }
            }
        }

        div { class: "card",
            {
                let (is_empty, empty_text) = match users_resource() {
                    None => (true, i18n.t("table.loading")),
                    Some(Err(_)) => (true, i18n.t("common.load_failed")),
                    Some(Ok(_)) if paged_users().is_empty() => (true, i18n.t("users.empty")),
                    _ => (false, ""),
                };
                rsx! {
                    Table {
                        empty: is_empty,
                        empty_text: empty_text.to_string(),
                        col_count: 6,
                        thead {
                            tr {
                                TableHead { {i18n.t("users.user")} }
                                TableHead { {i18n.t("table.role")} }
                                TableHead { {i18n.t("users.tenant")} }
                                TableHead { {i18n.t("users.balance")} }
                                TableHead { {i18n.t("users.registered_at")} }
                                TableHead { {i18n.t("table.actions")} }
                            }
                        }
                        tbody {
                            for u in paged_users().iter() {
                                tr {
                                    td {
                                        div { class: "user-cell",
                                            span { class: "user-name",
                                                { u.name.clone().unwrap_or_else(|| u.email.clone()) }
                                            }
                                            span { class: "user-email text-secondary", "{u.email}" }
                                        }
                                    }
                                    td {
                                        Badge { variant: BadgeVariant::Info, {user_role_label(&u.role, &i18n)} }
                                    }
                                    td { code { title: "{u.tenant_id}", {short_id(&u.tenant_id)} } }
                                    td {
                                        div { class: "balance-cell",
                                            span { class: "balance-available",
                                                "{fmt_balance(u.balance)}"
                                            }
                                            if u.frozen_balance > 0.0 {
                                                span { class: "balance-frozen text-secondary",
                                                    "({i18n.t(\"users.frozen_short\")} {fmt_balance(u.frozen_balance)})"
                                                }
                                            }
                                        }
                                    }
                                    td { { format_time(&u.created_at) } }
                                    td {
                                        div { class: "btn-group",
                                            // 仅 system 角色可编辑 system 用户；admin 可编辑其他用户
                                            if u.role != UserRole::System.as_str() || can_current_user_manage_roles {
                                                Button {
                                                    variant: ButtonVariant::Ghost,
                                                    size: ButtonSize::Small,
                                                    onclick: {
                                                        let uu = u.clone();
                                                        move |_| {
                                                            edit_name.set(uu.name.clone().unwrap_or_default());
                                                            edit_role.set(uu.role.clone());
                                                            edit_user.set(Some(uu.clone()));
                                                        }
                                                    },
                                                    {i18n.t("form.edit")}
                                                }
                                            }
                                            // 仅 system 角色可管理 system 用户的余额；admin 可管理其他用户
                                            if u.role != UserRole::System.as_str() || can_current_user_manage_roles {
                                                Button {
                                                    variant: ButtonVariant::Ghost,
                                                    size: ButtonSize::Small,
                                                    onclick: {
                                                        let uu = u.clone();
                                                        move |_| {
                                                            if balance_operation_tracker
                                                                .read()
                                                                .is_active()
                                                                || release_saving()
                                                            {
                                                                return;
                                                            }
                                                            balance_action.set("recharge".to_string());
                                                            balance_amount.set(String::new());
                                                            balance_reason.set(String::new());
                                                            balance_error.set(String::new());
                                                            release_request_id.set(None);
                                                            release_expected_version.set(None);
                                                            release_reason.set(String::new());
                                                            balance_operation_tracker.write().advance_modal();
                                                            balance_reservation_pagination.write().reset();
                                                            balance_user.set(Some(uu.clone()));
                                                        }
                                                    },
                                                    {i18n.t("users.balance_manage")}
                                                }
                                            }
                                            if u.id != current_user_id
                                                && u.role != UserRole::System.as_str()
                                                && (u.role != UserRole::Admin.as_str() || can_current_user_manage_roles) {
                                                Button {
                                                    variant: ButtonVariant::Danger,
                                                    size: ButtonSize::Small,
                                                    onclick: {
                                                        let uu = u.clone();
                                                        move |_| delete_user.set(Some(uu.clone()))
                                                    },
                                                    {i18n.t("form.delete")}
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        div { class: "pagination",
            span { class: "pagination-info",
                "{i18n.t(\"common.total_items\")} {total_items()} {i18n.t(\"pricing.items_suffix\")}"
            }
            Pagination {
                current: query().page,
                total_pages: total_pages(),
                previous_label: i18n.t("table.previous").to_string(),
                next_label: i18n.t("table.next").to_string(),
                on_page_change: move |p| query.write().page = p,
            }
        }

        // ── 编辑用户弹窗 ──────────────────────────────────────────
        if edit_user().is_some() {
            div { class: "modal-backdrop",
                onclick: move |_| edit_user.set(None),
                div {
                    class: "modal",
                    role: "dialog",
                    aria_modal: "true",
                    aria_label: i18n.t("users.edit_title"),
                    onclick: move |e| e.stop_propagation(),
                    div { class: "modal-header",
                        h2 { class: "modal-title", {i18n.t("users.edit_title")} }
                        button {
                            class: "btn btn-ghost btn-sm",
                            r#type: "button",
                            aria_label: i18n.t("common.close"),
                            onclick: move |_| edit_user.set(None),
                            "✕"
                        }
                    }
                    div { class: "modal-body",
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("users.display_name")} }
                            input {
                                class: "input-field",
                                placeholder: "{i18n.t(\"users.display_name_placeholder\")}",
                                value: "{edit_name}",
                                oninput: move |e| *edit_name.write() = e.value(),
                            }
                        }
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("table.role")} }
                            if can_edit_selected_role {
                                select {
                                    class: "input-field",
                                    value: "{edit_role}",
                                    onchange: move |e| *edit_role.write() = e.value(),
                                    option { value: "user", "{i18n.t(\"users.role_user\")}" }
                                    option { value: "admin", "{i18n.t(\"users.role_admin\")}" }
                                }
                            } else {
                                input {
                                    class: "input-field",
                                    value: "{edit_role}",
                                    readonly: true,
                                }
                            }
                        }
                    }
                    div { class: "modal-footer",
                        Button {
                            variant: ButtonVariant::Ghost,
                            onclick: move |_| edit_user.set(None),
                            {i18n.t("form.cancel")}
                        }
                        Button {
                            variant: ButtonVariant::Primary,
                            loading: edit_saving(),
                            onclick: on_edit_save,
                            "{edit_save_label}"
                        }
                    }
                }
            }
        }

        // ── 删除确认弹窗 ──────────────────────────────────────────
        if let Some(ref du) = delete_user() {
            div { class: "modal-backdrop",
                onclick: move |_| delete_user.set(None),
                div {
                    class: "modal",
                    role: "alertdialog",
                    aria_modal: "true",
                    aria_label: i18n.t("users.delete_confirm_title"),
                    onclick: move |e| e.stop_propagation(),
                    div { class: "modal-header",
                        h2 { class: "modal-title", {i18n.t("users.delete_confirm_title")} }
                    }
                    div { class: "modal-body",
                        p {
                            "{i18n.t(\"users.delete_confirm_prefix\")} "
                            strong { { du.name.clone().unwrap_or_else(|| du.email.clone()) } }
                            " ({du.email}) {i18n.t(\"users.delete_confirm_suffix\")}"
                        }
                    }
                    div { class: "modal-footer",
                        Button {
                            variant: ButtonVariant::Ghost,
                            onclick: move |_| delete_user.set(None),
                            {i18n.t("form.cancel")}
                        }
                        Button {
                            variant: ButtonVariant::Danger,
                            loading: delete_saving(),
                            onclick: on_delete_confirm,
                            "{delete_button_label}"
                        }
                    }
                }
            }
        }

        // ── 余额管理弹窗 ──────────────────────────────────────────
        if let Some(ref bu) = balance_user() {
            div { class: "modal-backdrop",
                onclick: move |_| {
                    if balance_operation_tracker.read().is_active() || release_saving() {
                        return;
                    }
                    balance_error.set(String::new());
                    release_request_id.set(None);
                    release_expected_version.set(None);
                    release_reason.set(String::new());
                    balance_operation_tracker.write().advance_modal();
                    balance_reservation_pagination.write().reset();
                    balance_user.set(None);
                },
                div {
                    class: "modal",
                    role: "dialog",
                    aria_modal: "true",
                    aria_label: i18n.t("users.balance_title"),
                    onclick: move |e| e.stop_propagation(),
                    div { class: "modal-header",
                        h2 { class: "modal-title",
                            "{i18n.t(\"users.balance_title\")} - {bu.name.clone().unwrap_or_else(|| bu.email.clone())}"
                        }
                        button {
                            class: "btn btn-ghost btn-sm",
                            r#type: "button",
                            aria_label: i18n.t("common.close"),
                            disabled: balance_modal_busy,
                            onclick: move |_| {
                                if balance_operation_tracker.read().is_active() || release_saving() {
                                    return;
                                }
                                balance_error.set(String::new());
                                release_request_id.set(None);
                                release_expected_version.set(None);
                                release_reason.set(String::new());
                                balance_operation_tracker.write().advance_modal();
                                balance_reservation_pagination.write().reset();
                                balance_user.set(None);
                            },
                            "✕"
                        }
                    }
                    div { class: "modal-body",
                        // 弹窗内联错误提示
                        if !balance_error().is_empty() {
                            div { class: "modal-inline-error", "{balance_error}" }
                        }
                        if balance_repeat_confirmation {
                            div { class: "alert alert-warning",
                                {i18n.t("users.balance_repeat_operation_warning")}
                            }
                        }
                        // 每次打开弹窗都实时读取余额拆分，避免把总冻结误认为可人工解冻余额。
                        match displayed_balance_details {
                            None => rsx! {
                                p { class: "text-secondary", {i18n.t("table.loading")} }
                            },
                            Some(Err(ref e)) => {
                                let message = user_error_message(e);
                                let reservation_page_number =
                                    balance_reservation_pagination.read().page_number();
                                let can_go_back =
                                    balance_reservation_pagination.read().can_go_back();
                                rsx! {
                                    div { class: "alert alert-warning",
                                        "{i18n.t(\"users.balance_details_load_failed\")}: {message}"
                                    }
                                    if can_go_back {
                                        div { class: "pagination", style: "margin-top: 12px;",
                                            Button {
                                                variant: ButtonVariant::Ghost,
                                                size: ButtonSize::Small,
                                                disabled: balance_modal_busy,
                                                onclick: move |_| {
                                                    release_request_id.set(None);
                                                    release_expected_version.set(None);
                                                    release_reason.set(String::new());
                                                    balance_error.set(String::new());
                                                    balance_reservation_pagination
                                                        .write()
                                                        .go_back(reservation_page_number);
                                                },
                                                {i18n.t("users.balance_reservations_previous_page")}
                                            }
                                        }
                                    }
                                }
                            },
                            Some(Ok(ref details)) => {
                                let available = crate::utils::format_precise_money_str(
                                    &details.available_balance,
                                );
                                let total_frozen = crate::utils::format_precise_money_str(
                                    &details.total_frozen_balance,
                                );
                                let request_reserved = crate::utils::format_precise_money_str(
                                    &details.request_reserved_balance,
                                );
                                let manually_frozen = crate::utils::format_precise_money_str(
                                    &details.manually_frozen_balance,
                                );
                                let reservation_page_number =
                                    balance_reservation_pagination.read().page_number();
                                let can_go_back =
                                    balance_reservation_pagination.read().can_go_back();
                                let next_cursor = details.next_cursor.clone();
                                rsx! {
                                    div { class: "balance-info",
                                        div { class: "balance-row",
                                            span { class: "balance-label", {i18n.t("users.balance_available")} }
                                            span { class: "balance-value", "{available}" }
                                        }
                                        div { class: "balance-row",
                                            span { class: "balance-label", {i18n.t("users.balance_total_frozen")} }
                                            span { class: "balance-value", "{total_frozen}" }
                                        }
                                        div { class: "balance-row",
                                            span { class: "balance-label", {i18n.t("users.balance_request_reserved")} }
                                            span { class: "balance-value", "{request_reserved}" }
                                        }
                                        div { class: "balance-row",
                                            span { class: "balance-label", {i18n.t("users.balance_manually_frozen")} }
                                            span { class: "balance-value", "{manually_frozen}" }
                                        }
                                    }

                                    if !details.reservations.is_empty() {
                                        div {
                                            style: "margin: 16px 0; display: flex; flex-direction: column; gap: 10px;",
                                            h3 { style: "margin: 0; font-size: 14px;",
                                                {i18n.t("users.balance_active_reservations")}
                                            }
                                            div { class: "alert alert-warning",
                                                {i18n.t("users.balance_release_warning")}
                                            }
                                            for reservation in details.reservations.iter() {
                                                div {
                                                    key: "{reservation.request_id}",
                                                    style: "border: 1px solid var(--border-color); border-radius: 8px; padding: 10px; display: flex; flex-direction: column; gap: 8px;",
                                                    div { style: "display: flex; justify-content: space-between; gap: 12px; align-items: center;",
                                                        div { style: "min-width: 0;",
                                                            code {
                                                                title: "{reservation.request_id}",
                                                                "{short_id(&reservation.request_id)}"
                                                            }
                                                            div { class: "text-secondary", style: "font-size: 12px;",
                                                                "{i18n.t(\"users.balance_reservation_expires\")}: {format_time(&reservation.expires_at)}"
                                                            }
                                                        }
                                                        div { style: "display: flex; gap: 8px; align-items: center;",
                                                            span { class: "balance-value",
                                                                "{crate::utils::format_precise_money_str(&reservation.amount)}"
                                                            }
                                                            Button {
                                                                variant: ButtonVariant::Danger,
                                                                size: ButtonSize::Small,
                                                                disabled: balance_modal_busy,
                                                                onclick: {
                                                                    let request_id = reservation.request_id.clone();
                                                                    let version = reservation.version.clone();
                                                                    move |_| {
                                                                        release_request_id.set(Some(request_id.clone()));
                                                                        release_expected_version.set(Some(version.clone()));
                                                                        release_reason.set(String::new());
                                                                        balance_error.set(String::new());
                                                                    }
                                                                },
                                                                {i18n.t("users.balance_release_reservation")}
                                                            }
                                                        }
                                                    }
                                                    if release_request_id().as_deref()
                                                        == Some(reservation.request_id.as_str())
                                                    {
                                                        div { class: "form-group", style: "margin: 0;",
                                                            label { class: "form-label",
                                                                {i18n.t("users.balance_release_reason")}
                                                                span { class: "required-mark", " *" }
                                                            }
                                                            input {
                                                                class: "input-field",
                                                                maxlength: "1000",
                                                                placeholder: "{i18n.t(\"users.balance_release_reason_placeholder\")}",
                                                                value: "{release_reason}",
                                                                oninput: move |e| {
                                                                    *release_reason.write() = e.value();
                                                                    balance_error.set(String::new());
                                                                },
                                                            }
                                                            div { style: "display: flex; justify-content: flex-end; gap: 8px; margin-top: 8px;",
                                                                Button {
                                                                    variant: ButtonVariant::Ghost,
                                                                    size: ButtonSize::Small,
                                                                    disabled: balance_modal_busy,
                                                                    onclick: move |_| {
                                                                        release_request_id.set(None);
                                                                        release_expected_version.set(None);
                                                                        release_reason.set(String::new());
                                                                    },
                                                                    {i18n.t("form.cancel")}
                                                                }
                                                                Button {
                                                                    variant: ButtonVariant::Danger,
                                                                    size: ButtonSize::Small,
                                                                    loading: release_saving(),
                                                                    onclick: on_release_reservation,
                                                                    {i18n.t("users.balance_confirm_release")}
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    if can_go_back || next_cursor.is_some() {
                                        div {
                                            class: "pagination",
                                            style: "margin-top: 12px;",
                                            Button {
                                                variant: ButtonVariant::Ghost,
                                                size: ButtonSize::Small,
                                                disabled: balance_modal_busy || !can_go_back,
                                                onclick: move |_| {
                                                    release_request_id.set(None);
                                                    release_expected_version.set(None);
                                                    release_reason.set(String::new());
                                                    balance_error.set(String::new());
                                                    balance_reservation_pagination
                                                        .write()
                                                        .go_back(reservation_page_number);
                                                },
                                                {i18n.t("users.balance_reservations_previous_page")}
                                            }
                                            span { class: "pagination-info",
                                                {i18n.t_with_args(
                                                    "users.balance_reservations_page",
                                                    &[("page", &reservation_page_number.to_string())],
                                                )}
                                            }
                                            if let Some(next_cursor) = next_cursor {
                                                Button {
                                                    variant: ButtonVariant::Ghost,
                                                    size: ButtonSize::Small,
                                                    disabled: balance_modal_busy,
                                                    onclick: move |_| {
                                                        release_request_id.set(None);
                                                        release_expected_version.set(None);
                                                        release_reason.set(String::new());
                                                        balance_error.set(String::new());
                                                        balance_reservation_pagination
                                                            .write()
                                                            .advance(
                                                                reservation_page_number,
                                                                next_cursor.clone(),
                                                            );
                                                    },
                                                    {i18n.t("users.balance_reservations_next_page")}
                                                }
                                            }
                                        }
                                    }
                                }
                            },
                        }
                        // 操作选择
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("users.balance_action")} }
                            select {
                                class: "input-field",
                                disabled: balance_modal_busy,
                                value: "{balance_action}",
                                onchange: move |e| {
                                    *balance_action.write() = e.value();
                                    balance_operation_tracker
                                        .write()
                                        .clear_terminal_confirmation();
                                    balance_error.set(String::new());
                                },
                                option { value: "recharge", {i18n.t("users.balance_recharge")} }
                                option { value: "deduct", {i18n.t("users.balance_deduct")} }
                                option { value: "freeze", {i18n.t("users.balance_freeze")} }
                                option { value: "unfreeze", {i18n.t("users.balance_unfreeze")} }
                            }
                            if balance_action() == "unfreeze" {
                                p { class: "text-secondary", style: "margin: 6px 0 0; font-size: 12px;",
                                    {i18n.t("users.balance_unfreeze_hint")}
                                }
                            }
                        }
                        // 金额输入
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("users.balance_amount")} }
                            input {
                                class: "input-field",
                                r#type: "number",
                                step: "0.01",
                                min: "0",
                                disabled: balance_modal_busy,
                                placeholder: "{i18n.t(\"users.balance_amount_placeholder\")}",
                                value: "{balance_amount}",
                                oninput: move |e| {
                                    *balance_amount.write() = e.value();
                                    balance_operation_tracker
                                        .write()
                                        .clear_terminal_confirmation();
                                    balance_error.set(String::new());
                                },
                            }
                        }
                        // 原因输入
                        div { class: "form-group",
                            label { class: "form-label",
                                {i18n.t("users.balance_reason")}
                                span { class: "required-mark", " *" }
                            }
                            input {
                                class: "input-field",
                                disabled: balance_modal_busy,
                                placeholder: "{i18n.t(\"users.balance_reason_placeholder\")}",
                                maxlength: "1000",
                                value: "{balance_reason}",
                                oninput: move |e| {
                                    *balance_reason.write() = e.value();
                                    balance_operation_tracker
                                        .write()
                                        .clear_terminal_confirmation();
                                    balance_error.set(String::new());
                                },
                            }
                        }
                    }
                    div { class: "modal-footer",
                        Button {
                            variant: ButtonVariant::Ghost,
                            disabled: balance_modal_busy,
                            onclick: move |_| {
                                if balance_operation_tracker.read().is_active() || release_saving() {
                                    return;
                                }
                                release_request_id.set(None);
                                release_expected_version.set(None);
                                release_reason.set(String::new());
                                balance_operation_tracker.write().advance_modal();
                                balance_reservation_pagination.write().reset();
                                balance_user.set(None);
                            },
                            {i18n.t("form.cancel")}
                        }
                        Button {
                            variant: if balance_repeat_confirmation {
                                ButtonVariant::Danger
                            } else {
                                ButtonVariant::Primary
                            },
                            disabled: release_saving(),
                            loading: balance_saving,
                            onclick: on_balance_save,
                            "{balance_save_label}"
                        }
                    }
                }
            }
        }
        }
    }
}

#[cfg(test)]
mod search_tests {
    use super::{
        BalanceDetailsLoadResult, BalanceReservationPagination, ManualBalanceBindingState,
        ManualBalanceOperationIdentity, ManualBalanceOperationPersistence,
        ManualBalanceOperationScope, ManualBalanceOperationStorageError,
        ManualBalanceOperationTracker, PersistedManualBalanceBinding, SEARCH_DEBOUNCE_MS,
        UserListQuery, current_balance_details_result, finalize_manual_balance_binding_in_snapshot,
        is_conflict_error, manual_balance_binding_from_snapshot,
        manual_balance_operation_api_namespace, normalize_manual_balance_reason,
        persist_manual_balance_binding_in_snapshot, rotate_manual_balance_binding_in_snapshot,
    };
    use crate::utils::resource::KeyedResourceValue;
    use client_api::{
        ClientConfig, ClientError,
        api::admin::{UpdateBalanceRequest, UserBalanceReservationsResponse},
    };
    use dioxus::prelude::UseResourceState;
    use std::{
        cell::RefCell,
        collections::HashMap,
        sync::{Arc, Barrier, Mutex},
    };

    const TEST_KEY_1: &str = "00000000-0000-4000-8000-000000000001";
    const TEST_KEY_2: &str = "00000000-0000-4000-8000-000000000002";
    const TEST_KEY_3: &str = "00000000-0000-4000-8000-000000000003";

    #[derive(Default)]
    struct MemoryBalanceOperationStorage {
        entries: RefCell<HashMap<String, String>>,
    }

    impl MemoryBalanceOperationStorage {
        fn raw_for(
            &self,
            scope: &ManualBalanceOperationScope,
            identity: &ManualBalanceOperationIdentity,
        ) -> Option<String> {
            let key = scope.storage_key(identity).ok()?;
            self.entries.borrow().get(&key).cloned()
        }

        fn load_binding(
            &self,
            scope: &ManualBalanceOperationScope,
            identity: &ManualBalanceOperationIdentity,
        ) -> Result<Option<PersistedManualBalanceBinding>, ManualBalanceOperationStorageError>
        {
            let storage_key = scope.storage_key(identity)?;
            let entries = self.entries.borrow();
            manual_balance_binding_from_snapshot(entries.get(&storage_key).map(String::as_str))
        }

        fn load_key(
            &self,
            scope: &ManualBalanceOperationScope,
            identity: &ManualBalanceOperationIdentity,
        ) -> Result<Option<String>, ManualBalanceOperationStorageError> {
            self.load_binding(scope, identity)
                .map(|binding| binding.map(|binding| binding.idempotency_key))
        }
    }

    impl ManualBalanceOperationPersistence for MemoryBalanceOperationStorage {
        async fn persist_binding(
            &self,
            scope: &ManualBalanceOperationScope,
            identity: &ManualBalanceOperationIdentity,
            proposed_key: &str,
        ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError> {
            let storage_key = scope.storage_key(identity)?;
            let raw = self.entries.borrow().get(&storage_key).cloned();
            let (binding, updated) =
                persist_manual_balance_binding_in_snapshot(raw.as_deref(), proposed_key)?;
            self.entries
                .borrow_mut()
                .insert(storage_key.clone(), updated);
            let verified = self.entries.borrow();
            if manual_balance_binding_from_snapshot(verified.get(&storage_key).map(String::as_str))?
                .as_ref()
                != Some(&binding)
            {
                return Err(ManualBalanceOperationStorageError::VerificationFailed);
            }
            Ok(binding)
        }

        async fn finalize_binding(
            &self,
            scope: &ManualBalanceOperationScope,
            identity: &ManualBalanceOperationIdentity,
            expected_key: &str,
            expected_revision: u64,
        ) -> Result<(), ManualBalanceOperationStorageError> {
            let storage_key = scope.storage_key(identity)?;
            let raw = self.entries.borrow().get(&storage_key).cloned();
            let (binding, updated) = finalize_manual_balance_binding_in_snapshot(
                raw.as_deref(),
                expected_key,
                expected_revision,
            )?;
            self.entries
                .borrow_mut()
                .insert(storage_key.clone(), updated);
            if self.load_binding(scope, identity)?.as_ref() != Some(&binding) {
                return Err(ManualBalanceOperationStorageError::VerificationFailed);
            }
            Ok(())
        }

        async fn rotate_binding(
            &self,
            scope: &ManualBalanceOperationScope,
            identity: &ManualBalanceOperationIdentity,
            expected_key: &str,
            expected_revision: u64,
            proposed_key: &str,
        ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError> {
            let storage_key = scope.storage_key(identity)?;
            let raw = self.entries.borrow().get(&storage_key).cloned();
            let (binding, updated) = rotate_manual_balance_binding_in_snapshot(
                raw.as_deref(),
                expected_key,
                expected_revision,
                proposed_key,
            )?;
            self.entries
                .borrow_mut()
                .insert(storage_key.clone(), updated);
            if self.load_binding(scope, identity)?.as_ref() != Some(&binding) {
                return Err(ManualBalanceOperationStorageError::VerificationFailed);
            }
            Ok(binding)
        }
    }

    #[derive(Default)]
    struct LockedMemoryBalanceOperationStorage {
        entries: Mutex<HashMap<String, String>>,
    }

    impl ManualBalanceOperationPersistence for LockedMemoryBalanceOperationStorage {
        async fn persist_binding(
            &self,
            scope: &ManualBalanceOperationScope,
            identity: &ManualBalanceOperationIdentity,
            proposed_key: &str,
        ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError> {
            let storage_key = scope.storage_key(identity)?;
            let mut entries = self.entries.lock().expect("test storage mutex poisoned");
            let (durable_key, updated) = persist_manual_balance_binding_in_snapshot(
                entries.get(&storage_key).map(String::as_str),
                proposed_key,
            )?;
            entries.insert(storage_key, updated);
            Ok(durable_key)
        }

        async fn finalize_binding(
            &self,
            scope: &ManualBalanceOperationScope,
            identity: &ManualBalanceOperationIdentity,
            expected_key: &str,
            expected_revision: u64,
        ) -> Result<(), ManualBalanceOperationStorageError> {
            let storage_key = scope.storage_key(identity)?;
            let mut entries = self.entries.lock().expect("test storage mutex poisoned");
            let (_, updated) = finalize_manual_balance_binding_in_snapshot(
                entries.get(&storage_key).map(String::as_str),
                expected_key,
                expected_revision,
            )?;
            entries.insert(storage_key, updated);
            Ok(())
        }

        async fn rotate_binding(
            &self,
            scope: &ManualBalanceOperationScope,
            identity: &ManualBalanceOperationIdentity,
            expected_key: &str,
            expected_revision: u64,
            proposed_key: &str,
        ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError> {
            let storage_key = scope.storage_key(identity)?;
            let mut entries = self.entries.lock().expect("test storage mutex poisoned");
            let (binding, updated) = rotate_manual_balance_binding_in_snapshot(
                entries.get(&storage_key).map(String::as_str),
                expected_key,
                expected_revision,
                proposed_key,
            )?;
            entries.insert(storage_key, updated);
            Ok(binding)
        }
    }

    struct FailingBalanceOperationStorage;

    impl ManualBalanceOperationPersistence for FailingBalanceOperationStorage {
        async fn persist_binding(
            &self,
            _scope: &ManualBalanceOperationScope,
            _identity: &ManualBalanceOperationIdentity,
            _proposed_key: &str,
        ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError> {
            Err(ManualBalanceOperationStorageError::ReadFailed)
        }

        async fn finalize_binding(
            &self,
            _scope: &ManualBalanceOperationScope,
            _identity: &ManualBalanceOperationIdentity,
            _expected_key: &str,
            _expected_revision: u64,
        ) -> Result<(), ManualBalanceOperationStorageError> {
            Err(ManualBalanceOperationStorageError::WriteFailed)
        }

        async fn rotate_binding(
            &self,
            _scope: &ManualBalanceOperationScope,
            _identity: &ManualBalanceOperationIdentity,
            _expected_key: &str,
            _expected_revision: u64,
            _proposed_key: &str,
        ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError> {
            Err(ManualBalanceOperationStorageError::WriteFailed)
        }
    }

    struct WriteFailingBalanceOperationStorage;

    impl ManualBalanceOperationPersistence for WriteFailingBalanceOperationStorage {
        async fn persist_binding(
            &self,
            _scope: &ManualBalanceOperationScope,
            _identity: &ManualBalanceOperationIdentity,
            _proposed_key: &str,
        ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError> {
            Err(ManualBalanceOperationStorageError::WriteFailed)
        }

        async fn finalize_binding(
            &self,
            _scope: &ManualBalanceOperationScope,
            _identity: &ManualBalanceOperationIdentity,
            _expected_key: &str,
            _expected_revision: u64,
        ) -> Result<(), ManualBalanceOperationStorageError> {
            panic!("a request blocked before submission cannot reach terminal cleanup")
        }

        async fn rotate_binding(
            &self,
            _scope: &ManualBalanceOperationScope,
            _identity: &ManualBalanceOperationIdentity,
            _expected_key: &str,
            _expected_revision: u64,
            _proposed_key: &str,
        ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError> {
            Err(ManualBalanceOperationStorageError::WriteFailed)
        }
    }

    struct FinalizationFailingBalanceOperationStorage<'a>(&'a MemoryBalanceOperationStorage);

    impl ManualBalanceOperationPersistence for FinalizationFailingBalanceOperationStorage<'_> {
        async fn persist_binding(
            &self,
            scope: &ManualBalanceOperationScope,
            identity: &ManualBalanceOperationIdentity,
            proposed_key: &str,
        ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError> {
            self.0.persist_binding(scope, identity, proposed_key).await
        }

        async fn finalize_binding(
            &self,
            _scope: &ManualBalanceOperationScope,
            _identity: &ManualBalanceOperationIdentity,
            _expected_key: &str,
            _expected_revision: u64,
        ) -> Result<(), ManualBalanceOperationStorageError> {
            Err(ManualBalanceOperationStorageError::WriteFailed)
        }

        async fn rotate_binding(
            &self,
            scope: &ManualBalanceOperationScope,
            identity: &ManualBalanceOperationIdentity,
            expected_key: &str,
            expected_revision: u64,
            proposed_key: &str,
        ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError> {
            self.0
                .rotate_binding(
                    scope,
                    identity,
                    expected_key,
                    expected_revision,
                    proposed_key,
                )
                .await
        }
    }

    struct RotationFailingBalanceOperationStorage<'a>(&'a MemoryBalanceOperationStorage);

    impl ManualBalanceOperationPersistence for RotationFailingBalanceOperationStorage<'_> {
        async fn persist_binding(
            &self,
            scope: &ManualBalanceOperationScope,
            identity: &ManualBalanceOperationIdentity,
            proposed_key: &str,
        ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError> {
            self.0.persist_binding(scope, identity, proposed_key).await
        }

        async fn finalize_binding(
            &self,
            scope: &ManualBalanceOperationScope,
            identity: &ManualBalanceOperationIdentity,
            expected_key: &str,
            expected_revision: u64,
        ) -> Result<(), ManualBalanceOperationStorageError> {
            self.0
                .finalize_binding(scope, identity, expected_key, expected_revision)
                .await
        }

        async fn rotate_binding(
            &self,
            _scope: &ManualBalanceOperationScope,
            _identity: &ManualBalanceOperationIdentity,
            _expected_key: &str,
            _expected_revision: u64,
            _proposed_key: &str,
        ) -> Result<PersistedManualBalanceBinding, ManualBalanceOperationStorageError> {
            Err(ManualBalanceOperationStorageError::WriteFailed)
        }
    }

    fn balance_details(user_id: &str) -> UserBalanceReservationsResponse {
        UserBalanceReservationsResponse {
            user_id: user_id.to_string(),
            available_balance: "10.00".to_string(),
            total_frozen_balance: "2.00".to_string(),
            request_reserved_balance: "1.00".to_string(),
            manually_frozen_balance: "1.00".to_string(),
            reservations: Vec::new(),
            next_cursor: None,
        }
    }

    fn loaded_balance_details(
        request_user_id: &str,
        response_user_id: &str,
    ) -> KeyedResourceValue<(Option<String>, Option<String>), BalanceDetailsLoadResult> {
        KeyedResourceValue::new(
            (Some(request_user_id.to_string()), None),
            Ok(Some(balance_details(response_user_id))),
        )
    }

    fn operation_scope(
        api_root: &str,
        administrator_user_id: &str,
        administrator_tenant_id: &str,
    ) -> ManualBalanceOperationScope {
        ManualBalanceOperationScope::new(api_root, administrator_user_id, administrator_tenant_id)
    }

    #[test]
    fn balance_retry_scope_uses_the_exact_admin_api_prefix() {
        let root = ClientConfig::new("https://api.example.com");
        let versioned = ClientConfig::new("https://api.example.com/v1");
        let auth_prefixed = ClientConfig::new("https://api.example.com/auth");

        assert_eq!(
            manual_balance_operation_api_namespace(&root),
            "https://api.example.com/api/v1"
        );
        assert_ne!(
            manual_balance_operation_api_namespace(&root),
            manual_balance_operation_api_namespace(&versioned)
        );
        assert_ne!(
            manual_balance_operation_api_namespace(&root),
            manual_balance_operation_api_namespace(&auth_prefixed)
        );
    }

    #[test]
    fn simultaneous_first_submissions_converge_on_one_durable_key() {
        let scope = operation_scope("https://api.example.com/api/v1", "admin-1", "tenant-1");
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "freeze",
            &UpdateBalanceRequest::new(2.0, "incident hold"),
        );
        let storage = Arc::new(LockedMemoryBalanceOperationStorage::default());
        let start = Arc::new(Barrier::new(3));

        let spawn_submission = |candidate: &'static str| {
            let scope = scope.clone();
            let identity = identity.clone();
            let storage = Arc::clone(&storage);
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                let mut tracker = ManualBalanceOperationTracker::new(scope);
                start.wait();
                tracker
                    .begin_submission(identity, || candidate.to_string(), storage.as_ref())
                    .expect("locked persistence should succeed")
                    .expect("submission should become active")
                    .idempotency_key
            })
        };
        let first = spawn_submission(TEST_KEY_1);
        let second = spawn_submission(TEST_KEY_2);
        start.wait();

        assert_eq!(
            first.join().expect("first tab panicked"),
            second.join().expect("second tab panicked")
        );
    }

    #[test]
    fn concurrent_explicit_rotations_create_only_one_successor_generation() {
        let scope = operation_scope("https://api.example.com/api/v1", "admin-1", "tenant-1");
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "freeze",
            &UpdateBalanceRequest::new(2.0, "incident hold"),
        );
        let storage = Arc::new(LockedMemoryBalanceOperationStorage::default());
        let mut seed = ManualBalanceOperationTracker::new(scope.clone());
        let initial = seed
            .begin_submission(
                identity.clone(),
                || TEST_KEY_1.to_string(),
                storage.as_ref(),
            )
            .unwrap()
            .unwrap();
        seed.finish_submission(&initial, true, storage.as_ref());

        let mut first_tab = ManualBalanceOperationTracker::new(scope.clone());
        assert_eq!(
            first_tab
                .begin_submission(
                    identity.clone(),
                    || TEST_KEY_2.to_string(),
                    storage.as_ref(),
                )
                .unwrap_err(),
            ManualBalanceOperationStorageError::TerminalConfirmationRequired
        );
        let mut second_tab = ManualBalanceOperationTracker::new(scope.clone());
        assert_eq!(
            second_tab
                .begin_submission(
                    identity.clone(),
                    || TEST_KEY_3.to_string(),
                    storage.as_ref(),
                )
                .unwrap_err(),
            ManualBalanceOperationStorageError::TerminalConfirmationRequired
        );
        let start = Arc::new(Barrier::new(3));
        let spawn_rotation = |mut tracker: ManualBalanceOperationTracker,
                              candidate: &'static str| {
            let identity = identity.clone();
            let storage = Arc::clone(&storage);
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                tracker
                    .begin_submission(identity, || candidate.to_string(), storage.as_ref())
                    .map(|submission| submission.map(|submission| submission.idempotency_key))
            })
        };
        let first = spawn_rotation(first_tab, TEST_KEY_2);
        let second = spawn_rotation(second_tab, TEST_KEY_3);
        start.wait();
        let results = [
            first.join().expect("first tab panicked"),
            second.join().expect("second tab panicked"),
        ];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| {
                    matches!(
                        result,
                        Err(ManualBalanceOperationStorageError::BindingChanged)
                    )
                })
                .count(),
            1
        );

        let storage_key = scope.storage_key(&identity).unwrap();
        let entries = storage.entries.lock().expect("test storage mutex poisoned");
        let binding =
            manual_balance_binding_from_snapshot(entries.get(&storage_key).map(String::as_str))
                .unwrap()
                .unwrap();
        assert!(matches!(
            binding.idempotency_key.as_str(),
            TEST_KEY_2 | TEST_KEY_3
        ));
        assert_eq!(binding.state, ManualBalanceBindingState::Pending);
        assert!(binding.revision > initial.binding_revision);
    }

    #[test]
    fn binding_state_machine_requires_terminalization_before_rotation() {
        let (pending, pending_raw) =
            persist_manual_balance_binding_in_snapshot(None, TEST_KEY_1).unwrap();
        assert_eq!(pending.state, ManualBalanceBindingState::Pending);

        let (reused, reused_raw) =
            persist_manual_balance_binding_in_snapshot(Some(&pending_raw), TEST_KEY_2).unwrap();
        assert_eq!(reused, pending);
        assert_eq!(reused_raw, pending_raw);
        assert_eq!(
            rotate_manual_balance_binding_in_snapshot(
                Some(&pending_raw),
                TEST_KEY_1,
                pending.revision,
                TEST_KEY_2,
            )
            .unwrap_err(),
            ManualBalanceOperationStorageError::BindingChanged
        );

        let (terminal, terminal_raw) = finalize_manual_balance_binding_in_snapshot(
            Some(&pending_raw),
            TEST_KEY_1,
            pending.revision,
        )
        .unwrap();
        assert_eq!(terminal.state, ManualBalanceBindingState::Terminal);
        assert!(terminal.revision > pending.revision);
        let (rotated, _) = rotate_manual_balance_binding_in_snapshot(
            Some(&terminal_raw),
            TEST_KEY_1,
            terminal.revision,
            TEST_KEY_2,
        )
        .unwrap();
        assert_eq!(rotated.idempotency_key, TEST_KEY_2);
        assert_eq!(rotated.state, ManualBalanceBindingState::Pending);
        assert!(rotated.revision > terminal.revision);
    }

    #[test]
    fn terminal_in_one_tab_blocks_a_new_key_after_another_tab_is_ambiguous() {
        let scope = operation_scope("https://api.example.com/api/v1", "admin-1", "tenant-1");
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "freeze",
            &UpdateBalanceRequest::new(2.0, "incident hold"),
        );
        let storage = MemoryBalanceOperationStorage::default();
        let mut first_tab = ManualBalanceOperationTracker::new(scope.clone());
        let first = first_tab
            .begin_submission(identity.clone(), || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();
        let mut second_tab = ManualBalanceOperationTracker::new(scope.clone());
        let second = second_tab
            .begin_submission(identity.clone(), || TEST_KEY_2.to_string(), &storage)
            .unwrap()
            .unwrap();

        assert_eq!(first.idempotency_key, TEST_KEY_1);
        assert_eq!(second.idempotency_key, TEST_KEY_1);
        assert_eq!(second.binding_revision, first.binding_revision);

        let terminal = second_tab.finish_submission(&second, true, &storage);
        assert!(terminal.storage_error.is_none());
        let ambiguous = first_tab.finish_submission(&first, false, &storage);
        assert!(ambiguous.storage_error.is_none());
        let tombstone = storage.load_binding(&scope, &identity).unwrap().unwrap();
        assert_eq!(tombstone.idempotency_key, TEST_KEY_1);
        assert_eq!(tombstone.state, ManualBalanceBindingState::Terminal);

        // A reload loses the older tab's in-memory key, but the tombstone
        // still blocks an automatic replacement. The first click only arms
        // an explicit repeat; no financial submission becomes active.
        drop(first_tab);
        let mut reloaded = ManualBalanceOperationTracker::new(scope.clone());
        assert_eq!(
            reloaded
                .begin_submission(identity.clone(), || TEST_KEY_3.to_string(), &storage)
                .unwrap_err(),
            ManualBalanceOperationStorageError::TerminalConfirmationRequired
        );
        assert!(reloaded.has_terminal_confirmation());
        assert_eq!(
            storage.load_binding(&scope, &identity).unwrap(),
            Some(tombstone.clone())
        );

        // Only the second, explicit confirmation rotates to a new generation.
        let repeated = reloaded
            .begin_submission(identity.clone(), || TEST_KEY_3.to_string(), &storage)
            .unwrap()
            .unwrap();
        assert_eq!(repeated.idempotency_key, TEST_KEY_3);
        assert!(repeated.binding_revision > tombstone.revision);
        assert_eq!(
            storage
                .load_binding(&scope, &identity)
                .unwrap()
                .unwrap()
                .state,
            ManualBalanceBindingState::Pending
        );
    }

    #[test]
    fn late_old_completion_cannot_overwrite_an_explicitly_rotated_generation() {
        let scope = operation_scope("https://api.example.com/api/v1", "admin-1", "tenant-1");
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "freeze",
            &UpdateBalanceRequest::new(2.0, "incident hold"),
        );
        let storage = MemoryBalanceOperationStorage::default();
        let mut old_tab = ManualBalanceOperationTracker::new(scope.clone());
        let old = old_tab
            .begin_submission(identity.clone(), || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();
        let mut resolving_tab = ManualBalanceOperationTracker::new(scope.clone());
        let resolving = resolving_tab
            .begin_submission(identity.clone(), || TEST_KEY_2.to_string(), &storage)
            .unwrap()
            .unwrap();
        resolving_tab.finish_submission(&resolving, true, &storage);

        let mut repeat_tab = ManualBalanceOperationTracker::new(scope.clone());
        assert_eq!(
            repeat_tab
                .begin_submission(identity.clone(), || TEST_KEY_2.to_string(), &storage)
                .unwrap_err(),
            ManualBalanceOperationStorageError::TerminalConfirmationRequired
        );
        let repeated = repeat_tab
            .begin_submission(identity.clone(), || TEST_KEY_2.to_string(), &storage)
            .unwrap()
            .unwrap();
        assert_eq!(repeated.idempotency_key, TEST_KEY_2);

        // The previous generation may complete after the explicit repeat has
        // already been persisted. Its stale terminalization is benign and
        // cannot replace the new pending key or turn it into a tombstone.
        let late = old_tab.finish_submission(&old, true, &storage);
        assert!(late.storage_error.is_none());
        let current = storage.load_binding(&scope, &identity).unwrap().unwrap();
        assert_eq!(current.idempotency_key, TEST_KEY_2);
        assert_eq!(current.state, ManualBalanceBindingState::Pending);
        assert_eq!(current.revision, repeated.binding_revision);

        repeat_tab.finish_submission(&repeated, false, &storage);
        drop(repeat_tab);
        let mut reloaded = ManualBalanceOperationTracker::new(scope);
        let retry = reloaded
            .begin_submission(identity, || TEST_KEY_3.to_string(), &storage)
            .unwrap()
            .unwrap();
        assert_eq!(retry.idempotency_key, TEST_KEY_2);
    }

    #[test]
    fn admin_search_uses_a_short_debounce() {
        assert!((250..=500).contains(&SEARCH_DEBOUNCE_MS));
    }

    #[test]
    fn committing_a_debounced_search_resets_any_intervening_page_change() {
        let mut query = UserListQuery {
            search: "old term".to_string(),
            page: 4,
        };

        // 模拟用户在防抖计时期间又点击了分页。
        query.page = 2;
        query.commit_search("new term".to_string());

        assert_eq!(
            query,
            UserListQuery {
                search: "new term".to_string(),
                page: 1,
            }
        );
    }

    #[test]
    fn stale_reservation_release_is_detected_as_a_conflict() {
        assert!(is_conflict_error(&ClientError::from_status(
            409,
            r#"{"error":{"message":"reservation version changed"}}"#,
        )));
        assert!(!is_conflict_error(&ClientError::from_status(
            500,
            "database unavailable",
        )));
    }

    #[test]
    fn balance_operation_identity_uses_the_server_reason_normalization() {
        let padded = UpdateBalanceRequest::new(2.0, "  incident hold\n");
        let normalized = UpdateBalanceRequest::new(2.0, "incident hold");

        assert_eq!(
            normalize_manual_balance_reason(&padded.reason),
            "incident hold"
        );
        assert_eq!(
            ManualBalanceOperationIdentity::new("user-1", "freeze", &padded),
            ManualBalanceOperationIdentity::new("user-1", "freeze", &normalized),
        );
    }

    #[test]
    fn pending_balance_resource_does_not_expose_its_retained_value() {
        let current_request = (Some("user-1".to_string()), None);

        assert!(
            current_balance_details_result(
                &current_request,
                UseResourceState::Pending,
                Some(loaded_balance_details("user-1", "user-1")),
            )
            .is_none()
        );
    }

    #[test]
    fn balance_resource_rejects_a_previous_or_mismatched_user() {
        let current_request = (Some("user-2".to_string()), None);

        assert!(
            current_balance_details_result(
                &current_request,
                UseResourceState::Ready,
                Some(loaded_balance_details("user-1", "user-1")),
            )
            .is_none()
        );
        assert!(matches!(
            current_balance_details_result(
                &current_request,
                UseResourceState::Ready,
                Some(loaded_balance_details("user-2", "user-1")),
            ),
            Some(Err(ClientError::InvalidResponse(_)))
        ));

        let current = current_balance_details_result(
            &current_request,
            UseResourceState::Ready,
            Some(loaded_balance_details("user-2", "user-2")),
        )
        .expect("current resource should be visible")
        .expect("current resource should be successful");
        assert_eq!(current.user_id, "user-2");
    }

    #[test]
    fn balance_resource_rejects_the_retained_value_from_another_cursor() {
        let first_page = KeyedResourceValue::new(
            (Some("user-1".to_string()), None),
            Ok(Some(balance_details("user-1"))),
        );
        let second_page_request = (
            Some("user-1".to_string()),
            Some("opaque-page-2".to_string()),
        );

        assert!(
            current_balance_details_result(
                &second_page_request,
                UseResourceState::Ready,
                Some(first_page),
            )
            .is_none(),
            "a cursor change must hide the resource's retained previous page"
        );
    }

    #[test]
    fn balance_reservation_cursor_history_is_reversible_and_resettable() {
        let mut pagination = BalanceReservationPagination::default();
        assert_eq!(pagination.current_cursor(), None);
        assert_eq!(pagination.page_number(), 1);
        assert!(!pagination.can_go_back());

        assert!(pagination.advance(1, "page-2".to_string()));
        assert!(!pagination.advance(1, "duplicate-click".to_string()));
        assert!(pagination.advance(2, "page-3".to_string()));
        assert_eq!(pagination.current_cursor().as_deref(), Some("page-3"));
        assert_eq!(pagination.page_number(), 3);

        assert!(pagination.go_back(3));
        assert!(!pagination.go_back(3));
        assert_eq!(pagination.current_cursor().as_deref(), Some("page-2"));
        assert!(pagination.advance(2, "replacement-page-3".to_string()));
        assert_eq!(
            pagination.current_cursor().as_deref(),
            Some("replacement-page-3")
        );

        pagination.reset();
        assert_eq!(pagination.current_cursor(), None);
        assert_eq!(pagination.page_number(), 1);
        assert!(!pagination.can_go_back());
        assert!(!pagination.go_back(1));
        assert_eq!(pagination.page_number(), 1);
    }

    #[test]
    fn balance_retry_reuses_a_key_across_close_and_reopen_after_ambiguous_failure() {
        let scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let storage = MemoryBalanceOperationStorage::default();
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "freeze",
            &UpdateBalanceRequest::new(2.0, "incident hold"),
        );
        let mut tracker = ManualBalanceOperationTracker::new(scope);
        tracker.advance_modal();
        let first = tracker
            .begin_submission(identity.clone(), || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();

        // A transport/server failure is ambiguous, so the binding survives.
        assert!(
            tracker
                .finish_submission(&first, false, &storage)
                .owns_modal
        );
        tracker.advance_modal(); // close
        tracker.advance_modal(); // reopen
        let retry = tracker
            .begin_submission(identity, || "must-not-be-used".to_string(), &storage)
            .unwrap()
            .unwrap();

        assert_eq!(retry.idempotency_key, TEST_KEY_1);
    }

    #[test]
    fn ambiguous_balance_retry_reuses_its_key_after_page_reload_or_component_remount() {
        let scope = operation_scope("https://api.example.com/", "admin-1", "tenant-1");
        let storage = MemoryBalanceOperationStorage::default();
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "freeze",
            &UpdateBalanceRequest::new(2.0, "incident hold"),
        );
        let mut mounted = ManualBalanceOperationTracker::new(scope.clone());
        let first = mounted
            .begin_submission(identity.clone(), || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();
        assert!(
            mounted
                .finish_submission(&first, false, &storage)
                .owns_modal
        );
        assert!(storage.raw_for(&scope, &identity).is_some());

        // A fresh tracker has no in-memory bindings. The atomic persistence
        // operation may generate a candidate, but must adopt the stored key
        // while it holds the per-identity lock.
        drop(mounted);
        let mut remounted = ManualBalanceOperationTracker::new(scope);
        let retry = remounted
            .begin_submission(identity, || TEST_KEY_2.to_string(), &storage)
            .unwrap()
            .unwrap();

        assert_eq!(retry.idempotency_key, TEST_KEY_1);
    }

    #[test]
    fn persisted_balance_retry_keys_are_isolated_by_api_admin_and_tenant() {
        let original_scope = operation_scope("https://api-a.example.com", "admin-1", "tenant-1");
        let storage = MemoryBalanceOperationStorage::default();
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "recharge",
            &UpdateBalanceRequest::new(2.0, "manual recharge"),
        );
        let mut original = ManualBalanceOperationTracker::new(original_scope.clone());
        let first = original
            .begin_submission(identity.clone(), || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();
        original.finish_submission(&first, false, &storage);

        for other_scope in [
            operation_scope("https://api-b.example.com", "admin-1", "tenant-1"),
            operation_scope("https://api-a.example.com", "admin-2", "tenant-1"),
            operation_scope("https://api-a.example.com", "admin-1", "tenant-2"),
        ] {
            assert_ne!(
                original_scope.storage_key(&identity),
                other_scope.storage_key(&identity)
            );
            assert!(storage.load_key(&other_scope, &identity).unwrap().is_none());
        }
    }

    #[test]
    fn tracker_switches_identity_scope_only_while_idle() {
        let original_scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let next_scope = operation_scope("https://api.example.com", "admin-2", "tenant-2");
        let storage = MemoryBalanceOperationStorage::default();
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "recharge",
            &UpdateBalanceRequest::new(2.0, "manual recharge"),
        );
        let mut tracker = ManualBalanceOperationTracker::new(original_scope.clone());
        let active = tracker
            .begin_submission(identity, || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();

        assert!(!tracker.activate_scope(&next_scope));
        assert_eq!(tracker.scope, original_scope);
        assert!(tracker.is_active());

        tracker.finish_submission(&active, false, &storage);
        let previous_epoch = tracker.modal_epoch;
        assert!(tracker.activate_scope(&next_scope));
        assert_eq!(tracker.scope, next_scope);
        assert!(tracker.pending.is_empty());
        assert_eq!(tracker.modal_epoch, previous_epoch.wrapping_add(1));
    }

    #[test]
    fn terminal_tombstone_survives_unmount_and_requires_explicit_rotation() {
        let scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let storage = MemoryBalanceOperationStorage::default();
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "deduct",
            &UpdateBalanceRequest::new(2.0, "manual adjustment"),
        );
        let mut tracker = ManualBalanceOperationTracker::new(scope.clone());
        let first = tracker
            .begin_submission(identity.clone(), || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();
        let ambiguous = tracker.finish_submission(&first, false, &storage);
        assert!(ambiguous.owns_modal);
        assert!(ambiguous.storage_error.is_none());
        assert_eq!(
            storage.load_key(&scope, &identity).unwrap().as_deref(),
            Some(TEST_KEY_1)
        );

        let mut remounted = ManualBalanceOperationTracker::new(scope.clone());
        let terminal_retry = remounted
            .begin_submission(identity.clone(), || TEST_KEY_2.to_string(), &storage)
            .unwrap()
            .unwrap();
        assert_eq!(terminal_retry.idempotency_key, TEST_KEY_1);
        let terminal = remounted.finish_submission(&terminal_retry, true, &storage);
        assert!(terminal.owns_modal);
        assert!(terminal.storage_error.is_none());
        let tombstone = storage.load_binding(&scope, &identity).unwrap().unwrap();
        assert_eq!(tombstone.idempotency_key, TEST_KEY_1);
        assert_eq!(tombstone.state, ManualBalanceBindingState::Terminal);

        // Simulate an immediate component unmount after the terminal marker
        // was written but before any later UI acknowledgement.
        drop(remounted);
        let mut next_mount = ManualBalanceOperationTracker::new(scope);
        assert_eq!(
            next_mount
                .begin_submission(identity.clone(), || TEST_KEY_2.to_string(), &storage)
                .unwrap_err(),
            ManualBalanceOperationStorageError::TerminalConfirmationRequired
        );
        let next = next_mount
            .begin_submission(identity, || TEST_KEY_2.to_string(), &storage)
            .unwrap()
            .unwrap();
        assert_eq!(next.idempotency_key, TEST_KEY_2);
        assert!(next.binding_revision > tombstone.revision);
    }

    #[test]
    fn storage_read_failure_blocks_submission_after_key_generation_without_sending() {
        let scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "recharge",
            &UpdateBalanceRequest::new(2.0, "manual recharge"),
        );
        let mut tracker = ManualBalanceOperationTracker::new(scope);
        let key_factory_called = std::cell::Cell::new(false);

        let result = tracker.begin_submission(
            identity.clone(),
            || {
                key_factory_called.set(true);
                TEST_KEY_1.to_string()
            },
            &FailingBalanceOperationStorage,
        );

        assert_eq!(
            result.unwrap_err(),
            ManualBalanceOperationStorageError::ReadFailed
        );
        assert!(key_factory_called.get());
        assert!(!tracker.is_active());
        assert_eq!(
            tracker.pending.get(&identity).map(String::as_str),
            Some(TEST_KEY_1)
        );
    }

    #[test]
    fn storage_write_failure_blocks_submission_after_key_generation() {
        let scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "recharge",
            &UpdateBalanceRequest::new(2.0, "manual recharge"),
        );
        let mut tracker = ManualBalanceOperationTracker::new(scope);

        let result = tracker.begin_submission(
            identity.clone(),
            || TEST_KEY_1.to_string(),
            &WriteFailingBalanceOperationStorage,
        );

        assert_eq!(
            result.unwrap_err(),
            ManualBalanceOperationStorageError::WriteFailed
        );
        assert!(!tracker.is_active());
        assert_eq!(
            tracker.pending.get(&identity).map(String::as_str),
            Some(TEST_KEY_1)
        );
    }

    #[test]
    fn non_uuid_generated_key_is_rejected_before_submission() {
        let scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let storage = MemoryBalanceOperationStorage::default();
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "recharge",
            &UpdateBalanceRequest::new(2.0, "manual recharge"),
        );
        let mut tracker = ManualBalanceOperationTracker::new(scope.clone());

        let result =
            tracker.begin_submission(identity.clone(), || "not-a-uuid".to_string(), &storage);

        assert_eq!(
            result.unwrap_err(),
            ManualBalanceOperationStorageError::CorruptedData
        );
        assert!(!tracker.is_active());
        assert!(storage.raw_for(&scope, &identity).is_none());
    }

    #[test]
    fn terminal_storage_failure_keeps_the_binding_for_an_idempotent_retry() {
        let scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let storage = MemoryBalanceOperationStorage::default();
        let finalization_fails = FinalizationFailingBalanceOperationStorage(&storage);
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "freeze",
            &UpdateBalanceRequest::new(2.0, "incident hold"),
        );
        let mut tracker = ManualBalanceOperationTracker::new(scope.clone());
        let submission = tracker
            .begin_submission(identity.clone(), || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();

        let finish = tracker.finish_submission(&submission, true, &finalization_fails);

        assert!(finish.owns_modal);
        assert_eq!(
            finish.storage_error,
            Some(ManualBalanceOperationStorageError::WriteFailed)
        );
        assert_eq!(
            tracker.pending.get(&identity).map(String::as_str),
            Some(TEST_KEY_1)
        );
        assert_eq!(
            storage.load_key(&scope, &identity).unwrap().as_deref(),
            Some(TEST_KEY_1)
        );
    }

    #[test]
    fn failed_explicit_rotation_keeps_the_terminal_tombstone() {
        let scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let storage = MemoryBalanceOperationStorage::default();
        let rotation_fails = RotationFailingBalanceOperationStorage(&storage);
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "freeze",
            &UpdateBalanceRequest::new(2.0, "incident hold"),
        );
        let mut original = ManualBalanceOperationTracker::new(scope.clone());
        let submission = original
            .begin_submission(identity.clone(), || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();
        original.finish_submission(&submission, true, &storage);
        let tombstone = storage.load_binding(&scope, &identity).unwrap().unwrap();

        let mut repeat = ManualBalanceOperationTracker::new(scope.clone());
        assert_eq!(
            repeat
                .begin_submission(identity.clone(), || TEST_KEY_2.to_string(), &rotation_fails)
                .unwrap_err(),
            ManualBalanceOperationStorageError::TerminalConfirmationRequired
        );
        assert_eq!(
            repeat
                .begin_submission(identity.clone(), || TEST_KEY_2.to_string(), &rotation_fails)
                .unwrap_err(),
            ManualBalanceOperationStorageError::WriteFailed
        );
        assert!(!repeat.is_active());
        assert!(repeat.has_terminal_confirmation());
        assert_eq!(
            storage.load_binding(&scope, &identity).unwrap(),
            Some(tombstone)
        );
    }

    #[test]
    fn different_payloads_use_independent_storage_keys_across_tabs() {
        let scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let storage = MemoryBalanceOperationStorage::default();
        let first_identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "freeze",
            &UpdateBalanceRequest::new(2.0, "first"),
        );
        let second_identity = ManualBalanceOperationIdentity::new(
            "user-2",
            "freeze",
            &UpdateBalanceRequest::new(3.0, "second"),
        );
        let mut first_tab = ManualBalanceOperationTracker::new(scope.clone());
        let mut second_tab = ManualBalanceOperationTracker::new(scope.clone());

        let first = first_tab
            .begin_submission(first_identity.clone(), || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();
        first_tab.finish_submission(&first, false, &storage);
        let second = second_tab
            .begin_submission(second_identity.clone(), || TEST_KEY_2.to_string(), &storage)
            .unwrap()
            .unwrap();
        second_tab.finish_submission(&second, false, &storage);

        assert_ne!(
            scope.storage_key(&first_identity),
            scope.storage_key(&second_identity)
        );
        assert_eq!(
            storage
                .load_key(&scope, &first_identity)
                .unwrap()
                .as_deref(),
            Some(TEST_KEY_1)
        );
        assert_eq!(
            storage
                .load_key(&scope, &second_identity)
                .unwrap()
                .as_deref(),
            Some(TEST_KEY_2)
        );
    }

    #[test]
    fn changed_same_payload_binding_is_not_adopted_after_an_ambiguous_request() {
        let scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let storage = MemoryBalanceOperationStorage::default();
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "freeze",
            &UpdateBalanceRequest::new(2.0, "incident hold"),
        );
        let mut tracker = ManualBalanceOperationTracker::new(scope.clone());
        let first = tracker
            .begin_submission(identity.clone(), || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();
        tracker.finish_submission(&first, false, &storage);

        let (_, replacement) =
            persist_manual_balance_binding_in_snapshot(None, TEST_KEY_2).unwrap();
        storage
            .entries
            .borrow_mut()
            .insert(scope.storage_key(&identity).unwrap(), replacement);

        assert_eq!(
            tracker
                .begin_submission(identity, || TEST_KEY_3.to_string(), &storage)
                .unwrap_err(),
            ManualBalanceOperationStorageError::BindingChanged
        );
        assert!(!tracker.is_active());
    }

    #[test]
    fn ambiguous_balance_bindings_survive_other_payloads() {
        let scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let storage = MemoryBalanceOperationStorage::default();
        let first_identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "freeze",
            &UpdateBalanceRequest::new(2.0, "incident hold"),
        );
        let changed_identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "freeze",
            &UpdateBalanceRequest::new(3.0, "incident hold"),
        );
        let mut tracker = ManualBalanceOperationTracker::new(scope);
        tracker.advance_modal();

        let first = tracker
            .begin_submission(first_identity.clone(), || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();
        assert!(
            tracker
                .finish_submission(&first, false, &storage)
                .owns_modal
        );
        let changed = tracker
            .begin_submission(
                changed_identity.clone(),
                || TEST_KEY_2.to_string(),
                &storage,
            )
            .unwrap()
            .unwrap();
        assert_eq!(changed.idempotency_key, TEST_KEY_2);
        assert!(
            tracker
                .finish_submission(&changed, false, &storage)
                .owns_modal
        );

        // Neither ambiguous operation may lose its key when the user switches
        // between payloads: either request might already have committed.
        let old_payload_again = tracker
            .begin_submission(first_identity, || "must-not-be-used".to_string(), &storage)
            .unwrap()
            .unwrap();
        assert_eq!(old_payload_again.idempotency_key, TEST_KEY_1);
        assert!(
            tracker
                .finish_submission(&old_payload_again, false, &storage)
                .owns_modal
        );

        let changed_payload_again = tracker
            .begin_submission(
                changed_identity,
                || "must-not-be-used".to_string(),
                &storage,
            )
            .unwrap()
            .unwrap();
        assert_eq!(changed_payload_again.idempotency_key, TEST_KEY_2);
    }

    #[test]
    fn definitive_completion_terminalizes_only_the_matching_balance_binding() {
        let scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let storage = MemoryBalanceOperationStorage::default();
        let first_identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "recharge",
            &UpdateBalanceRequest::new(2.0, "first"),
        );
        let second_identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "recharge",
            &UpdateBalanceRequest::new(3.0, "second"),
        );
        let mut tracker = ManualBalanceOperationTracker::new(scope);

        let first = tracker
            .begin_submission(first_identity.clone(), || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();
        assert!(
            tracker
                .finish_submission(&first, false, &storage)
                .owns_modal
        );
        let second = tracker
            .begin_submission(second_identity.clone(), || TEST_KEY_2.to_string(), &storage)
            .unwrap()
            .unwrap();
        assert!(
            tracker
                .finish_submission(&second, false, &storage)
                .owns_modal
        );

        let first_retry = tracker
            .begin_submission(first_identity.clone(), || "unused".to_string(), &storage)
            .unwrap()
            .unwrap();
        assert_eq!(first_retry.idempotency_key, TEST_KEY_1);
        assert!(
            tracker
                .finish_submission(&first_retry, true, &storage)
                .owns_modal
        );

        let second_retry = tracker
            .begin_submission(
                second_identity.clone(),
                || "must-not-be-used".to_string(),
                &storage,
            )
            .unwrap()
            .unwrap();
        assert_eq!(second_retry.idempotency_key, TEST_KEY_2);
        assert!(
            tracker
                .finish_submission(&second_retry, false, &storage)
                .owns_modal
        );

        assert_eq!(
            storage
                .load_binding(&tracker.scope, &first_identity)
                .unwrap()
                .unwrap()
                .state,
            ManualBalanceBindingState::Terminal
        );
        assert_eq!(
            storage
                .load_binding(&tracker.scope, &second_identity)
                .unwrap()
                .unwrap()
                .state,
            ManualBalanceBindingState::Pending
        );

        assert_eq!(
            tracker
                .begin_submission(first_identity.clone(), || TEST_KEY_3.to_string(), &storage)
                .unwrap_err(),
            ManualBalanceOperationStorageError::TerminalConfirmationRequired
        );
        let next_first_operation = tracker
            .begin_submission(first_identity, || TEST_KEY_3.to_string(), &storage)
            .unwrap()
            .unwrap();
        assert_eq!(next_first_operation.idempotency_key, TEST_KEY_3);
    }

    #[test]
    fn tracker_rejects_a_second_active_submission_for_the_same_identity() {
        let scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let storage = MemoryBalanceOperationStorage::default();
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "unfreeze",
            &UpdateBalanceRequest::new(2.0, "release hold"),
        );
        let mut tracker = ManualBalanceOperationTracker::new(scope);
        let active = tracker
            .begin_submission(identity.clone(), || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();

        assert!(
            tracker
                .begin_submission(identity.clone(), || TEST_KEY_2.to_string(), &storage)
                .unwrap()
                .is_none()
        );
        assert!(
            tracker
                .finish_submission(&active, false, &storage)
                .owns_modal
        );
        let retry = tracker
            .begin_submission(identity, || "must-not-be-used".to_string(), &storage)
            .unwrap()
            .unwrap();
        assert_eq!(retry.idempotency_key, TEST_KEY_1);
    }

    #[test]
    fn stale_balance_completion_cannot_own_a_reopened_modal() {
        let scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let storage = MemoryBalanceOperationStorage::default();
        let identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "recharge",
            &UpdateBalanceRequest::new(2.0, "manual recharge"),
        );
        let mut tracker = ManualBalanceOperationTracker::new(scope);
        tracker.advance_modal();
        let old_submission = tracker
            .begin_submission(identity.clone(), || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();

        // Simulate an external modal replacement while the task is alive. The
        // UI normally blocks this, and the epoch check is the second barrier.
        tracker.advance_modal();
        assert!(
            !tracker
                .finish_submission(&old_submission, false, &storage)
                .owns_modal
        );
        let retry = tracker
            .begin_submission(identity, || "unused".to_string(), &storage)
            .unwrap()
            .unwrap();
        assert_eq!(retry.idempotency_key, TEST_KEY_1);
    }

    #[test]
    fn late_completion_cannot_clear_a_newer_payload_binding() {
        let scope = operation_scope("https://api.example.com", "admin-1", "tenant-1");
        let storage = MemoryBalanceOperationStorage::default();
        let old_identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "recharge",
            &UpdateBalanceRequest::new(2.0, "first"),
        );
        let new_identity = ManualBalanceOperationIdentity::new(
            "user-1",
            "recharge",
            &UpdateBalanceRequest::new(3.0, "second"),
        );
        let mut tracker = ManualBalanceOperationTracker::new(scope);
        let old = tracker
            .begin_submission(old_identity, || TEST_KEY_1.to_string(), &storage)
            .unwrap()
            .unwrap();
        assert!(tracker.finish_submission(&old, false, &storage).owns_modal);
        let new = tracker
            .begin_submission(new_identity.clone(), || TEST_KEY_2.to_string(), &storage)
            .unwrap()
            .unwrap();

        assert!(!tracker.finish_submission(&old, true, &storage).owns_modal);
        assert!(tracker.finish_submission(&new, false, &storage).owns_modal);
        let retry = tracker
            .begin_submission(new_identity, || "must-not-be-used".to_string(), &storage)
            .unwrap()
            .unwrap();
        assert_eq!(retry.idempotency_key, TEST_KEY_2);
    }
}

// ── 普通用户视图 ──────────────────────────────────────────────────────

#[component]
fn UserSelfView() -> Element {
    let i18n = use_i18n();
    let user_store = use_context::<UserStore>();
    let user_info = user_store.info.read();
    let nav = use_navigator();

    let display_name = user_info
        .as_ref()
        .map(|u| u.display_name().to_string())
        .unwrap_or_default();
    let email = user_info
        .as_ref()
        .map(|u| u.email.clone())
        .unwrap_or_default();
    let role = user_info
        .as_ref()
        .map(|u| u.role.clone())
        .unwrap_or_default();

    rsx! {
        div { class: "page-container",
        PageHeader {
            title: i18n.t("users.self_title").to_string(),
            description: i18n.t("users.self_desc").to_string(),
        }

        div { class: "card",
            div { class: "card-header",
                h3 { class: "card-title", {i18n.t("users.account_info")} }
                Button {
                    variant: ButtonVariant::Secondary,
                    size: ButtonSize::Small,
                    onclick: move |_| { nav.push(Route::UserProfile {}); },
                    {i18n.t("profile.edit")}
                }
            }
            div { class: "card-body",
                div { class: "info-grid",
                    div { class: "info-item",
                        span { class: "info-label", {i18n.t("users.display_name")} }
                        span { class: "info-value", "{display_name}" }
                    }
                    div { class: "info-item",
                        span { class: "info-label", {i18n.t("table.email")} }
                        span { class: "info-value", "{email}" }
                    }
                    div { class: "info-item",
                        span { class: "info-label", {i18n.t("table.role")} }
                        Badge { variant: BadgeVariant::Info, "{role}" }
                    }
                }
            }
        }
        }
    }
}
