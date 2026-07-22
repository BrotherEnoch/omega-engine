// crates/omega-relay/src/blacklist.rs
//! MEV-Boost builder blacklist (Â§12.3, V1).
//!
//! ## Spec requirements
//! - Storage: off-chain `config/builder_blacklist.toml`
//! - Hot-reload without restart via `reload()`
//! - Applied at bundle submission time: skip relays that cannot exclude blacklisted builders
//! - Phase 4+ L1 only (Arbitrum uses direct sequencer â€” no MEV-Boost)
//! - Quarterly governance review; additions via L2 fast-approve; removals via L3 (48 h timelock)

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::error::{RelayError, RelayResult};

// â”€â”€ TOML schema â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Matches a single `[[blacklisted_builders]]` entry in `config/builder_blacklist.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlacklistedBuilderEntry {
    /// Builder public key or coinbase address (hex, `0x`-prefixed).
    pub key: String,
    /// Human-readable reason for blacklisting.
    pub reason: String,
    /// ISO-8601 date the entry was added.
    pub added: String,
}

/// Top-level structure of `config/builder_blacklist.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BlacklistFile {
    #[serde(default)]
    blacklisted_builders: Vec<BlacklistedBuilderEntry>,
}

// â”€â”€ BuilderBlacklist â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Thread-safe, hot-reloadable builder blacklist.
///
/// All lookups are O(1) via a `HashSet` of normalised keys.
/// The full entry list is retained for API responses and audit logs.
pub struct BuilderBlacklist {
    /// Normalised (lowercase, `0x`-prefixed) keys for O(1) lookup.
    keys: RwLock<HashSet<String>>,
    /// Full entry list for API / audit.
    entries: RwLock<Vec<BlacklistedBuilderEntry>>,
    /// Path to the TOML file for hot-reload.
    path: PathBuf,
}

impl BuilderBlacklist {
    /// Load from `path`.  Fails loudly â€” a missing blacklist file at startup
    /// is a configuration error.
    pub fn load(path: impl AsRef<Path>) -> RelayResult<Arc<Self>> {
        let path = path.as_ref().to_path_buf();
        let bl = Arc::new(Self {
            keys: RwLock::new(HashSet::new()),
            entries: RwLock::new(Vec::new()),
            path,
        });
        bl.reload_inner()?;
        Ok(bl)
    }

    /// Hot-reload the blacklist from disk without restarting.
    /// Called by `POST /api/v1/builders/blacklist/update` handler.
    pub fn reload(&self) -> RelayResult<usize> {
        self.reload_inner()?;
        Ok(self.entries.read().len())
    }

    fn reload_inner(&self) -> RelayResult<()> {
        let raw = std::fs::read_to_string(&self.path).map_err(|e| RelayError::BlacklistLoadFailed {
            path: self.path.display().to_string(),
            source: e,
        })?;

        let file: BlacklistFile = toml::from_str(&raw)
            .map_err(|e| RelayError::BlacklistParseFailed(e.to_string()))?;

        let new_keys: HashSet<String> = file
            .blacklisted_builders
            .iter()
            .map(|e| normalise_key(&e.key))
            .collect();

        let count = new_keys.len();
        *self.keys.write() = new_keys;
        *self.entries.write() = file.blacklisted_builders;

        info!(path = %self.path.display(), entries = count, "builder blacklist reloaded");
        Ok(())
    }

    /// Returns `true` if `key` appears in the blacklist.
    /// `key` is normalised before lookup, so `0XABC` and `0xabc` match.
    #[inline]
    pub fn contains(&self, key: &str) -> bool {
        self.keys.read().contains(&normalise_key(key))
    }

    /// All current entries â€” used by `GET /api/v1/builders/blacklist`.
    pub fn entries(&self) -> Vec<BlacklistedBuilderEntry> {
        self.entries.read().clone()
    }

    /// Add an entry at runtime (L2 fast-approve path).
    /// The entry is written back to disk so it survives restart.
    pub fn add_entry(&self, entry: BlacklistedBuilderEntry) -> RelayResult<()> {
        let norm = normalise_key(&entry.key);
        {
            let mut keys = self.keys.write();
            let mut entries = self.entries.write();
            if keys.contains(&norm) {
                warn!(key = %norm, "blacklist add: key already present");
                return Ok(());
            }
            keys.insert(norm);
            entries.push(entry);
        }
        self.persist()
    }

    /// Remove an entry at runtime (L3 48 h timelock path).
    pub fn remove_entry(&self, key: &str) -> RelayResult<bool> {
        let norm = normalise_key(key);
        let removed = {
            let mut keys = self.keys.write();
            let mut entries = self.entries.write();
            let before = keys.len();
            keys.remove(&norm);
            entries.retain(|e| normalise_key(&e.key) != norm);
            keys.len() < before
        };
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    fn persist(&self) -> RelayResult<()> {
        let file = BlacklistFile {
            blacklisted_builders: self.entries.read().clone(),
        };
        let raw = toml::to_string_pretty(&file)
            .map_err(|e| RelayError::BlacklistParseFailed(e.to_string()))?;
        std::fs::write(&self.path, raw)?;
        info!(path = %self.path.display(), "builder blacklist persisted to disk");
        Ok(())
    }
}

/// Normalise a builder key to lowercase, ensuring `0x` prefix.
fn normalise_key(key: &str) -> String {
    let trimmed = key.trim().to_lowercase();
    if trimmed.starts_with("0x") {
        trimmed
    } else {
        format!("0x{trimmed}")
    }
}

// â”€â”€ Tests â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_blacklist(keys: &[(&str, &str)]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        for (key, reason) in keys {
            writeln!(
                f,
                "[[blacklisted_builders]]\nkey = \"{key}\"\nreason = \"{reason}\"\nadded = \"2026-04-19\"\n"
            )
            .unwrap();
        }
        f
    }

    #[test]
    fn contains_normalises_case() {
        let f = write_blacklist(&[("0xDeAdBeEf", "test")]);
        let bl = BuilderBlacklist::load(f.path()).unwrap();
        assert!(bl.contains("0xdeadbeef"));
        assert!(bl.contains("0XDEADBEEF"));
        assert!(bl.contains("deadbeef")); // no 0x prefix
        assert!(!bl.contains("0xcafe"));
    }

    #[test]
    fn empty_blacklist_is_valid() {
        let f = write_blacklist(&[]);
        let bl = BuilderBlacklist::load(f.path()).unwrap();
        assert!(!bl.contains("0xanything"));
        assert!(bl.entries().is_empty());
    }

    #[test]
    fn hot_reload_picks_up_new_entries() {
        let mut f = write_blacklist(&[("0xaaa", "first")]);
        let bl = BuilderBlacklist::load(f.path()).unwrap();
        assert!(bl.contains("0xaaa"));
        assert!(!bl.contains("0xbbb"));

        // Overwrite file
        f.rewind().unwrap();
        f.as_file_mut().set_len(0).unwrap();
        writeln!(
            f,
            "[[blacklisted_builders]]\nkey = \"0xbbb\"\nreason = \"second\"\nadded = \"2026-04-20\"\n"
        )
        .unwrap();
        f.flush().unwrap();

        let n = bl.reload().unwrap();
        assert_eq!(n, 1);
        assert!(!bl.contains("0xaaa"), "old key must be gone after reload");
        assert!(bl.contains("0xbbb"), "new key must appear after reload");
    }

    #[test]
    fn add_and_remove_runtime() {
        let f = write_blacklist(&[("0xaaa", "seed")]);
        let bl = BuilderBlacklist::load(f.path()).unwrap();

        bl.add_entry(BlacklistedBuilderEntry {
            key: "0xbbb".into(),
            reason: "runtime add".into(),
            added: "2026-05-01".into(),
        })
        .unwrap();
        assert!(bl.contains("0xbbb"));

        let removed = bl.remove_entry("0xaaa").unwrap();
        assert!(removed);
        assert!(!bl.contains("0xaaa"));

        // Idempotent remove
        let removed2 = bl.remove_entry("0xaaa").unwrap();
        assert!(!removed2);
    }
}