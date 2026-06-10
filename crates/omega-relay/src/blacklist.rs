// crates/omega-relay/src/blacklist.rs
//! MEV-Boost builder blacklist (§12.3, V1).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::error::{RelayError, RelayResult};

// ── TOML schema ───────────────────────────────────────────────────────────────

/// A single `[[blacklisted_builders]]` entry in `config/builder_blacklist.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlacklistedBuilderEntry {
    /// Builder public key or coinbase address (hex, `0x`-prefixed).
    pub key:    String,
    /// Human-readable reason for blacklisting.
    pub reason: String,
    /// ISO-8601 date the entry was added.
    pub added:  String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BlacklistFile {
    #[serde(default)]
    blacklisted_builders: Vec<BlacklistedBuilderEntry>,
}

// ── BuilderBlacklist ──────────────────────────────────────────────────────────

/// Thread-safe, hot-reloadable builder blacklist (§12.3).
pub struct BuilderBlacklist {
    keys:    RwLock<HashSet<String>>,
    entries: RwLock<Vec<BlacklistedBuilderEntry>>,
    path:    PathBuf,
}

impl BuilderBlacklist {
    /// Load from `path`.
    pub fn load(path: impl AsRef<Path>) -> RelayResult<Arc<Self>> {
        let path = path.as_ref().to_path_buf();
        let bl   = Arc::new(Self {
            keys:    RwLock::new(HashSet::new()),
            entries: RwLock::new(Vec::new()),
            path,
        });
        bl.reload_inner()?;
        Ok(bl)
    }

    /// Hot-reload the blacklist from disk without restarting.
    pub fn reload(&self) -> RelayResult<usize> {
        self.reload_inner()?;
        Ok(self.entries.read().len())
    }

    fn reload_inner(&self) -> RelayResult<()> {
        let raw = std::fs::read_to_string(&self.path)
            .map_err(|e| RelayError::BlacklistLoadFailed {
                path:   self.path.display().to_string(),
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
        *self.keys.write()    = new_keys;
        *self.entries.write() = file.blacklisted_builders;

        info!(path = %self.path.display(), entries = count, "builder blacklist reloaded");
        Ok(())
    }

    /// Returns `true` if `key` appears in the blacklist.
    #[inline]
    pub fn contains(&self, key: &str) -> bool {
        self.keys.read().contains(&normalise_key(key))
    }

    /// All current entries.
    pub fn entries(&self) -> Vec<BlacklistedBuilderEntry> {
        self.entries.read().clone()
    }

    /// Add an entry at runtime (L2 fast-approve path).
    pub fn add_entry(&self, entry: BlacklistedBuilderEntry) -> RelayResult<()> {
        let norm = normalise_key(&entry.key);
        {
            let mut keys    = self.keys.write();
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
        let norm    = normalise_key(key);
        let removed = {
            let mut keys    = self.keys.write();
            let mut entries = self.entries.write();
            let before      = keys.len();
            keys.remove(&norm);
            entries.retain(|e| normalise_key(&e.key) != norm);
            keys.len() < before
        };
        if removed { self.persist()?; }
        Ok(removed)
    }

    fn persist(&self) -> RelayResult<()> {
        let file = BlacklistFile { blacklisted_builders: self.entries.read().clone() };
        let raw  = toml::to_string_pretty(&file)
            .map_err(|e| RelayError::BlacklistParseFailed(e.to_string()))?;
        std::fs::write(&self.path, raw)?;
        info!(path = %self.path.display(), "builder blacklist persisted to disk");
        Ok(())
    }
}

fn normalise_key(key: &str) -> String {
    let trimmed = key.trim().to_lowercase();
    if trimmed.starts_with("0x") { trimmed } else { format!("0x{trimmed}") }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::{Seek, Write};
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
        let f  = write_blacklist(&[("0xDeAdBeEf", "test")]);
        let bl = BuilderBlacklist::load(f.path()).unwrap();
        assert!( bl.contains("0xdeadbeef"));
        assert!( bl.contains("0XDEADBEEF"));
        assert!( bl.contains("deadbeef"));
        assert!(!bl.contains("0xcafe"));
    }

    #[test]
    fn empty_blacklist_is_valid() {
        let f  = write_blacklist(&[]);
        let bl = BuilderBlacklist::load(f.path()).unwrap();
        assert!(!bl.contains("0xanything"));
        assert!(bl.entries().is_empty());
    }

    #[test]
    fn hot_reload_picks_up_new_entries() {
        let mut f  = write_blacklist(&[("0xaaa", "first")]);
        let bl     = BuilderBlacklist::load(f.path()).unwrap();
        assert!( bl.contains("0xaaa"));
        assert!(!bl.contains("0xbbb"));

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
        assert!( bl.contains("0xbbb"), "new key must appear after reload");
    }

    #[test]
    fn add_and_remove_runtime() {
        let f  = write_blacklist(&[("0xaaa", "seed")]);
        let bl = BuilderBlacklist::load(f.path()).unwrap();

        bl.add_entry(BlacklistedBuilderEntry {
            key:    "0xbbb".into(),
            reason: "runtime add".into(),
            added:  "2026-05-01".into(),
        })
        .unwrap();
        assert!(bl.contains("0xbbb"));

        let removed = bl.remove_entry("0xaaa").unwrap();
        assert!(removed);
        assert!(!bl.contains("0xaaa"));

        let removed2 = bl.remove_entry("0xaaa").unwrap();
        assert!(!removed2);
    }
}