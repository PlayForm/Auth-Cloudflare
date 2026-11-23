//! Fallback catalog - the offline model list when live discovery fails.
//!
//! ORDER IS POLICY: DeepSeek V4.1 Flash is first and is
//! the development default; GLM-5.3 is experimental and must never lead the
//! fallback list. The single source of truth lives in [`crate::catalog`];
//! these functions expose it without duplicating ids.

/// The ordered offline fallback list (DeepSeek Flash first, GLM not first).
///
/// Delisted-but-shipped ids stay in the list: they are `Deprecated`, so they
/// resolve by id without ever being offered.
pub fn fallback_models() -> Vec<&'static str> {
	crate::catalog::FALLBACK_MODELS.to_vec()
}

/// Models marked experimental by project policy (GLM-5.3).
pub fn experimental_models() -> Vec<&'static str> {
	crate::catalog::EXPERIMENTAL_MODELS.to_vec()
}

/// Models Cloudflare no longer lists, retained so existing configurations keep
/// resolving. Never the default, never primary-agent eligible.
pub fn deprecated_models() -> Vec<&'static str> {
	crate::catalog::DEPRECATED_MODELS.to_vec()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn fallback_first_is_default() {
		assert_eq!(fallback_models()[0], crate::DEFAULT_MODEL);
	}

	#[test]
	fn glm_is_not_first_in_fallback() {
		let models = fallback_models();
		// The fallback list carries the glm-5.3 family; no GLM variant may lead it.
		assert_ne!(models[0], crate::EXPERIMENTAL_MODEL);
		assert!(!models[0].contains("glm"));
		assert!(models.iter().any(|id| id.contains("glm")));
	}

	#[test]
	fn experimental_models_is_glm_53() {
		// glm-5.3-flash moved to deprecated when Cloudflare delisted it, so the
		// experimental set is the single listed GLM model.
		assert_eq!(experimental_models(), vec!["@cf/zai-org/glm-5.3"]);
	}

	#[test]
	fn deprecated_models_keep_the_delisted_glm_flash() {
		let deprecated = deprecated_models();
		assert!(deprecated.contains(&"@cf/zai-org/glm-5.3-flash"));
		// A model is never both experimental and deprecated.
		assert!(deprecated.iter().all(|id| !experimental_models().contains(id)));
		// ...and every deprecated id stays reachable through the fallback list.
		assert!(deprecated.iter().all(|id| fallback_models().contains(id)));
	}

	#[test]
	fn fallback_omits_safety_model() {
		assert!(!fallback_models().contains(&"@cf/meta/llama-guard-3-8b"));
	}
}
