//! Session storage trait and JSONL implementation (tau
//! `tau_agent/session/storage.py`).
//!
//! [`SessionStorage`] is tau's append-only `Protocol`. rho renders it with
//! [`async_trait`] so it stays object-safe (`dyn SessionStorage`) — the coding
//! layer and CLI hold storage behind a trait object. [`JsonlSessionStorage`]
//! appends one `exclude_none` JSONL line per entry (via M1's
//! [`entry_to_json_line`]) and reads the whole file back through the migrating
//! decoder. Like tau, the file I/O itself is synchronous inside the async method
//! (there is no `.await` on the filesystem); the async signature is what the
//! trait requires.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::session::entries::SessionEntry;
use crate::session::jsonl::{SessionJsonlError, entries_from_json_lines, entry_to_json_line};

/// An I/O error from session storage.
#[derive(Debug, thiserror::Error)]
pub enum SessionStorageError {
    /// A filesystem operation failed.
    #[error("session storage io error: {0}")]
    Io(#[from] std::io::Error),
    /// A stored line failed to decode.
    #[error(transparent)]
    Decode(#[from] SessionJsonlError),
}

/// Append-only session storage (tau `SessionStorage`).
#[async_trait]
pub trait SessionStorage: Send + Sync {
    /// Append one entry to storage.
    async fn append(&self, entry: &SessionEntry) -> Result<(), SessionStorageError>;

    /// Read all entries in storage order.
    async fn read_all(&self) -> Result<Vec<SessionEntry>, SessionStorageError>;

    /// Return the backing file path when this storage is file-backed.
    ///
    /// tau's `_storage_path` `isinstance`-checks `JsonlSessionStorage` and reads
    /// its `.path`; rho exposes it as a trait method that defaults to `None`
    /// (in-memory storage) and is overridden by [`JsonlSessionStorage`].
    fn storage_path(&self) -> Option<PathBuf> {
        None
    }
}

/// Local append-only JSONL session storage (tau `JsonlSessionStorage`).
#[derive(Debug, Clone)]
pub struct JsonlSessionStorage {
    /// The backing file path.
    pub path: PathBuf,
}

impl JsonlSessionStorage {
    /// Build storage backed by `path`.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }
}

#[async_trait]
impl SessionStorage for JsonlSessionStorage {
    async fn append(&self, entry: &SessionEntry) -> Result<(), SessionStorageError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(entry_to_json_line(entry).as_bytes())?;
        Ok(())
    }

    fn storage_path(&self) -> Option<PathBuf> {
        Some(self.path.clone())
    }

    async fn read_all(&self) -> Result<Vec<SessionEntry>, SessionStorageError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&self.path)?;
        // Split on newlines only: Python's str.splitlines() (and Rust's
        // str::lines()) would also break on characters like U+2028 that appear
        // unescaped inside JSON string values. tau splits on "\n" alone here.
        let lines: Vec<&str> = text.split('\n').collect();
        Ok(entries_from_json_lines(&lines)?)
    }
}
