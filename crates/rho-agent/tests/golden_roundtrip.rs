//! Golden round-trip harness — the correctness oracle for M1.
//!
//! For every non-synthetic fixture: parse the tau-emitted bytes into the typed
//! rho model, re-serialize, and assert the output is **byte-identical**. For the
//! legacy fixtures, the parse goes through the Tau-v1 migration and is compared
//! against the sibling `.expected` golden.
//!
//! Repo policy (AGENTS.md): if a golden diffs, the *code* is wrong, never the
//! fixture. So a mismatch here is a hard failure with a byte-level diff.
//!
//! ## Scope note (documented skip)
//!
//! `fixtures/wire/session-events/` is intentionally **not** covered here. Those
//! fixtures come from `tau_coding.events` (`SessionOwnEvent`), which maps to the
//! `rho-coding` crate (milestone M4), not `rho-agent`. The layering contract
//! (AGENTS.md) forbids `rho-agent` from owning coding-layer types, so they are
//! covered when that crate lands.

use std::fs;
use std::path::{Path, PathBuf};

use rho_agent::events::AgentEvent;
use rho_agent::messages::{
    AgentMessage, ImageContent, TextContent, ThinkingContent, ToolCall, Usage, UsageCost,
};
use rho_agent::provider_events::AssistantMessageEvent;
use rho_agent::session::entries::SessionEntry;
use rho_agent::session::jsonl::{entry_from_json_line, entry_to_json_line};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Any content block — a test-only union covering all four block kinds so the
/// heterogeneous `wire/content/` directory can be round-tripped uniformly.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum AnyContent {
    Text(TextContent),
    Thinking(ThinkingContent),
    Image(ImageContent),
    ToolCall(ToolCall),
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .canonicalize()
        .expect("fixtures dir exists")
}

/// Sorted list of files in `dir` matching `ext`.
fn files_in(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == ext))
        .collect();
    out.sort();
    out
}

/// Parse `bytes` into `T`, re-serialize, and return `Err(diff)` on mismatch.
fn roundtrip<T: Serialize + DeserializeOwned>(label: &str, bytes: &str) -> Result<(), String> {
    let expected = bytes.trim_end_matches('\n');
    let parsed: T = serde_json::from_str(expected)
        .map_err(|e| format!("{label}: parse error: {e}\n  input: {expected}"))?;
    let actual =
        serde_json::to_string(&parsed).map_err(|e| format!("{label}: serialize error: {e}"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{label}: BYTE MISMATCH\n  expected: {expected}\n  actual:   {actual}"
        ))
    }
}

/// Collect round-trip failures for every `*.json` file in a `wire/` subdir.
fn check_wire_dir<T: Serialize + DeserializeOwned>(sub: &str, fails: &mut Vec<String>) {
    let dir = fixtures_dir().join("wire").join(sub);
    for path in files_in(&dir, "json") {
        let bytes = fs::read_to_string(&path).unwrap();
        let label = format!("wire/{sub}/{}", path.file_name().unwrap().to_string_lossy());
        if let Err(e) = roundtrip::<T>(&label, &bytes) {
            fails.push(e);
        }
    }
}

#[test]
fn wire_messages_roundtrip() {
    let mut fails = Vec::new();
    check_wire_dir::<AgentMessage>("messages", &mut fails);
    check_wire_dir::<AnyContent>("content", &mut fails);
    check_wire_dir::<rho_agent::tools::AgentToolResult>("tool_result", &mut fails);
    check_wire_dir::<SessionEntry>("entries", &mut fails);
    check_wire_dir::<AgentEvent>("agent-events", &mut fails);
    check_wire_dir::<AssistantMessageEvent>("assistant-events", &mut fails);
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
}

#[test]
fn wire_usage_roundtrip() {
    // The usage/ dir mixes two types; dispatch by filename.
    let dir = fixtures_dir().join("wire").join("usage");
    let mut fails = Vec::new();
    for path in files_in(&dir, "json") {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = fs::read_to_string(&path).unwrap();
        let label = format!("wire/usage/{name}");
        let res = if name.starts_with("usage_cost") {
            roundtrip::<UsageCost>(&label, &bytes)
        } else {
            roundtrip::<Usage>(&label, &bytes)
        };
        if let Err(e) = res {
            fails.push(e);
        }
    }
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
}

/// Each line of a JSONL file round-trips through `entry_from/to_json_line`.
#[test]
fn sessions_roundtrip() {
    let dir = fixtures_dir().join("sessions");
    let mut fails = Vec::new();
    for name in ["linear", "branched", "compaction", "kitchen-sink"] {
        let path = dir.join(format!("{name}.jsonl"));
        let text = fs::read_to_string(&path).unwrap();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let entry = match entry_from_json_line(line, Some(i + 1)) {
                Ok(e) => e,
                Err(e) => {
                    fails.push(format!("sessions/{name}.jsonl:{}: {e}", i + 1));
                    continue;
                }
            };
            let actual = entry_to_json_line(&entry);
            let actual = actual.trim_end_matches('\n');
            if actual != line {
                fails.push(format!(
                    "sessions/{name}.jsonl:{} BYTE MISMATCH\n  expected: {line}\n  actual:   {actual}",
                    i + 1
                ));
            }
        }
    }
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
}

/// Legacy sessions: migrate each input line, compare to the `.expected` golden.
#[test]
fn legacy_session_migration() {
    let dir = fixtures_dir().join("sessions");
    let input = fs::read_to_string(dir.join("legacy-v1.jsonl")).unwrap();
    let expected = fs::read_to_string(dir.join("legacy-v1.expected.jsonl")).unwrap();
    let mut fails = Vec::new();
    let in_lines: Vec<&str> = input.lines().filter(|l| !l.trim().is_empty()).collect();
    let exp_lines: Vec<&str> = expected.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(in_lines.len(), exp_lines.len(), "line count mismatch");
    for (i, (line, want)) in in_lines.iter().zip(exp_lines.iter()).enumerate() {
        match entry_from_json_line(line, Some(i + 1)) {
            Ok(entry) => {
                let got = entry_to_json_line(&entry);
                let got = got.trim_end_matches('\n');
                if got != *want {
                    fails.push(format!(
                        "legacy-v1:{} BYTE MISMATCH\n  expected: {want}\n  actual:   {got}",
                        i + 1
                    ));
                }
            }
            Err(e) => fails.push(format!("legacy-v1:{}: {e}", i + 1)),
        }
    }
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
}

/// `wire-legacy/*.jsonl` → migrate → compare to sibling `.expected.json`.
#[test]
fn wire_legacy_migration() {
    let dir = fixtures_dir().join("wire-legacy");
    let mut fails = Vec::new();
    for path in files_in(&dir, "jsonl") {
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let expected_path = dir.join(format!("{stem}.expected.json"));
        let input = fs::read_to_string(&path).unwrap();
        let line = input.lines().find(|l| !l.trim().is_empty()).unwrap();
        let want = fs::read_to_string(&expected_path).unwrap();
        let want = want.trim_end_matches('\n');
        match entry_from_json_line(line, None) {
            Ok(entry) => {
                let got = entry_to_json_line(&entry);
                let got = got.trim_end_matches('\n');
                if got != want {
                    fails.push(format!(
                        "wire-legacy/{stem} BYTE MISMATCH\n  expected: {want}\n  actual:   {got}"
                    ));
                }
            }
            Err(e) => fails.push(format!("wire-legacy/{stem}: {e}")),
        }
    }
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
}

/// Each line of every event-stream sequence round-trips through the typed event.
#[test]
fn event_streams_roundtrip() {
    let root = fixtures_dir().join("event-streams");
    let mut fails = Vec::new();
    let mut dirs: Vec<PathBuf> = fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for scenario in dirs {
        let name = scenario.file_name().unwrap().to_string_lossy().to_string();
        roundtrip_jsonl::<AgentEvent>(
            &scenario.join("agent-events.jsonl"),
            &format!("event-streams/{name}/agent-events.jsonl"),
            &mut fails,
        );
        roundtrip_jsonl::<AssistantMessageEvent>(
            &scenario.join("assistant-events.jsonl"),
            &format!("event-streams/{name}/assistant-events.jsonl"),
            &mut fails,
        );
    }
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
}

fn roundtrip_jsonl<T: Serialize + DeserializeOwned>(
    path: &Path,
    label: &str,
    fails: &mut Vec<String>,
) {
    let text = fs::read_to_string(path).unwrap();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if let Err(e) = roundtrip::<T>(&format!("{label}:{}", i + 1), line) {
            fails.push(e);
        }
    }
}

/// Assert `serialize ∘ parse` is idempotent: the *second* round-trip must equal
/// the first. This is the dedicated idempotence check, run over the whole
/// in-scope corpus. It is strictly weaker than the byte-golden tests above (which
/// pin the *exact* tau bytes), but it fails loudly if any type were ever
/// non-deterministic across runs.
fn idempotent<T: Serialize + DeserializeOwned>(label: &str, bytes: &str, fails: &mut Vec<String>) {
    let once: T = match serde_json::from_str(bytes.trim_end_matches('\n')) {
        Ok(v) => v,
        Err(e) => {
            fails.push(format!("{label}: parse: {e}"));
            return;
        }
    };
    let s1 = serde_json::to_string(&once).unwrap();
    let twice: T = serde_json::from_str(&s1).expect("re-parse of own output");
    let s2 = serde_json::to_string(&twice).unwrap();
    if s1 != s2 {
        fails.push(format!(
            "{label}: NON-IDEMPOTENT\n  first:  {s1}\n  second: {s2}"
        ));
    }
}

fn idempotent_dir<T: Serialize + DeserializeOwned>(sub: &str, fails: &mut Vec<String>) {
    let dir = fixtures_dir().join("wire").join(sub);
    for path in files_in(&dir, "json") {
        let bytes = fs::read_to_string(&path).unwrap();
        let label = format!(
            "idem wire/{sub}/{}",
            path.file_name().unwrap().to_string_lossy()
        );
        idempotent::<T>(&label, &bytes, fails);
    }
}

#[test]
fn corpus_idempotence() {
    let mut fails = Vec::new();
    idempotent_dir::<AgentMessage>("messages", &mut fails);
    idempotent_dir::<AnyContent>("content", &mut fails);
    idempotent_dir::<rho_agent::tools::AgentToolResult>("tool_result", &mut fails);
    idempotent_dir::<SessionEntry>("entries", &mut fails);
    idempotent_dir::<AgentEvent>("agent-events", &mut fails);
    idempotent_dir::<AssistantMessageEvent>("assistant-events", &mut fails);

    // Session JSONL lines (through the migrating decode path).
    let sessions = fixtures_dir().join("sessions");
    for name in ["linear", "branched", "compaction", "kitchen-sink"] {
        let text = fs::read_to_string(sessions.join(format!("{name}.jsonl"))).unwrap();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let entry = entry_from_json_line(line, Some(i + 1)).unwrap();
            let s1 = entry_to_json_line(&entry);
            let reparsed = entry_from_json_line(s1.trim_end_matches('\n'), None).unwrap();
            let s2 = entry_to_json_line(&reparsed);
            if s1 != s2 {
                fails.push(format!("idem sessions/{name}:{}", i + 1));
            }
        }
    }
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
}
