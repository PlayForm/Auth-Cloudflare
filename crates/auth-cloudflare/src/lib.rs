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
pub mod capabilities;
pub mod catalog;
pub mod config;
pub mod error;
pub mod fallback_catalog;
pub mod fetch;
pub mod health;
pub mod observability;
pub mod policy;
pub mod schema;
pub mod tool_loop;
pub mod verify;

pub use auth::{AccountCredentials, AuthProvider};
pub use cache::{cache_dir_for_account, cache_is_stale, read_catalog_cache, write_catalog_cache, CatalogCacheMeta};
pub use capabilities::infer_from_id;
pub use catalog::{CapabilityState, ModelRecord, ModelRole};
pub use error::CloudflareError;
pub use fallback_catalog::{experimental_models, fallback_models};
pub use health::{
	CONFORMANCE_SUITE_VERSION, FailureClass, FailureEvidence, ModelHealthRecord, ModelVerification,
	VerificationConfidence, VerificationStatus,
};
pub use observability::{
	debug_protocol_enabled, redact, Event, EventLog, DEBUG_PROTOCOL_ENV, EVENT_LOG_ENV, OBSERVABILITY_ENV,
};
pub use policy::{
	ranking_score, ModelPolicy, ModelStatus, PolicyEntry, RankingBreakdown, REFERENCE_CONTEXT_TOKENS,
	REFERENCE_LATENCY_MS, REFERENCE_PRICE_PER_MILLION, WEIGHT_CONTEXT, WEIGHT_DELIVERY, WEIGHT_LATENCY, WEIGHT_PRICE,
	WEIGHT_TOOL_LOOP,
};
pub use schema::{VersionInfo, CATALOG_SCHEMA_VERSION, PROTOCOL_VERSION};
pub use tool_loop::{
	all_arguments_valid, execute_tool, find_duplicates, fixture_source, run_tool_loop, tool_schemas, validate_ordering,
	ToolCallObservation, ToolLoopOutcome, TOOL_LOOP_MAX_TURNS, TOOL_LOOP_SYSTEM_PROMPT, TOOL_LOOP_USER_PROMPT,
};
pub use verify::{HealthStore, SmokeRunReport, SuiteKind, MAX_COST_ENV};

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
