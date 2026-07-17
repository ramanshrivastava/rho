//! Local credential storage for rho provider credentials.
//!
//! Port of tau's `tau_coding/credentials.py` (`FileCredentialStore`,
//! `OAuthCredential`, `ApiKeyCredential`, `CredentialStoreError`,
//! `credentials_path`). The store is a JSON object under rho home
//! (`~/.rho/credentials.json`), written with `json.dumps(indent=2,
//! sort_keys=True) + "\n"` via a temp-file + atomic rename, `chmod 0o600`.
//!
//! rho reproduces tau's on-disk bytes exactly: sorted keys, 2-space indent,
//! `ensure_ascii` string escaping, a trailing newline, and the same
//! `OAuthCredential.to_json` field ordering. Validation error messages are kept
//! verbatim (tau's literal "Tau ..." prose), matching how other rho ports keep
//! tau's exact user-facing strings for byte-parity.

// This module is dense with Python/JSON identifiers in prose.
#![allow(clippy::doc_markdown)]

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use rho_agent::types::JsonMap;
use serde_json::Value;

use crate::paths::RhoPaths;

/// Raised when rho credential storage cannot be read or written.
///
/// Ports tau's `CredentialStoreError(ValueError)`; errors are data (returned as
/// `Err`), never panics.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CredentialStoreError(pub String);

impl CredentialStoreError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Refreshable OAuth credential persisted under rho home.
///
/// `account_id` stays optional so legacy OpenAI Codex credentials load unchanged
/// while device-code providers persist only the metadata they receive.
/// Provider-specific, non-secret values live in `metadata`.
#[derive(Debug, Clone, PartialEq)]
pub struct OAuthCredential {
    /// Short-lived access token used to authenticate requests.
    pub access: String,
    /// Long-lived refresh token used to mint new access tokens.
    pub refresh: String,
    /// Access-token expiry, in integer milliseconds since the epoch.
    pub expires: i64,
    /// Optional provider account id (e.g. ChatGPT account id).
    pub account_id: Option<String>,
    /// Provider-specific, non-secret JSON metadata.
    pub metadata: JsonMap,
}

impl OAuthCredential {
    /// Build an OAuth credential with no `account_id` and empty metadata.
    #[must_use]
    pub fn new(access: impl Into<String>, refresh: impl Into<String>, expires: i64) -> Self {
        Self {
            access: access.into(),
            refresh: refresh.into(),
            expires,
            account_id: None,
            metadata: JsonMap::new(),
        }
    }

    /// Set the `account_id` field.
    #[must_use]
    pub fn with_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = Some(account_id.into());
        self
    }

    /// Set the `metadata` field.
    #[must_use]
    pub fn with_metadata(mut self, metadata: JsonMap) -> Self {
        self.metadata = metadata;
        self
    }

    /// Serialize this OAuth credential to a JSON object.
    ///
    /// Field insertion order matches tau exactly: `type`, `access`, `refresh`,
    /// `expires`, then optional `account_id`, then optional `metadata` (omitted
    /// when empty). The credential file itself is dumped with `sort_keys=True`,
    /// so this insertion order only matters for callers that read the map
    /// directly.
    #[must_use]
    pub fn to_json(&self) -> JsonMap {
        let mut result = JsonMap::new();
        result.insert("type".to_string(), Value::from("oauth"));
        result.insert("access".to_string(), Value::from(self.access.clone()));
        result.insert("refresh".to_string(), Value::from(self.refresh.clone()));
        result.insert("expires".to_string(), Value::from(self.expires));
        if let Some(account_id) = &self.account_id {
            result.insert("account_id".to_string(), Value::from(account_id.clone()));
        }
        if !self.metadata.is_empty() {
            result.insert("metadata".to_string(), Value::Object(self.metadata.clone()));
        }
        result
    }
}

/// API-key credential persisted under rho home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyCredential {
    /// The stored API key value.
    pub key: String,
}

impl ApiKeyCredential {
    /// Serialize this API-key credential to a JSON object (`type`, `key`).
    #[must_use]
    pub fn to_json(&self) -> JsonMap {
        let mut result = JsonMap::new();
        result.insert("type".to_string(), Value::from("api_key"));
        result.insert("key".to_string(), Value::from(self.key.clone()));
        result
    }
}

/// A stored credential: a bare API-key string, an [`ApiKeyCredential`] object,
/// or an [`OAuthCredential`] (tau's `StoredCredential` union).
///
/// `set`/`set_api_key` persist the bare-string form (matching tau); the object
/// [`ApiKeyCredential`] form only ever arises when loading a `{"type":
/// "api_key", ...}` entry written by another tool.
#[derive(Debug, Clone, PartialEq)]
pub enum StoredCredential {
    /// A bare API-key string (tau's legacy/default form written by `set`).
    Str(String),
    /// An API-key credential object.
    ApiKey(ApiKeyCredential),
    /// An OAuth credential object.
    OAuth(OAuthCredential),
}

impl StoredCredential {
    fn to_json(&self) -> Value {
        match self {
            Self::Str(value) => Value::from(value.clone()),
            Self::ApiKey(credential) => Value::Object(credential.to_json()),
            Self::OAuth(credential) => Value::Object(credential.to_json()),
        }
    }
}

/// Small JSON-backed provider credential store under rho home.
#[derive(Debug, Clone)]
pub struct FileCredentialStore {
    /// The credential file path.
    pub path: PathBuf,
}

impl FileCredentialStore {
    /// Build a store backed by an explicit path.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Build a store backed by the default [`credentials_path`].
    #[must_use]
    pub fn at_default() -> Self {
        Self::new(credentials_path(None))
    }

    /// Return a stored API-key credential value by name.
    ///
    /// Returns the bare-string value, or an object API-key's `key`; OAuth
    /// entries return `None` (use [`Self::get_oauth`]).
    pub fn get(&self, name: &str) -> Result<Option<String>, CredentialStoreError> {
        Ok(match self.load()?.get(name) {
            Some(StoredCredential::Str(value)) => Some(value.clone()),
            Some(StoredCredential::ApiKey(credential)) => Some(credential.key.clone()),
            _ => None,
        })
    }

    /// Store an API-key credential value by name.
    pub fn set(&self, name: &str, value: &str) -> Result<(), CredentialStoreError> {
        let name = validate_credential_name(name)?;
        let value = value.trim();
        if value.is_empty() {
            return Err(CredentialStoreError::new(
                "Credential value must not be empty",
            ));
        }
        let mut data = self.load()?;
        data.insert(name, StoredCredential::Str(value.to_string()));
        self.save(&data)
    }

    /// Store an API-key credential value by name (alias of [`Self::set`]).
    pub fn set_api_key(&self, name: &str, value: &str) -> Result<(), CredentialStoreError> {
        self.set(name, value)
    }

    /// Return a stored OAuth credential by name.
    pub fn get_oauth(&self, name: &str) -> Result<Option<OAuthCredential>, CredentialStoreError> {
        Ok(match self.load()?.get(name) {
            Some(StoredCredential::OAuth(credential)) => Some(credential.clone()),
            _ => None,
        })
    }

    /// Store a refreshable OAuth credential by name.
    pub fn set_oauth(
        &self,
        name: &str,
        credential: OAuthCredential,
    ) -> Result<(), CredentialStoreError> {
        let name = validate_credential_name(name)?;
        validate_oauth_credential(&credential)?;
        let mut data = self.load()?;
        data.insert(name, StoredCredential::OAuth(credential));
        self.save(&data)
    }

    /// Delete a stored credential value by name.
    pub fn delete(&self, name: &str) -> Result<(), CredentialStoreError> {
        let mut data = self.load()?;
        data.remove(name);
        self.save(&data)
    }

    fn load(&self) -> Result<CredentialMap, CredentialStoreError> {
        if !self.path.exists() {
            return Ok(CredentialMap::new());
        }
        let text = std::fs::read_to_string(&self.path)
            .map_err(|error| CredentialStoreError::new(error.to_string()))?;
        let raw: Value = serde_json::from_str(&text)
            .map_err(|error| CredentialStoreError::new(error.to_string()))?;
        let Value::Object(object) = raw else {
            return Err(CredentialStoreError::new(
                "Tau credentials must be a JSON object",
            ));
        };
        let mut credentials = CredentialMap::new();
        for (key, value) in object {
            credentials.insert(key, credential_from_json(&value)?);
        }
        Ok(credentials)
    }

    fn save(&self, data: &CredentialMap) -> Result<(), CredentialStoreError> {
        let parent = self
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|error| CredentialStoreError::new(error.to_string()))?;

        let mut raw = serde_json::Map::new();
        for (key, value) in data {
            raw.insert(key.clone(), value.to_json());
        }
        let mut content = dumps_pretty_sorted(&Value::Object(raw));
        content.push('\n');

        let file_name = self.path.file_name().map_or_else(
            || "credentials.json".to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let mut temp = tempfile::Builder::new()
            .prefix(&format!(".{file_name}."))
            .tempfile_in(parent)
            .map_err(|error| CredentialStoreError::new(error.to_string()))?;
        set_owner_only_permissions(temp.as_file())?;
        temp.write_all(content.as_bytes())
            .map_err(|error| CredentialStoreError::new(error.to_string()))?;
        temp.flush()
            .map_err(|error| CredentialStoreError::new(error.to_string()))?;
        let file = temp
            .persist(&self.path)
            .map_err(|error| CredentialStoreError::new(error.to_string()))?;
        set_owner_only_permissions(&file)?;
        Ok(())
    }
}

/// Insertion-order-preserving credential map (output is sorted at write time, so
/// the in-memory order is immaterial; a map keeps set/delete O(1)).
type CredentialMap = std::collections::BTreeMap<String, StoredCredential>;

/// Return rho's local provider credential path (`<home>/credentials.json`).
#[must_use]
pub fn credentials_path(paths: Option<&RhoPaths>) -> PathBuf {
    let home = paths.map_or_else(|| RhoPaths::default().home, |paths| paths.home.clone());
    home.join("credentials.json")
}

/// Set `0o600` (owner read/write only) permissions on a file, matching tau's
/// `chmod(0o600)`. A no-op on non-Unix targets.
#[allow(clippy::unnecessary_wraps)]
fn set_owner_only_permissions(file: &std::fs::File) -> Result<(), CredentialStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| CredentialStoreError::new(error.to_string()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = file;
    }
    Ok(())
}

fn validate_credential_name(name: &str) -> Result<String, CredentialStoreError> {
    let normalized = name.trim();
    if normalized.is_empty() {
        return Err(CredentialStoreError::new(
            "Credential name must not be empty",
        ));
    }
    Ok(normalized.to_string())
}

fn validate_oauth_credential(credential: &OAuthCredential) -> Result<(), CredentialStoreError> {
    if credential.access.trim().is_empty() {
        return Err(CredentialStoreError::new(
            "OAuth access token must not be empty",
        ));
    }
    if credential.refresh.trim().is_empty() {
        return Err(CredentialStoreError::new(
            "OAuth refresh token must not be empty",
        ));
    }
    if let Some(account_id) = &credential.account_id {
        if account_id.trim().is_empty() {
            return Err(CredentialStoreError::new(
                "OAuth account id must not be empty",
            ));
        }
    }
    if credential.expires <= 0 {
        return Err(CredentialStoreError::new(
            "OAuth expiry must be greater than 0",
        ));
    }
    validate_oauth_metadata(&credential.metadata)
}

fn credential_from_json(value: &Value) -> Result<StoredCredential, CredentialStoreError> {
    if let Value::String(text) = value {
        return Ok(StoredCredential::Str(text.clone()));
    }
    let Value::Object(object) = value else {
        return Err(CredentialStoreError::new(
            "Tau credential values must be strings or objects",
        ));
    };

    let credential_type = object.get("type").and_then(Value::as_str);
    match credential_type {
        Some("api_key") => {
            let key = string_field(object, "key", "api_key")?;
            Ok(StoredCredential::ApiKey(ApiKeyCredential { key }))
        }
        Some("oauth") => {
            let expires = positive_integer(object.get("expires")).ok_or_else(|| {
                CredentialStoreError::new("Tau oauth credential expires must be a positive integer")
            })?;
            let account_id = match object.get("account_id") {
                None | Some(Value::Null) => None,
                Some(Value::String(text)) if !text.trim().is_empty() => Some(text.clone()),
                Some(_) => {
                    return Err(CredentialStoreError::new(
                        "Tau oauth credential account_id must be a non-empty string",
                    ));
                }
            };
            let metadata = match object.get("metadata") {
                None => JsonMap::new(),
                Some(Value::Object(map)) => map.clone(),
                Some(_) => {
                    return Err(CredentialStoreError::new(
                        "Tau oauth credential metadata must be an object",
                    ));
                }
            };
            validate_oauth_metadata(&metadata)?;
            Ok(StoredCredential::OAuth(OAuthCredential {
                access: string_field(object, "access", "oauth")?,
                refresh: string_field(object, "refresh", "oauth")?,
                expires,
                account_id,
                metadata,
            }))
        }
        _ => Err(CredentialStoreError::new(
            "Tau credential object type must be api_key or oauth",
        )),
    }
}

/// Return the integer value of `value` iff it is a positive JSON integer.
///
/// Mirrors tau's `isinstance(expires, int) and not isinstance(expires, bool)
/// and expires > 0`: a JSON `true`/`false` parses to [`Value::Bool`] (rejected),
/// and a float such as `123.0` parses to a non-integer number (rejected).
fn positive_integer(value: Option<&Value>) -> Option<i64> {
    let number = value?.as_number()?;
    if let Some(signed) = number.as_i64() {
        (signed > 0).then_some(signed)
    } else {
        number.as_u64().and_then(|unsigned| {
            (unsigned > 0)
                .then(|| i64::try_from(unsigned).ok())
                .flatten()
        })
    }
}

fn validate_oauth_metadata(metadata: &JsonMap) -> Result<(), CredentialStoreError> {
    for key in metadata.keys() {
        if key.trim().is_empty() {
            return Err(CredentialStoreError::new(
                "Tau oauth credential metadata keys must be strings",
            ));
        }
    }
    Ok(())
}

fn string_field(
    object: &JsonMap,
    field_name: &str,
    credential_type: &str,
) -> Result<String, CredentialStoreError> {
    match object.get(field_name).and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => Ok(text.trim().to_string()),
        _ => Err(CredentialStoreError::new(format!(
            "Tau {credential_type} credential field must be a non-empty string: {field_name}"
        ))),
    }
}

/// Serialize a JSON value like Python's `json.dumps(value, indent=2,
/// sort_keys=True)`: sorted object keys, 2-space indentation, `ensure_ascii`
/// string escaping, `float.__repr__` numbers, and `{}`/`[]` for empty
/// containers.
fn dumps_pretty_sorted(value: &Value) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out
}

fn write_pretty(value: &Value, indent: usize, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => {
            if number.is_f64() {
                out.push_str(&crate::pystr::python_float_repr(
                    number.as_f64().unwrap_or(0.0),
                ));
            } else {
                out.push_str(&number.to_string());
            }
        }
        Value::String(text) => write_json_string(text, out),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                push_indent(indent + 2, out);
                write_pretty(item, indent + 2, out);
                if index + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(indent, out);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push_str("{\n");
            for (index, key) in keys.iter().enumerate() {
                push_indent(indent + 2, out);
                write_json_string(key, out);
                out.push_str(": ");
                write_pretty(&map[*key], indent + 2, out);
                if index + 1 < keys.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(indent, out);
            out.push('}');
        }
    }
}

fn push_indent(spaces: usize, out: &mut String) {
    for _ in 0..spaces {
        out.push(' ');
    }
}

/// Write a JSON string literal with Python `json` escaping (`ensure_ascii=True`).
fn write_json_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (u32::from(c)) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c if c.is_ascii() => out.push(c),
            c => {
                let cp = u32::from(c);
                if cp <= 0xFFFF {
                    let _ = write!(out, "\\u{cp:04x}");
                } else {
                    let value = cp - 0x10000;
                    let high = 0xD800 + (value >> 10);
                    let low = 0xDC00 + (value & 0x3FF);
                    let _ = write!(out, "\\u{high:04x}\\u{low:04x}");
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn temp_store() -> (tempfile::TempDir, FileCredentialStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("credentials.json"));
        (dir, store)
    }

    #[test]
    fn round_trips_and_sets_private_permissions() {
        let (dir, store) = temp_store();
        store.set("openai", "test-key").unwrap();

        assert_eq!(store.get("openai").unwrap().as_deref(), Some("test-key"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&store.path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        drop(dir);
    }

    #[test]
    fn deletes_key() {
        let (_dir, store) = temp_store();
        store.set("openai", "test-key").unwrap();
        store.delete("openai").unwrap();
        assert_eq!(store.get("openai").unwrap(), None);
    }

    #[test]
    fn rejects_empty_values() {
        let (_dir, store) = temp_store();
        let error = store.set("openai", "").unwrap_err();
        assert!(error.to_string().contains("must not be empty"));
    }

    #[test]
    fn round_trips_oauth_credentials() {
        let (_dir, store) = temp_store();
        let credential = OAuthCredential::new("access-token", "refresh-token", 123_456)
            .with_account_id("account-1");
        store.set_oauth("openai-codex", credential.clone()).unwrap();

        assert_eq!(store.get("openai-codex").unwrap(), None);
        assert_eq!(store.get_oauth("openai-codex").unwrap(), Some(credential));
        let text = fs::read_to_string(&store.path).unwrap();
        assert!(text.contains("\"type\": \"oauth\""));
    }

    #[test]
    fn round_trips_extensible_oauth_metadata() {
        let (dir, store) = temp_store();
        let mut metadata = JsonMap::new();
        metadata.insert("enterprise_domain".into(), Value::from("ghe.example.com"));
        metadata.insert(
            "available_model_ids".into(),
            serde_json::json!(["gpt-5.4", "claude-sonnet-4.6"]),
        );
        let credential =
            OAuthCredential::new("copilot-access", "github-token", 123_456).with_metadata(metadata);
        store
            .set_oauth("github-copilot", credential.clone())
            .unwrap();

        assert_eq!(store.get_oauth("github-copilot").unwrap(), Some(credential));
        let text = fs::read_to_string(&store.path).unwrap();
        assert!(!text.contains("\"account_id\""));
        // No leftover temp files matching `.credentials.json.*`.
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".credentials.json.")
            })
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn loads_legacy_codex_oauth_shape() {
        let (_dir, store) = temp_store();
        fs::write(
            &store.path,
            r#"{"openai-codex":{"type":"oauth","access":"a","refresh":"r","expires":123,"account_id":"account"}}"#,
        )
        .unwrap();

        let credential = store.get_oauth("openai-codex").unwrap();
        assert_eq!(
            credential,
            Some(OAuthCredential::new("a", "r", 123).with_account_id("account"))
        );
    }

    #[test]
    fn oauth_file_bytes_are_sorted_indented_and_newline_terminated() {
        let (_dir, store) = temp_store();
        store
            .set_oauth(
                "openai-codex",
                OAuthCredential::new("a", "r", 123).with_account_id("acc"),
            )
            .unwrap();
        let text = fs::read_to_string(&store.path).unwrap();
        // Sorted keys, 2-space indent, trailing newline — byte-for-byte identical
        // to Python `json.dumps(raw, indent=2, sort_keys=True) + "\n"`.
        let expected = "{\n  \"openai-codex\": {\n    \"access\": \"a\",\n    \"account_id\": \"acc\",\n    \"expires\": 123,\n    \"refresh\": \"r\",\n    \"type\": \"oauth\"\n  }\n}\n";
        assert_eq!(text, expected);
    }

    #[test]
    fn api_key_stored_as_bare_string() {
        let (_dir, store) = temp_store();
        store.set("openai", "sk-123").unwrap();
        let text = fs::read_to_string(&store.path).unwrap();
        assert_eq!(text, "{\n  \"openai\": \"sk-123\"\n}\n");
    }

    #[test]
    fn rejects_non_object_file() {
        let (_dir, store) = temp_store();
        fs::write(&store.path, "[]").unwrap();
        let error = store.get("x").unwrap_err();
        assert_eq!(error.to_string(), "Tau credentials must be a JSON object");
    }

    #[test]
    fn set_oauth_validates_fields() {
        let (_dir, store) = temp_store();
        let error = store
            .set_oauth("p", OAuthCredential::new("", "r", 1))
            .unwrap_err();
        assert_eq!(error.to_string(), "OAuth access token must not be empty");
        let error = store
            .set_oauth("p", OAuthCredential::new("a", "r", 0))
            .unwrap_err();
        assert_eq!(error.to_string(), "OAuth expiry must be greater than 0");
    }

    #[test]
    fn credentials_path_defaults_to_home() {
        let paths = RhoPaths::new(PathBuf::from("/tmp/home"), PathBuf::from("/tmp/agents"));
        assert_eq!(
            credentials_path(Some(&paths)),
            PathBuf::from("/tmp/home/credentials.json")
        );
    }

    #[test]
    fn ensure_ascii_escapes_non_ascii_metadata() {
        let (_dir, store) = temp_store();
        let mut metadata = JsonMap::new();
        metadata.insert("label".into(), Value::from("caf\u{e9}"));
        store
            .set_oauth(
                "p",
                OAuthCredential::new("a", "r", 1).with_metadata(metadata),
            )
            .unwrap();
        let text = fs::read_to_string(&store.path).unwrap();
        assert!(text.contains("\"caf\\u00e9\""), "{text}");
    }
}
