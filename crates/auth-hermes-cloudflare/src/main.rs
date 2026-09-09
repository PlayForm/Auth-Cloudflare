//! auth-hermes-cloudflare CLI - optional utility around the plugin/binary.
//!
//! The primary runtime artifact is the `auth-cloudflare` executable (doctor,
//! catalog, model verify). This optional binary manages it from the Rust
//! side (feedback 06 "optional utility"):
//!
//! ```text
//! auth-hermes-cloudflare status                  # JSON: plugin + binary state
//! auth-hermes-cloudflare install                 # run download.sh (plugin dir)
//! auth-hermes-cloudflare upgrade                 # alias of install
//! auth-hermes-cloudflare unlink                  # remove ~/.hermes/bin/auth-cloudflare
//! ```
//!
//! Exit codes: 0 ok, 1 operational error. The binary never touches the API
//! token and never prints secrets.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

/// Plugin identity (matches lib.rs / plugin manifest).
const PLUGIN_NAME: &str = "auth-hermes-cloudflare";
const BINARY_NAME: &str = "auth-cloudflare";

/// Managed binary install path (the same path download.sh and the plugin
/// locator use).
fn managed_binary_path() -> PathBuf {
	PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "~".to_string()))
		.join(".hermes")
		.join("bin")
		.join(BINARY_NAME)
}

/// Plugin submodule directory, discovered from common layouts relative to
/// this binary's CARGO_MANIFEST_DIR (repo checkout) or the managed plugins
/// dir (installed profile).
fn plugin_dirs() -> Vec<PathBuf> {
	let mut dirs = Vec::new();
	if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
		let manifest = PathBuf::from(manifest);
		// <repo>/crates/auth-hermes-cloudflare -> <repo>/plugins/auth-hermes-cloudflare
		for candidate in [
			manifest
				.parent()
				.and_then(Path::parent)
				.map(|repo| repo.join("plugins").join(PLUGIN_NAME)),
			manifest.parent().map(|parent| parent.join(PLUGIN_NAME)),
		] {
			if let Some(candidate) = candidate {
				dirs.push(candidate);
			}
		}
	}
	dirs.push(
		PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "~".to_string()))
			.join(".hermes")
			.join("plugins")
			.join(PLUGIN_NAME),
	);
	dirs
}

/// Locate the auth-cloudflare executable: AUTH_CLOUDFLARE_BIN env → PATH →
/// ~/.hermes/bin → plugin bin/ → plugin binaries/ (mirrors the Python
/// plugin's locator order).
fn locate_binary() -> Option<PathBuf> {
	if let Ok(explicit) = std::env::var("AUTH_CLOUDFLARE_BIN") {
		let candidate = PathBuf::from(explicit);
		if candidate.is_file() {
			return Some(candidate);
		}
	}
	if let Some(found) = which(BINARY_NAME) {
		return Some(found);
	}
	for dir in plugin_dirs() {
		for candidate in [dir.join("bin").join(BINARY_NAME), dir.join("binaries").join(BINARY_NAME)] {
			if candidate.is_file() {
				return Some(candidate);
			}
		}
	}
	let managed = managed_binary_path();
	if managed.is_file() {
		return Some(managed);
	}
	None
}

/// Minimal PATH lookup (no `which` crate dependency).
fn which(name: &str) -> Option<PathBuf> {
	let path = std::env::var("PATH").unwrap_or_default();
	for dir in path.split(':') {
		if dir.is_empty() {
			continue;
		}
		let candidate = PathBuf::from(dir).join(name);
		if candidate.is_file() {
			return Some(candidate);
		}
	}
	None
}

/// Run `auth-cloudflare version --format json` and parse it.
fn binary_version(bin: &Path) -> Result<serde_json::Value, String> {
	let output = Command::new(bin)
		.args(["version", "--format", "json"])
		.output()
		.map_err(|error| format!("cannot execute {bin:?}: {error}"))?;
	if !output.status.success() {
		return Err(format!("{BINARY_NAME} version exited with {}", output.status));
	}
	serde_json::from_slice(&output.stdout)
		.map_err(|error| format!("{BINARY_NAME} version returned invalid JSON: {error}"))
}

fn cmd_status(out: &mut dyn std::io::Write) -> i32 {
	let bin = locate_binary();
	let version = bin.as_deref().and_then(|path| binary_version(path).ok());
	let managed = managed_binary_path();
	let report = json!({
		"plugin": PLUGIN_NAME,
		"binary": {
			"located": bin.as_ref().map(|path| path.display().to_string()),
			"managed_path": managed.display().to_string(),
			"managed_present": managed.is_file(),
			"version": version,
		},
	});
	match serde_json::to_string_pretty(&report) {
		Ok(json) => {
			let _ = writeln!(out, "{json}");
			0
		},
		Err(error) => {
			let _ = writeln!(out, "auth-hermes-cloudflare: failed to serialize status: {error}");
			1
		},
	}
}

fn cmd_install(out: &mut dyn std::io::Write) -> i32 {
	let Some(script) = plugin_dirs()
		.iter()
		.map(|dir| dir.join("download.sh"))
		.find(|script| script.is_file())
	else {
		let _ = writeln!(
			out,
			"auth-hermes-cloudflare: download.sh not found - run it from a checkout of {PLUGIN_NAME} or install the plugin first"
		);
		return 1;
	};
	match Command::new(&script).status() {
		Ok(status) if status.success() => {
			let _ = writeln!(out, "auth-hermes-cloudflare: installed via {script:?}");
			0
		},
		Ok(status) => {
			let _ = writeln!(out, "auth-hermes-cloudflare: {script:?} exited with {status}");
			1
		},
		Err(error) => {
			let _ = writeln!(out, "auth-hermes-cloudflare: cannot run {script:?}: {error}");
			1
		},
	}
}

fn cmd_unlink(out: &mut dyn std::io::Write) -> i32 {
	let managed = managed_binary_path();
	if !managed.is_file() {
		let _ = writeln!(out, "auth-hermes-cloudflare: nothing to unlink at {managed:?}");
		return 0;
	}
	match std::fs::remove_file(&managed) {
		Ok(()) => {
			let _ = writeln!(out, "auth-hermes-cloudflare: removed {managed:?}");
			0
		},
		Err(error) => {
			let _ = writeln!(out, "auth-hermes-cloudflare: cannot remove {managed:?}: {error}");
			1
		},
	}
}

fn main() {
	let args: Vec<String> = std::env::args().skip(1).collect();
	let code = match args.as_slice() {
		[] => cmd_status(&mut std::io::stdout()),
		[cmd] if cmd == "status" => cmd_status(&mut std::io::stdout()),
		[cmd] if cmd == "install" || cmd == "upgrade" => cmd_install(&mut std::io::stdout()),
		[cmd] if cmd == "unlink" => cmd_unlink(&mut std::io::stdout()),
		[cmd] if cmd == "--help" || cmd == "-h" || cmd == "help" => {
			println!(
				"auth-hermes-cloudflare - plugin/binary utility\n\nUsage:\n  auth-hermes-cloudflare status    JSON plugin + binary state\n  auth-hermes-cloudflare install   run download.sh (plugin dir)\n  auth-hermes-cloudflare upgrade   alias of install\n  auth-hermes-cloudflare unlink    remove ~/.hermes/bin/auth-cloudflare"
			);
			0
		},
		[other, ..] => {
			eprintln!("auth-hermes-cloudflare: unknown command {other:?} (try status)");
			1
		},
	};
	std::process::exit(code);
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn managed_binary_path_is_hermes_bin() {
		let path = managed_binary_path();
		assert!(path.ends_with(".hermes/bin/auth-cloudflare"));
	}

	#[test]
	fn locate_binary_falls_back_to_managed_path() {
		// With AUTH_CLOUDFLARE_BIN unset and no PATH hit, the locator must
		// still check ~/.hermes/bin (and not panic when it is absent).
		let found = locate_binary();
		if let Some(path) = &found {
			assert!(path.is_file(), "located binary must exist: {path:?}");
		}
	}
}
