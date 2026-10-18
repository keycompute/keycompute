//! Tenant metadata administration and personal issuance are separate routes and types.
mod admin;
mod editor;
mod owner;
#[cfg(test)]
mod tests;
mod types;
pub(crate) use admin::PlatformTenantKeys;
pub use admin::TenantKeys;
pub use owner::OwnerKeyIssuance;
