//! auth-cloudflare - Cloudflare Workers AI auth provider core.
//!
//! Account/token resolution, Workers AI endpoint construction, model catalog
//! normalization types, and account-scoped cache path helpers. The Hermes
//! integration crate (`auth-hermes-cloudflare`) and the Python plugin both
//! build on this crate so the endpoint/auth logic has exactly one source of
//! truth.
//!
//! Scope: direct Cloudflare-hosted `@cf/...` Workers AI text-generation
//! models over the OpenAI-compatible Chat Completions surface. AI Gateway
//! third-party models, non-chat modalities, and Cloudflare MCP operations are
//! explicitly out of scope for this package.

pub mod auth;
pub mod cache;
pub mod catalog;
pub mod error;

pub use auth::{AccountCredentials, AuthProvider};
pub use cache::cache_dir_for_account;
pub use catalog::{CapabilityState, ModelRecord, ModelRole};
pub use error::CloudflareError;

/// crate version, from Cargo.toml.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default inference model - the project's development default.
///
/// DeepSeek V4 Flash is the default because it has demonstrated reliable
/// delivery in the local Hermes tool-use workflow. GLM-5.3 Flash remains
/// available but is experimental pending conformance tests.
///
/// Single source of truth: adapters MUST read this (or the policy JSON
/// contract), never duplicate it.
pub const DEFAULT_MODEL: &str = "@cf/deepseek-ai/deepseek-v4-flash-0731";

/// Premium coding model (higher capability tier).
pub const PREMIUM_CODING_MODEL: &str = "@cf/moonshotai/kimi-k2.7-code";

/// Premium reasoning model (higher capability tier).
pub const PREMIUM_REASONING_MODEL: &str = "@cf/deepseek-ai/deepseek-v4-pro-0813";

/// Experimental model - available but not default (delivery reliability not
/// yet validated; requires passing conformance suite).
pub const EXPERIMENTAL_MODEL: &str = "@cf/zai-org/glm-5.3-flash";
