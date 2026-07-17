//! Golden round-trip harness for `fixtures/wire/session-events/`.
//!
//! These fixtures come from tau's `tau_coding.events` (`SessionOwnEvent`). M1's
//! `rho-agent` golden harness intentionally skipped them (they are coding-layer
//! types); M4b lands them here. For each fixture: parse tau's exact bytes into
//! the typed [`SessionOwnEvent`], re-serialize, and assert byte-identity.
//!
//! Repo policy (AGENTS.md): if a golden diffs, the *code* is wrong, never the
//! fixture.

use std::fs;
use std::path::{Path, PathBuf};

use rho_coding::events::SessionOwnEvent;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .canonicalize()
        .expect("fixtures dir exists")
}

#[test]
fn session_events_roundtrip() {
    let dir = fixtures_dir().join("wire").join("session-events");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no session-events fixtures found");

    let mut fails = Vec::new();
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = fs::read_to_string(&path).unwrap();
        let expected = bytes.trim_end_matches('\n');
        match serde_json::from_str::<SessionOwnEvent>(expected) {
            Ok(parsed) => {
                let actual = serde_json::to_string(&parsed).unwrap();
                if actual != expected {
                    fails.push(format!(
                        "session-events/{name}: BYTE MISMATCH\n  expected: {expected}\n  actual:   {actual}"
                    ));
                }
            }
            Err(e) => fails.push(format!("session-events/{name}: parse error: {e}")),
        }
    }
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
}
