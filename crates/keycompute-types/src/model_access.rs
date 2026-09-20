//! Product-facing access modes. Protocol, credential, selection provenance and
//! execution resource remain separate concepts; these contracts explain the
//! configured model offering without disclosing upstream secrets.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAccessMode {
    #[default]
    AccountPool,
    Passthrough,
    NodeDispatch,
}
impl ModelAccessMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AccountPool => "account_pool",
            Self::Passthrough => "passthrough",
            Self::NodeDispatch => "node_dispatch",
        }
    }

    pub fn is_generation_path(path: &str) -> bool {
        Self::from_chat_path(path).is_some()
            || matches!(
                path,
                "/v1/messages"
                    | "/pt/v1/messages"
                    | "/nt/v1/messages"
                    | "/v1/responses"
                    | "/pt/v1/responses"
                    | "/nt/v1/responses"
                    | "/v1/responses/compact"
                    | "/v1/responses/input_tokens"
            )
    }
    pub fn uses_execution_rpm(path: &str) -> bool {
        Self::is_generation_path(path) && path != "/v1/responses/input_tokens"
    }

    pub const fn models_path(self) -> &'static str {
        match self {
            Self::AccountPool => "/v1/models",
            Self::Passthrough => "/pt/v1/models",
            Self::NodeDispatch => "/nt/v1/models",
        }
    }
    /// Classify registered generation routes, never a client parameter.
    pub fn from_chat_path(path: &str) -> Option<Self> {
        match path {
            "/v1/chat/completions" => Some(Self::AccountPool),
            "/pt/v1/chat/completions" => Some(Self::Passthrough),
            "/nt/v1/chat/completions" => Some(Self::NodeDispatch),
            _ => None,
        }
    }

    /// Public Chat Completions path for this access mode.
    pub const fn chat_path(self) -> &'static str {
        match self {
            Self::AccountPool => "/v1/chat/completions",
            Self::Passthrough => "/pt/v1/chat/completions",
            Self::NodeDispatch => "/nt/v1/chat/completions",
        }
    }

    /// Return the public family selected by a generation endpoint.  Resource
    /// paths intentionally return `None`; their query cannot switch protocol
    /// families.
    pub fn from_generation_path(path: &str) -> Option<Self> {
        Self::from_chat_path(path).or(match path {
            "/v1/messages" | "/v1/responses" => Some(Self::AccountPool),
            "/pt/v1/messages" | "/pt/v1/responses" => Some(Self::Passthrough),
            "/nt/v1/messages" | "/nt/v1/responses" => Some(Self::NodeDispatch),
            _ => None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability {
    /// Eligible at the observation instant; not a capacity reservation.
    Ready,
    Disabled,
    Unverified,
    Stale,
    Unhealthy,
    Unavailable,
}

/// Administrator-only model offering; no endpoint or upstream credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalogEntry {
    pub model: String,
    pub request_model: String,
    pub protocol: String,
    pub capability: String,
    pub request_path: String,
    pub status: ModelAvailability,
    pub reason_code: String,
    pub configured_targets: u64,
    pub eligible_targets: u64,
    pub binding_id: Option<String>,
    pub binding_revision: Option<i64>,
    pub account_id: Option<String>,
    pub account_name: Option<String>,
    pub pool_enabled: Option<bool>,
    pub health_expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalogPage {
    pub mode: ModelAccessMode,
    pub tenant_id: String,
    pub entries: Vec<ModelCatalogEntry>,
    pub page: u64,
    pub page_size: u64,
    pub total: u64,
    pub total_pages: u64,
    pub observed_at: String,
}

/// Bounded, tenant-authorized account choices for the passthrough setup flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingAccountOption {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub pool_enabled: bool,
    pub models: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingAccountOptions {
    pub accounts: Vec<BindingAccountOption>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn product_modes_are_distinct_from_protocol_and_execution_provenance() {
        for (value, name) in [
            (ModelAccessMode::AccountPool, "account_pool"),
            (ModelAccessMode::Passthrough, "passthrough"),
            (ModelAccessMode::NodeDispatch, "node_dispatch"),
        ] {
            assert_eq!(value.as_str(), name);
            assert_eq!(serde_json::to_value(value).unwrap(), name);
            assert_eq!(
                serde_json::from_value::<ModelAccessMode>(name.into()).unwrap(),
                value
            );
        }
        assert!(serde_json::from_str::<ModelAccessMode>("\"openai\"").is_err());
        assert!(serde_json::from_str::<ModelAccessMode>("\"node_token\"").is_err());
    }
}

#[cfg(test)]
mod ingress_contract_tests {
    use super::*;
    #[test]
    fn paths_are_unique_explicit_and_never_inferred_from_model_text() {
        for mode in [
            ModelAccessMode::AccountPool,
            ModelAccessMode::Passthrough,
            ModelAccessMode::NodeDispatch,
        ] {
            assert_eq!(
                ModelAccessMode::from_chat_path(mode.chat_path()),
                Some(mode)
            );
            assert!(ModelAccessMode::is_generation_path(mode.chat_path()));
            assert!(ModelAccessMode::uses_execution_rpm(mode.chat_path()));
            assert!(mode.models_path().ends_with("/models"));
        }
        for native in [
            "/nt/v1/responses",
            "/nt/v1/messages",
            "/pt/v1/responses",
            "/pt/v1/messages",
        ] {
            assert_eq!(ModelAccessMode::from_chat_path(native), None);
            assert!(ModelAccessMode::is_generation_path(native));
            assert!(ModelAccessMode::uses_execution_rpm(native));
        }
        for bad in [
            "/nt/v1/chat/completions/extra",
            "/nt/v1/models",
            "node:gemma3",
        ] {
            assert_eq!(ModelAccessMode::from_chat_path(bad), None);
            assert!(!ModelAccessMode::uses_execution_rpm(bad));
        }
        assert!(ModelAccessMode::is_generation_path(
            "/v1/responses/input_tokens"
        ));
        assert!(!ModelAccessMode::uses_execution_rpm(
            "/v1/responses/input_tokens"
        ));
    }
}
