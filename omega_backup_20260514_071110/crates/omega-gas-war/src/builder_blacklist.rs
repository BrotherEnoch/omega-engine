ï»¿// crates/omega-gas-war/src/builder_blacklist.rs
//
// MEV-Boost builder blacklist (spec Â§12.3, Â§V1).
//
// ## Scope
//
//   Phase 4 (MEV-OFA) AND Phase 3 L1 liquidations ONLY.
//   NOT applicable on Arbitrum â€” the Arbitrum sequencer receives
//   bundles directly with no MEV-Boost routing.
//   NOT applicable on Base â€” same reason.
//
// ## Governance
//
//   Storage: `config/builder_blacklist.toml`
//   Additions: L2 fast-approve (POST /api/v1/builders/blacklist/update)
//   Removals:  L3 (48h timelock) â€” more conservative than additions
//   Quarterly review by governance committee.
//
// ## Hot-reload
//
//   `BuilderBlacklist` wraps its inner set in `arc_swap::ArcSwap` so
//   the relay submission path reads without acquiring any lock.  Hot-
//   reload atomically swaps the inner `Arc<BlacklistInner>` â€” readers
//   already holding a reference to the old set complete safely; new
//   reads see the updated set immediately.
//
// ## TOML format
//
//   ```toml
//   # config/builder_blacklist.toml
//   # One entry per known front-running builder.
//   # Keys are builder public keys (hex-encoded) or coinbase addresses.
//   # Source: public MEV-Boost analytics.  Review quarterly.
//
//   keys = [
//     "0xdeadbeef...",
//     "0xcafe1234...",
//   ]
//   ```
//
// ## Bundle routing
//
//   At submission time, filter_relays_for_bundle() (see below) returns
//   only relays that cannot route to any blacklisted builder.  If a
//   relay cannot guarantee builder exclusion, the bundle is not sent to
//   that relay for this submission.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::Deserialize;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// TOML schema
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Deserialised content of `config/builder_blacklist.toml`.
#[derive(Debug, Deserialize)]
struct BlacklistFile {
    /// Builder public keys or coinbase addresses to block.
    /// Entries are stored as lowercase strings for case-insensitive matching.
    #[serde(default)]
    keys: Vec<String>,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// BlacklistInner
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Immutable snapshot of the blacklist, shared via Arc.
#[derive(Debug)]
struct BlacklistInner {
    /// Lowercase-normalised set for O(1) lookup.
    keys: HashSet<String>,
}

impl BlacklistInner {
    fn from_file_data(data: BlacklistFile) -> Self {
        let keys = data
            .keys
            .into_iter()
            .map(|k| k.to_ascii_lowercase())
            .collect();
        Self { keys }
    }

    fn contains(&self, key: &str) -> bool {
        self.keys.contains(&key.to_ascii_lowercase())
    }

    fn len(&self) -> usize {
        self.keys.len()
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// BuilderBlacklist
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Hot-reloadable MEV-Boost builder blacklist (spec Â§12.3).
///
/// Safe to share across threads via `Arc<BuilderBlacklist>`.
/// The relay submission path calls `contains()` without any locking.
#[derive(Debug)]
pub struct BuilderBlacklist {
    inner: ArcSwap<BlacklistInner>,
    path:  PathBuf,
}

impl BuilderBlacklist {
    /// Load the blacklist from `path`.
    ///
    /// Returns an empty blacklist (rather than an error) when the file
    /// does not exist â€” this is valid for a fresh deployment before the
    /// governance committee has populated the list.  Logs a WARN so the
    /// absence is visible.
    pub fn load(path: &Path) -> anyhow::Result<Arc<Self>> {
        let inner = Self::read_file(path)?;
        tracing::info!(
            path       = %path.display(),
            entry_count = inner.len(),
            "Builder blacklist loaded",
        );
        Ok(Arc::new(Self {
            inner: ArcSwap::from_pointee(inner),
            path:  path.to_owned(),
        }))
    }

    /// Reload the blacklist from the original path without restarting.
    ///
    /// Called by the control-plane on receipt of a POST
    /// /api/v1/builders/blacklist/update (L2 fast-approve).  The swap
    /// is atomic â€” in-flight `contains()` calls on the old inner set
    /// complete safely.
    pub fn reload(&self) -> anyhow::Result<()> {
        let new_inner = Self::read_file(&self.path)?;
        let count     = new_inner.len();
        self.inner.store(Arc::new(new_inner));
        tracing::info!(
            path        = %self.path.display(),
            entry_count = count,
            "Builder blacklist hot-reloaded",
        );
        Ok(())
    }

    /// Returns `true` if `key` is on the blacklist.
    ///
    /// `key` may be a builder public key or coinbase address.
    /// Comparison is case-insensitive.
    ///
    /// This is a lock-free read â€” safe to call on every bundle
    /// submission without performance impact.
    #[inline]
    pub fn contains(&self, key: &str) -> bool {
        self.inner.load().contains(key)
    }

    /// Number of entries in the current blacklist snapshot.
    pub fn len(&self) -> usize {
        self.inner.load().len()
    }

    /// Returns `true` when the blacklist is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.load().len() == 0
    }

    /// Path to the TOML file this blacklist was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    // â”€â”€ Private helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    fn read_file(path: &Path) -> anyhow::Result<BlacklistInner> {
        if !path.exists() {
            tracing::warn!(
                path = %path.display(),
                "Builder blacklist file not found â€” starting with empty list",
            );
            return Ok(BlacklistInner { keys: HashSet::new() });
        }

        let contents = std::fs::read_to_string(path)?;
        let data: BlacklistFile = toml::from_str(&contents)
            .map_err(|e| anyhow::anyhow!("Failed to parse builder_blacklist.toml: {e}"))?;

        Ok(BlacklistInner::from_file_data(data))
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Relay filtering
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// A minimal relay descriptor for blacklist filtering.
///
/// In production, omega-relay provides a richer type; this trait allows
/// omega-gas-war to filter without depending on omega-relay.
pub trait RelayDescriptor {
    /// Builder keys or coinbase addresses associated with this relay.
    fn associated_builder_keys(&self) -> Vec<String>;
    /// Relay identifier.
    fn name(&self) -> &str;
}

/// Filter relays for a bundle, excluding those that cannot guarantee
/// exclusion of blacklisted builders (spec Â§12.3).
///
/// Returns only the relays that have no associated blacklisted builder.
/// Logs an INFO event for each relay excluded.
///
/// ## Arguments
///
/// - `relays`: all candidate relays for this bundle.
/// - `blacklist`: the current `BuilderBlacklist`.
///
/// ## Panics
///
/// Never panics.
pub fn filter_relays_for_bundle<'a, R: RelayDescriptor>(
    relays:     &'a [R],
    blacklist:  &BuilderBlacklist,
) -> Vec<&'a R> {
    // Fast path: empty blacklist means all relays are permitted.
    if blacklist.is_empty() {
        return relays.iter().collect();
    }

    relays
        .iter()
        .filter(|relay| {
            let blocked = relay
                .associated_builder_keys()
                .iter()
                .any(|k| blacklist.contains(k));

            if blocked {
                tracing::info!(
                    relay = relay.name(),
                    "Relay excluded: associated with blacklisted builder (Â§12.3)",
                );
            }
            !blocked
        })
        .collect()
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_toml(dir: &std::path::Path, keys: &[&str]) -> PathBuf {
        let path = dir.join("builder_blacklist.toml");
        let list = keys
            .iter()
            .map(|k| format!("\"{k}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let content = format!("keys = [{list}]\n");
        std::fs::write(&path, content).unwrap();
        path
    }

    // â”€â”€ load â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn load_from_toml() {
        let dir  = tempfile::tempdir().unwrap();
        let path = write_toml(dir.path(), &["0xABCD", "0x1234"]);
        let bl   = BuilderBlacklist::load(&path).unwrap();
        assert_eq!(bl.len(), 2);
        assert!(bl.contains("0xABCD"));
        assert!(bl.contains("0xabcd")); // case-insensitive
        assert!(!bl.contains("0x9999"));
    }

    #[test]
    fn missing_file_gives_empty_blacklist() {
        let dir  = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let bl   = BuilderBlacklist::load(&path).unwrap();
        assert!(bl.is_empty());
    }

    // â”€â”€ reload â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn hot_reload_updates_list() {
        let dir  = tempfile::tempdir().unwrap();
        let path = write_toml(dir.path(), &["0xAAAA"]);
        let bl   = BuilderBlacklist::load(&path).unwrap();
        assert!(bl.contains("0xAAAA"));
        assert!(!bl.contains("0xBBBB"));

        // Update the file
        write_toml(dir.path(), &["0xBBBB"]);
        bl.reload().unwrap();

        assert!(!bl.contains("0xAAAA"), "old key must be gone after reload");
        assert!(bl.contains("0xBBBB"),  "new key must be visible after reload");
    }

    // â”€â”€ filter_relays_for_bundle â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    struct MockRelay {
        name:    &'static str,
        builders: Vec<String>,
    }

    impl RelayDescriptor for MockRelay {
        fn associated_builder_keys(&self) -> Vec<String> { self.builders.clone() }
        fn name(&self) -> &str { self.name }
    }

    fn relays() -> Vec<MockRelay> {
        vec![
            MockRelay { name: "relay_clean", builders: vec!["0xGOOD".into()] },
            MockRelay { name: "relay_bad",   builders: vec!["0xBAD".into()] },
            MockRelay { name: "relay_mixed", builders: vec!["0xOK".into(), "0xBAD".into()] },
        ]
    }

    #[test]
    fn filter_excludes_blacklisted_relay() {
        let dir  = tempfile::tempdir().unwrap();
        let path = write_toml(dir.path(), &["0xBAD"]);
        let bl   = BuilderBlacklist::load(&path).unwrap();
        let rs   = relays();

        let allowed = filter_relays_for_bundle(&rs, &bl);
        assert_eq!(allowed.len(), 1);
        assert_eq!(allowed[0].name(), "relay_clean");
    }

    #[test]
    fn empty_blacklist_allows_all_relays() {
        let dir  = tempfile::tempdir().unwrap();
        let path = dir.path().join("none.toml"); // does not exist
        let bl   = BuilderBlacklist::load(&path).unwrap();
        let rs   = relays();

        let allowed = filter_relays_for_bundle(&rs, &bl);
        assert_eq!(allowed.len(), 3, "empty blacklist must allow all relays");
    }

    #[test]
    fn case_insensitive_filter() {
        let dir  = tempfile::tempdir().unwrap();
        let path = write_toml(dir.path(), &["0xbad"]); // lowercase in file
        let bl   = BuilderBlacklist::load(&path).unwrap();

        // relay_bad's key is "0xBAD" â€” must still match
        let rs      = relays();
        let allowed = filter_relays_for_bundle(&rs, &bl);
        assert_eq!(allowed.len(), 1);
        assert_eq!(allowed[0].name(), "relay_clean");
    }
}