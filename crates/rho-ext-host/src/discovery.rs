//! Extension discovery — the WASM analogue of tau's `loader.py`.
//!
//! **Parity divergence, by construction:** a compiled WASM component is a single
//! self-contained file, so tau's Python-package machinery (submodule search
//! locations, `sys.modules` namespacing, relative imports, `src/`-layout
//! manifests) has no rho counterpart. A rho extension is a `.wasm` file (or a
//! directory containing `extension.wasm`). What rho *does* preserve from tau:
//! the directory precedence (project-first when opted in, then user), the
//! `.`/`_`-prefix skip, sorted deterministic order, explicit `-x` paths (files
//! or dirs) that load even when directory discovery is disabled, and
//! first-loaded-wins de-duplication by both resolved path and name.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::host::{ExtensionSpec, HostDiagnostic};

/// The component file name a directory-form extension must contain.
pub const DIRECTORY_ENTRY: &str = "extension.wasm";

/// Resource paths discovery reads (the slice of rho's `RhoResourcePaths` this
/// crate needs, kept local to avoid a `rho-coding` dependency).
#[derive(Debug, Clone)]
pub struct DiscoveryPaths {
    /// The rho home root (`~/.rho`); extensions live under `<root>/extensions`.
    pub root: PathBuf,
    /// The session working directory, for project-scoped extensions.
    pub cwd: Option<PathBuf>,
}

/// Extension directories in load order (project first when enabled, then user).
///
/// Mirrors tau's `extension_dirs`: project extensions shadow user ones, and are
/// opt-in (`include_project_dir`) because they execute at session startup.
#[must_use]
pub fn extension_dirs(paths: &DiscoveryPaths, include_project_dir: bool) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if include_project_dir {
        if let Some(cwd) = &paths.cwd {
            dirs.push(cwd.join(".rho").join("extensions"));
        }
    }
    dirs.push(paths.root.join("extensions"));
    dedupe_paths(dirs)
}

/// Discover extension components across resource directories and explicit paths.
///
/// Explicit `extra_paths` always load, even when `include_resource_dirs` is
/// false (the `--no-extensions` escape hatch). Returns discovered specs in load
/// order plus non-fatal diagnostics.
#[must_use]
pub fn discover_extensions(
    paths: &DiscoveryPaths,
    extra_paths: &[PathBuf],
    include_resource_dirs: bool,
    include_project_dir: bool,
) -> (Vec<ExtensionSpec>, Vec<HostDiagnostic>) {
    let mut discovered: Vec<ExtensionSpec> = Vec::new();
    let mut diagnostics: Vec<HostDiagnostic> = Vec::new();
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();
    let mut seen_names: HashSet<String> = HashSet::new();

    let mut add = |entry: ExtensionSpec,
                   discovered: &mut Vec<ExtensionSpec>,
                   diagnostics: &mut Vec<HostDiagnostic>| {
        let resolved = entry
            .path
            .canonicalize()
            .unwrap_or_else(|_| entry.path.clone());
        if seen_paths.contains(&resolved) {
            return;
        }
        if seen_names.contains(&entry.name) {
            diagnostics.push(HostDiagnostic {
                extension: entry.name.clone(),
                path: Some(entry.path.clone()),
                message: "duplicate extension name ignored (first-loaded wins)".to_string(),
                is_error: false,
            });
            return;
        }
        seen_paths.insert(resolved);
        seen_names.insert(entry.name.clone());
        discovered.push(entry);
    };

    if include_resource_dirs {
        for directory in extension_dirs(paths, include_project_dir) {
            for entry in discover_in_dir(&directory) {
                add(entry, &mut discovered, &mut diagnostics);
            }
        }
    }

    for path in extra_paths {
        let expanded = expand_user(path);
        if expanded.is_dir() {
            let entry_file = expanded.join(DIRECTORY_ENTRY);
            if entry_file.is_file() {
                let name = dir_name(&expanded);
                add(
                    ExtensionSpec {
                        name,
                        path: entry_file,
                    },
                    &mut discovered,
                    &mut diagnostics,
                );
                continue;
            }
            let mut found_any = false;
            for entry in discover_in_dir(&expanded) {
                found_any = true;
                add(entry, &mut discovered, &mut diagnostics);
            }
            if !found_any {
                diagnostics.push(HostDiagnostic {
                    extension: String::new(),
                    path: Some(expanded.clone()),
                    message: "no extensions found in explicit extension path".to_string(),
                    is_error: false,
                });
            }
        } else if expanded.is_file() {
            add(
                ExtensionSpec {
                    name: file_stem(&expanded),
                    path: expanded.clone(),
                },
                &mut discovered,
                &mut diagnostics,
            );
        } else {
            diagnostics.push(HostDiagnostic {
                extension: String::new(),
                path: Some(expanded.clone()),
                message: "explicit extension path does not exist".to_string(),
                is_error: true,
            });
        }
    }

    (discovered, diagnostics)
}

/// Discover `.wasm` files and `extension.wasm` directories inside one directory,
/// in deterministic (name-sorted) order, skipping `.`/`_`-prefixed entries.
fn discover_in_dir(directory: &Path) -> Vec<ExtensionSpec> {
    let Ok(read_dir) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut entries: Vec<PathBuf> = read_dir.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort_by_key(|a| file_name(a));

    let mut specs = Vec::new();
    for path in entries {
        let name = file_name(&path);
        if name.starts_with('_') || name.starts_with('.') {
            continue;
        }
        if path.is_file() && path.extension().is_some_and(|ext| ext == "wasm") {
            specs.push(ExtensionSpec {
                name: file_stem(&path),
                path,
            });
        } else if path.is_dir() {
            let entry_file = path.join(DIRECTORY_ENTRY);
            if entry_file.is_file() {
                specs.push(ExtensionSpec {
                    name: dir_name(&path),
                    path: entry_file,
                });
            }
        }
    }
    specs
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::new();
    for path in paths {
        let expanded = expand_user(&path);
        if seen.insert(expanded.clone()) {
            out.push(expanded);
        }
    }
    out
}

/// Expand a leading `~` against `$HOME` (best-effort; leaves the path untouched
/// when it cannot be expanded).
fn expand_user(path: &Path) -> PathBuf {
    if let Ok(stripped) = path.strip_prefix("~") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    }
    path.to_path_buf()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn dir_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
