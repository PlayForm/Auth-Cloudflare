//! Capabilities - marker-based capability inference from a model id.
//!
//! The three-state contract forbids encoding incomplete
//! metadata as `Unsupported`: only a positive safety marker yields
//! `Unsupported`, and only a positive tool-family marker yields `Confirmed`.
//! Every other verdict stays `Unknown` - absence of evidence is never
//! evidence of absence.

use crate::catalog::{CapabilityState, ModelCapabilities};

/// Tool-capable model families, per Cloudflare docs/schema.
/// Family-level markers: each matches every released variant of the family
/// (e.g. `glm-5` covers glm-5.2, glm-5.3, glm-5.3-flash).
const TOOL_CAPABLE_FAMILIES: &[&str] = &[
	"deepseek-v4",
	"kimi",
	"gpt-oss",
	"qwen3",
	"nemotron",
	"mistral-small",
	"llama-3.3",
	"llama-4-scout",
	"granite",
	"glm-5",
];

/// Non-chat variants whose id also matches a tool-capable family marker
/// (image/video models must never be inferred tool-capable).
const NON_CHAT_OVERRIDES: &[&str] = &["kimi-k3", "qwen3-vl", "qwen-image"];

/// Safety/classification models - never chat-capable, never tool-capable.
const SAFETY_MARKERS: &[&str] = &["llama-guard"];

/// Infer the capability matrix from a model id (marker-based).
///
/// - Safety models: `chat` and `tools` are `Unsupported` (positive marker).
/// - Tool-capable families: `tools` is `Confirmed`.
/// - Anything else: `Unknown` - never `Unsupported` from absence alone.
pub fn infer_from_id(id: &str) -> ModelCapabilities {
	let lower = id.to_lowercase();
	let mut capabilities = ModelCapabilities::default();

	if SAFETY_MARKERS.iter().any(|marker| lower.contains(marker)) {
		capabilities.chat = CapabilityState::Unsupported;
		capabilities.tools = CapabilityState::Unsupported;
		return capabilities;
	}

	if NON_CHAT_OVERRIDES.iter().any(|marker| lower.contains(marker)) {
		return capabilities;
	}

	if TOOL_CAPABLE_FAMILIES.iter().any(|marker| lower.contains(marker)) {
		capabilities.tools = CapabilityState::Confirmed;
	}

	capabilities
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn deepseek_is_tool_capable() {
		let caps = infer_from_id("@cf/deepseek-ai/deepseek-v4-flash-0731");
		assert_eq!(caps.tools, CapabilityState::Confirmed);
		assert_eq!(caps.chat, CapabilityState::Confirmed);
	}

	#[test]
	fn premium_models_are_tool_capable() {
		assert_eq!(
			infer_from_id("@cf/deepseek-ai/deepseek-v4-pro-0813").tools,
			CapabilityState::Confirmed
		);
		assert_eq!(infer_from_id("@cf/moonshotai/kimi-k2.7-code").tools, CapabilityState::Confirmed);
		assert_eq!(infer_from_id("@cf/zai-org/glm-5.3-flash").tools, CapabilityState::Confirmed);
		assert_eq!(infer_from_id("@cf/openai/gpt-oss-120b").tools, CapabilityState::Confirmed);
	}

	#[test]
	fn safety_model_is_unsupported_not_unknown() {
		let caps = infer_from_id("@cf/meta/llama-guard-3-8b");
		assert_eq!(caps.chat, CapabilityState::Unsupported);
		assert_eq!(caps.tools, CapabilityState::Unsupported);
	}

	#[test]
	fn unknown_model_stays_unknown_not_unsupported() {
		let caps = infer_from_id("@cf/acme/mystery-model");
		assert_eq!(caps.chat, CapabilityState::Confirmed);
		assert_eq!(caps.tools, CapabilityState::Unknown);
		assert_ne!(caps.tools, CapabilityState::Unsupported);
	}

	#[test]
	fn unlisted_chat_family_stays_unknown() {
		// llama-3.2 has no tool marker - absence must yield Unknown, not Unsupported.
		let caps = infer_from_id("@cf/meta/llama-3.2-1b-instruct");
		assert_eq!(caps.tools, CapabilityState::Unknown);
		assert_ne!(caps.tools, CapabilityState::Unsupported);
	}

	#[test]
	fn non_chat_override_never_tool_capable() {
		// kimi-k3 matches the "kimi" family marker but is an image model.
		let caps = infer_from_id("@cf/moonshotai/kimi-k3");
		assert_eq!(caps.tools, CapabilityState::Unknown);
		assert_ne!(caps.tools, CapabilityState::Confirmed);
	}
}
