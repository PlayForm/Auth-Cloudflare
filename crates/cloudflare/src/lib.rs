//! cloudflare - Cloudflare Workers AI auth provider core.
//!
//! Account/token resolution, Workers AI endpoint construction, model catalog
//! normalization types, and account-scoped cache path helpers. The Hermes
//! integration crate (`cloudflare-hermes`) and the Python plugin both build on
//! this crate so the endpoint/auth logic has exactly one source of truth.

pub mod auth;
pub mod catalog;
pub mod cache;
pub mod error;

pub use auth::{AccountCredentials, AuthProvider};
pub use cache::cache_dir_for_account;
pub use catalog::{CapabilityState, ModelRecord, ModelRole};
pub use error::CloudflareError;

/// crate version, from Cargo.toml.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default inference model - the cheapest strong coding/tool default on
/// Workers AI (GLM-5.3 Flash: 1.31M context, function calling, $0.15/M in).
pub const DEFAULT_MODEL: &str = "@cf/zai-org/glm-5.3-flash";
