//! cloudflare-hermes - Hermes Agent-specific integration for the Cloudflare
//! Workers AI auth provider.
//!
//! Python-side plugin logic lives in `plugins/cloudflare/__init__.py` (a
//! cdylib hot-reload target, mirroring the Aphrodite plugin architecture).
//! This crate re-exports the core provider types so tool schemas and future
//! dylib entry points have one dependency root.

pub use cloudflare::{
	AccountCredentials, AuthProvider, CapabilityState, CloudflareError, ModelRecord, ModelRole,
	DEFAULT_MODEL, VERSION,
};

/// Env vars the plugin profiles - re-exported for the setup helper.
pub use cloudflare::auth::{ACCOUNT_ENV, API_BASE, TOKEN_ENV};

/// Plugin identity, as `hermes plugins list` should see it.
pub const PLUGIN_NAME: &str = "cloudflare";
pub const PLUGIN_KIND: &str = "model-provider";
