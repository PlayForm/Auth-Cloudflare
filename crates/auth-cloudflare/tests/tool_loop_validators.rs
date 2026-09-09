//! Integration tests for the PUBLIC tool-loop validators - the pure,
//! deterministic acceptance machinery behind the multi-turn conformance
//! harness (feedback 02 suite 40). Zero network: no model is called; only
//! the ordering, duplicate, argument-schema, and schema-generation checks
//! are exercised with synthetic observations.

use auth_cloudflare::tool_loop::{all_arguments_valid, find_duplicates, tool_schemas, validate_ordering};
use auth_cloudflare::ToolCallObservation;

fn obs(name: &str, arguments: serde_json::Value, turn: u32) -> ToolCallObservation {
	ToolCallObservation { name: name.to_string(), arguments, turn }
}

#[test]
fn ordering_accepts_read_run_write() {
	let calls = vec![
		obs("read_fixture", serde_json::json!({"fixture_id": "calc"}), 1),
		obs("run_fixture_test", serde_json::json!({"fixture_id": "calc"}), 2),
		obs(
			"write_fixture_patch",
			serde_json::json!({"fixture_id": "calc", "patch": "fix the off-by-one"}),
			3,
		),
	];
	assert!(validate_ordering(&calls));
}

#[test]
fn ordering_rejects_write_first_and_read_after_run() {
	let write_first = vec![
		obs(
			"write_fixture_patch",
			serde_json::json!({"fixture_id": "calc", "patch": "fix"}),
			1,
		),
		obs("read_fixture", serde_json::json!({"fixture_id": "calc"}), 2),
		obs("run_fixture_test", serde_json::json!({"fixture_id": "calc"}), 3),
	];
	assert!(!validate_ordering(&write_first), "writing before reading is a violation");

	let read_after_run = vec![
		obs("read_fixture", serde_json::json!({"fixture_id": "calc"}), 1),
		obs("run_fixture_test", serde_json::json!({"fixture_id": "calc"}), 2),
		obs("read_fixture", serde_json::json!({"fixture_id": "calc"}), 3),
	];
	assert!(!validate_ordering(&read_after_run), "re-reading after a run is a violation");
}

#[test]
fn duplicates_are_detected_on_repeated_identical_calls() {
	let repeated_read = vec![
		obs("read_fixture", serde_json::json!({"fixture_id": "calc"}), 1),
		obs("read_fixture", serde_json::json!({"fixture_id": "calc"}), 2),
	];
	let duplicates = find_duplicates(&repeated_read);
	assert_eq!(duplicates.len(), 1);
	assert_eq!(duplicates[0].name, "read_fixture");
	assert_eq!(duplicates[0].turn, 2);

	// Same tool, different arguments: NOT a duplicate.
	let different_args = vec![
		obs("read_fixture", serde_json::json!({"fixture_id": "calc"}), 1),
		obs("read_fixture", serde_json::json!({"fixture_id": "greeter"}), 1),
	];
	assert!(find_duplicates(&different_args).is_empty());
}

#[test]
fn arguments_accept_conformant_calls() {
	let calls = vec![
		obs("read_fixture", serde_json::json!({"fixture_id": "calc"}), 1),
		obs("run_fixture_test", serde_json::json!({"fixture_id": "greeter"}), 2),
		obs(
			"write_fixture_patch",
			serde_json::json!({"fixture_id": "calc", "patch": "fix add"}),
			3,
		),
	];
	assert!(all_arguments_valid(&calls));
}

#[test]
fn arguments_reject_missing_required_fields() {
	assert!(!all_arguments_valid(&[obs("read_fixture", serde_json::json!({}), 1)]));
	assert!(!all_arguments_valid(&[obs(
		"write_fixture_patch",
		serde_json::json!({"fixture_id": "calc"}),
		1
	)]));
}

#[test]
fn arguments_reject_wrong_enum_value() {
	assert!(!all_arguments_valid(&[obs(
		"read_fixture",
		serde_json::json!({"fixture_id": "nope"}),
		1
	)]));
}

#[test]
fn arguments_reject_extra_properties() {
	// additionalProperties: false must be respected.
	assert!(!all_arguments_valid(&[obs(
		"read_fixture",
		serde_json::json!({"fixture_id": "calc", "extra": 1}),
		1
	)]));
}

#[test]
fn tool_schemas_are_three_and_reject_extra_properties() {
	let schemas = tool_schemas();
	assert_eq!(schemas.len(), 3);
	let names: Vec<&str> = schemas
		.iter()
		.map(|schema| schema["function"]["name"].as_str().expect("schema names a tool"))
		.collect();
	assert_eq!(names, vec!["read_fixture", "run_fixture_test", "write_fixture_patch"]);
	for schema in &schemas {
		assert_eq!(schema["type"], "function");
		assert_eq!(
			schema["function"]["parameters"]["additionalProperties"].as_bool(),
			Some(false),
			"every tool schema must forbid extra properties"
		);
		let required = schema["function"]["parameters"]["required"]
			.as_array()
			.expect("required present");
		assert!(!required.is_empty());
	}
}
