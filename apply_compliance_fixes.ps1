# apply_compliance_fixes.ps1
# Run from C:\Users\silve\Documents\omega-engine
# Writes the four corrected files: omega-rpc/net.rs, omega-core/config.rs, omega-compliance/ofa.rs, omega-compliance/policy.rs
$ErrorActionPreference = 'Stop'

Write-Host 'Writing crates\omega-rpc\src\net.rs...'
$content_0 = @'
// crates/omega-rpc/src/net.rs
//     gives an operator no signal that the problem needs their
//     attention, not more patience.
//   - verify_chain_id: nothing anywhere in this crate previously checked
//     that the connected endpoint's actual chain matched what was
//     configured. A misrouted or misconfigured endpoint would silently
//     mislabel every downstream signal with the wrong chain_id.
//   - redact_ws_url: most RPC providers embed an API key directly in
//     the WS URL (path or query string). Logging the raw URL — as the
//     previous code did at info/warn level — leaks that key into log
//     aggregation.
//   - wei_to_gwei_saturating: a bare `as u64` cast after wei→gwei
//     division silently wraps an absurd/malformed value from the RPC
//     endpoint into a small, WRONG number — dangerous specifically
//     because it's wrong in the unsafe direction (an artificially low
//     gas cost estimate makes a trade look more profitable than it is).
//   - validate_ws_scheme: fail immediately and clearly on an obviously
//     wrong URL (e.g. a copy-paste of an http:// endpoint) rather than
//     relying on whatever downstream error eventually surfaces.
//
// ## Fix (this revision): clippy::bool_comparison in
// validate_ws_scheme_rejects_http
//
// `assert!(!err.is_fatal() == false || err.is_fatal());` was leftover
// edit debris — a tautology (`!x == false` is just `x`, so this reduces
// to `assert!(err.is_fatal() || err.is_fatal())`) that clippy's
// `bool_comparison` lint correctly flags as a no-op comparison against a
// literal `false`. The very next line already asserts the real
// invariant (`assert!(err.is_fatal())`), so the redundant line is
// removed rather than rewritten — nothing of value was being checked
// that isn't already covered.

use alloy::providers::Provider;

// ─────────────────────────────────────────────────────────────────────────────
// RpcClientError
// ─────────────────────────────────────────────────────────────────────────────

/// Errors from establishing or verifying an RPC connection.
///
/// The `is_fatal()` distinction is the whole point of this type:
/// `connect_with_retry` (client.rs) and the subscription reconnect loops
/// (subscriptions.rs, client.rs) use it to stop retrying immediately on
/// a configuration error rather than looping forever against something
/// that can never succeed.
#[derive(Debug, thiserror::Error)]
pub enum RpcClientError {
    /// The configured URL is structurally wrong (e.g. missing/incorrect
    /// scheme). Retrying with the same URL can never succeed.
    #[error("invalid RPC URL {url}: {reason}")]
    InvalidUrl { url: String, reason: String },

    /// The connected endpoint reports a different chain than configured.
    /// Retrying with the same URL can never succeed — the endpoint
    /// itself is pointed at the wrong network.
    #[error("chain ID mismatch: configured {expected}, endpoint reports {actual}")]
    ChainIdMismatch { expected: u64, actual: u64 },

    /// Connection or a required initial RPC call failed for reasons
    /// that may well be transient (network blip, node restart, DNS
    /// hiccup) — safe to retry.
    #[error("connection failed: {0}")]
    ConnectFailed(String),
}

impl RpcClientError {
    /// True for errors where retrying with the SAME configuration can
    /// never succeed — these are configuration/misuse errors, not
    /// transient network conditions.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            RpcClientError::InvalidUrl { .. } | RpcClientError::ChainIdMismatch { .. }
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// URL validation and redaction
// ─────────────────────────────────────────────────────────────────────────────

/// Rejects a `ws_url` that doesn't start with `ws://` or `wss://` before
/// any connection attempt is made. `ProviderBuilder::on_builtin` performs
/// its own scheme-based transport detection, but relying on whatever
/// error eventually surfaces downstream (which may be an unrelated-
/// looking transport error) is worse than failing immediately with a
/// clear, specific message at the point of misconfiguration.
pub(crate) fn validate_ws_scheme(ws_url: &str) -> Result<(), RpcClientError> {
    if ws_url.starts_with("ws://") || ws_url.starts_with("wss://") {
        Ok(())
    } else {
        Err(RpcClientError::InvalidUrl {
            url: redact_ws_url(ws_url),
            reason: "must start with ws:// or wss://".to_string(),
        })
    }
}

/// Redacts everything after the host in a URL — scheme and host are
/// kept, path/query/userinfo are dropped. Most RPC providers embed an
/// API key directly in the path or query string (e.g.
/// `wss://host/v2/<API_KEY>`); logging the raw URL at info/warn level
/// (as this crate previously did) leaks that key into log aggregation
/// and anywhere those logs are shipped.
///
/// Best-effort string parsing rather than a full URL-parsing dependency
/// — this crate's dependency list is deliberately minimal, and the
/// redaction only needs to be conservative (better to over-redact than
/// under-redact), not RFC-3986-perfect.
pub(crate) fn redact_ws_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return "<redacted>".to_string();
    };
    let scheme = &url[..scheme_end];
    let rest = &url[scheme_end + 3..];
    let host_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let host_with_possible_userinfo = &rest[..host_end];
    // Strip "user:pass@" userinfo from the host portion, if present.
    let host = host_with_possible_userinfo
        .rsplit('@')
        .next()
        .unwrap_or(host_with_possible_userinfo);
    format!("{scheme}://{host}/<redacted>")
}

// ─────────────────────────────────────────────────────────────────────────────
// Chain ID verification
// ─────────────────────────────────────────────────────────────────────────────

/// Verifies the connected endpoint reports the expected chain ID via
/// `eth_chainId`. This is the single check that closes the biggest gap
/// found in this audit: nothing anywhere previously verified that a
/// connected RPC endpoint actually serves the chain the engine believes
/// it does. A misconfigured or misrouted endpoint would otherwise
/// silently mislabel every block, log, and fee snapshot with the wrong
/// `chain_id` from that point forward.
pub(crate) async fn verify_chain_id(
    provider: &dyn Provider,
    expected: u64,
) -> Result<(), RpcClientError> {
    let actual = provider
        .get_chain_id()
        .await
        .map_err(|e| RpcClientError::ConnectFailed(format!("eth_chainId failed: {e}")))?;
    if actual != expected {
        return Err(RpcClientError::ChainIdMismatch { expected, actual });
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Numeric conversions
// ─────────────────────────────────────────────────────────────────────────────

/// Converts a wei amount to gwei, saturating rather than silently
/// wrapping on overflow.
///
/// A bare `(wei / 1_000_000_000) as u64` cast — the previous pattern in
/// this crate — truncates any value whose gwei-denominated form exceeds
/// `u64::MAX` down to a small, WRONG number via integer wraparound. That
/// is the dangerous direction for a value that feeds directly into gas
/// cost / profitability calculations: an artificially LOW gas cost
/// estimate (from a malformed or extreme RPC-reported base fee wrapping
/// to something small) makes a trade look more profitable than it
/// actually is. Saturating at `u64::MAX` instead makes an absurd input
/// read as absurdly (and correctly, safely) HIGH, which a downstream
/// profitability check will correctly reject rather than silently
/// accept.
///
/// Takes `u128` so it's correct regardless of whether the caller's
/// underlying field type is `u64` or `u128` — callers widen with `as
/// u128`, which is always a safe, lossless cast in that direction.
pub(crate) fn wei_to_gwei_saturating(wei: u128) -> u64 {
    (wei / 1_000_000_000).min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_ws_url_strips_path_and_query() {
        assert_eq!(
            redact_ws_url("wss://arb-mainnet.g.alchemy.com/v2/SUPER_SECRET_KEY"),
            "wss://arb-mainnet.g.alchemy.com/<redacted>"
        );
        assert_eq!(
            redact_ws_url("wss://node.example.com/ws?apikey=SECRET"),
            "wss://node.example.com/<redacted>"
        );
    }

    #[test]
    fn redact_ws_url_strips_userinfo() {
        assert_eq!(
            redact_ws_url("wss://user:hunter2@node.example.com/v2/key"),
            "wss://node.example.com/<redacted>"
        );
    }

    #[test]
    fn redact_ws_url_handles_bare_host() {
        assert_eq!(
            redact_ws_url("wss://node.example.com"),
            "wss://node.example.com/<redacted>"
        );
    }

    #[test]
    fn redact_ws_url_handles_garbage_input() {
        assert_eq!(redact_ws_url("not a url at all"), "<redacted>");
    }

    #[test]
    fn validate_ws_scheme_accepts_ws_and_wss() {
        assert!(validate_ws_scheme("ws://localhost:8545").is_ok());
        assert!(validate_ws_scheme("wss://node.example.com/v2/key").is_ok());
    }

    #[test]
    fn validate_ws_scheme_rejects_http() {
        let err = validate_ws_scheme("https://node.example.com").unwrap_err();
        assert!(matches!(err, RpcClientError::InvalidUrl { .. }));
        assert!(
            err.is_fatal(),
            "an InvalidUrl error must be fatal — retrying the same misconfigured URL can never succeed"
        );
    }

    #[test]
    fn wei_to_gwei_saturating_normal_value() {
        assert_eq!(wei_to_gwei_saturating(50_000_000_000), 50); // 50 gwei
    }

    #[test]
    fn wei_to_gwei_saturating_zero() {
        assert_eq!(wei_to_gwei_saturating(0), 0);
    }

    #[test]
    fn wei_to_gwei_saturating_saturates_on_overflow() {
        // A wei value whose gwei form exceeds u64::MAX must saturate,
        // not wrap around to a small, wrong number.
        let absurd_wei = (u128::from(u64::MAX) + 1) * 1_000_000_000;
        assert_eq!(wei_to_gwei_saturating(absurd_wei), u64::MAX);
    }

    #[test]
    fn chain_id_mismatch_is_fatal() {
        let err = RpcClientError::ChainIdMismatch {
            expected: 42161,
            actual: 1,
        };
        assert!(err.is_fatal());
    }

    #[test]
    fn connect_failed_is_not_fatal() {
        let err = RpcClientError::ConnectFailed("timeout".to_string());
        assert!(!err.is_fatal());
    }
}

'@
Set-Content -Path 'crates\omega-rpc\src\net.rs' -Value $content_0 -Encoding UTF8 -NoNewline

Write-Host 'Writing crates\omega-core\src\config.rs...'
$content_1 = @'
// crates/omega-core/src/config.rs
//
// OmegaConfig — runtime configuration for the Omega Engine.
//
// ## Governance tiers (§5)
//
// Every field is annotated with the governance tier required to change it
// in production.  The tiers and their properties are:
//
//   L1 (Operator)    Hot-reload via POST /api/v1/config.  No timelock.
//                    Risk: low.  Scope: operational knobs (log levels,
//                    metrics endpoints, relay timeouts).
//
//   L2 (Fast-Approve) Signed by ≥2/5 governance keys.  Effective
//                    immediately after signature validation.
//                    Risk: medium.  Scope: strategy parameters, fee
//                    ceilings, ML learning rate.
//
//   L3 (Timelock)   48-hour timelock + 3/5 multisig.  Risk: high.
//                   Scope: phase gates, Vault parameters, DAO fee bps.
//                   Emergency L3 path: 24h + 3/5, qualifying criteria
//                   only (§5.1).
//
// Fields that are IMMUTABLE after deployment (chain_id, contract
// addresses that are Certora-verified invariants) are marked IMMUTABLE.
// They can only change via a full redeployment.
//
// ## Serialisation contract
//
// OmegaConfig is loaded from a TOML file at startup and can be
// hot-reloaded via the control-plane API (§17).  All defaults are set
// via serde's `default` attribute so that a minimal config file works
// in development.  Production deployments must provide all L3 fields
// explicitly — missing L3 fields cause a `OmegaError::Config` halt.
//
// `#[serde(deny_unknown_fields)]` is applied at every level, including
// the top-level `OmegaConfig` itself (previously missing here — an
// unrecognised top-level TOML key would have been silently ignored
// rather than rejected, the one place in this file that didn't match
// its own stated strictness policy).
//
// ## WeiAmount — u128 amounts that survive TOML round-trips
//
// The `toml` crate (v0.5/v0.8) represents all TOML integers as `i64`
// internally and rejects values that don't fit — `i64::MAX` ≈
// 9.223 × 10^18, which is only ~9.223 ETH in wei. Neither a plain `u64`
// nor a plain `u128` Rust field changes this: the TOML *parser* rejects
// the literal before serde ever sees it, regardless of what Rust type
// is on the receiving end. `WeiAmount` (defined below) sidesteps this
// by serializing as a decimal STRING in TOML (TOML strings have no
// magnitude limit) while storing the value as `u128` internally.
//
// This replaces a previous `u64`-typed workaround whose defaults were
// both hardcoded to ~9 ETH — not merely an approximation of the spec's
// 50 ETH / 500 ETH values, but numerically IDENTICAL to each other,
// which collapsed the per-transfer and daily caps into one limit in
// practice (a single transfer could already exhaust the entire "daily"
// budget). `WeiAmount` restores the actual spec values and `validate()`
// now enforces `per_transfer_cap_wei <= daily_cap_wei` explicitly rather
// than relying on the defaults happening to make sense.
//
// ## Fix (this revision): dead_code on tests::Wrapper.amount
//
// `cargo clippy -- -D warnings` promotes `dead_code` (a rustc lint, not
// even a clippy one) to a hard error. Of the two local
// `struct Wrapper { amount: WeiAmount }` test fixtures near the bottom
// of this file, `wei_amount_deserializes_from_plain_integer_too` reads
// `parsed.amount` and was never flagged. `wei_amount_rejects_garbage_string`
// only asserts `result.is_err()` — since deserialization always fails in
// that test, `Wrapper` is never successfully constructed, so `.amount`
// is genuinely, permanently unread there: the field's TYPE (driving
// `WeiAmount`'s custom `Deserialize` impl) is what's under test, not any
// value read from it. Fixed with a scoped `#[allow(dead_code)]` on that
// one local struct rather than changing the test's behavior — reading
// `.amount` after confirming `is_err()` isn't possible (there's no `Ok`
// value to read it from), so there's no way to make the field
// "genuinely used" without testing something this test isn't about.
//
// Spec references:
//   §1.1  — phase gates → active_phase
//   §5    — governance tiers
//   §5.1  — emergency L3 criteria
//   §7    — dual-component gas model → GasConfig
//   §11.1 — LA tier thresholds → LaConfig
//   §11.2 — cascade backpressure → RelayConfig
//   §12.1 — emergency bundle profit check → GasConfig
//   §12.2 — 500 gwei priority fee ceiling → GasConfig
//   §13   — ML online learner → MlConfig
//   §14   — address rotation → RotationConfig
//   §15   — Vault parameters → VaultConfig
//   §15.2 — per-transfer / daily caps → VaultConfig::{per_transfer_cap_wei, daily_cap_wei}
//   §17.1 — WebSocket rate limits → ApiConfig
//   §18   — CHI/GST gas tokens (Phase 4+ L1 only) → GasConfig

use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// WeiAmount
// ─────────────────────────────────────────────────────────────────────────────

/// Wei-denominated amount that can express values larger than
/// `i64::MAX` in TOML config files. See the module doc comment above
/// for why this exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WeiAmount(u128);

impl WeiAmount {
    pub const ZERO: WeiAmount = WeiAmount(0);

    #[inline]
    pub const fn from_wei(wei: u128) -> Self {
        WeiAmount(wei)
    }

    #[inline]
    pub const fn as_wei(self) -> u128 {
        self.0
    }

    /// Construct from a whole-ETH amount, converting to wei internally.
    /// Panics on overflow — unreachable with any realistic config value;
    /// even 1 million ETH fits comfortably under `u128::MAX`.
    #[inline]
    pub fn from_eth(eth: u64) -> Self {
        WeiAmount(
            (eth as u128)
                .checked_mul(1_000_000_000_000_000_000)
                .expect("from_eth overflow: value exceeds representable wei range"),
        )
    }
}

impl std::fmt::Display for WeiAmount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for WeiAmount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Always serialize as a decimal string. This is what makes the
        // type TOML-safe (see module doc comment) and is also
        // unambiguous in JSON, which silently loses precision for
        // integers beyond 2^53 in many JSON consumers (e.g.
        // JavaScript's Number type) — a string sidesteps that too.
        serializer.serialize_str(&self.0.to_string())
    }
}

/// Accepts either a decimal string (the canonical wire format — see
/// `Serialize` above) or a plain integer (so config constructed
/// programmatically, or via a JSON source with a small-enough value,
/// still deserializes without requiring the string form).
#[derive(Deserialize)]
#[serde(untagged)]
enum WeiAmountRepr {
    String(String),
    Number(u64),
}

impl<'de> Deserialize<'de> for WeiAmount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match WeiAmountRepr::deserialize(deserializer)? {
            WeiAmountRepr::String(s) => s
                .trim()
                .parse::<u128>()
                .map(WeiAmount)
                .map_err(|e| serde::de::Error::custom(format!("invalid wei amount {s:?}: {e}"))),
            WeiAmountRepr::Number(n) => Ok(WeiAmount(n as u128)),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Top-level config
// ─────────────────────────────────────────────────────────────────────────────

/// Full runtime configuration for the Omega Engine.
///
/// Loaded from `config/omega.toml` at startup.  Hot-reloaded via
/// `POST /api/v1/config` (L1 fields only; L2/L3 fields require the
/// appropriate governance signature).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OmegaConfig {
    /// Currently active system phase (§1.1, §20).
    ///
    /// GOVERNANCE: L3 (48h timelock).  Phase activation is a
    /// high-stakes irreversible operation — once Phase 3 is active,
    /// Phase 2 strategies continue running alongside it.
    ///
    /// Valid values: 0 (Shadow/Backtest), 1 (SA), 2 (MSA), 3 (LA), 4 (MEV).
    #[serde(default = "defaults::active_phase")]
    pub active_phase: u8,

    /// Gas model configuration (§7, §12.1, §12.2).
    #[serde(default)]
    pub gas: GasConfig,

    /// LA-specific configuration (§11).
    #[serde(default)]
    pub la: LaConfig,

    /// Relay submission configuration (§11.2, §12).
    #[serde(default)]
    pub relay: RelayConfig,

    /// ML online learner configuration (§13).
    #[serde(default)]
    pub ml: MlConfig,

    /// Address rotation configuration (§14).
    #[serde(default)]
    pub rotation: RotationConfig,

    /// Vault and PIL treasury configuration (§15).
    #[serde(default)]
    pub vault: VaultConfig,

    /// Control-plane API configuration (§17).
    #[serde(default)]
    pub api: ApiConfig,
}

impl Default for OmegaConfig {
    fn default() -> Self {
        Self {
            active_phase: defaults::active_phase(),
            gas: GasConfig::default(),
            la: LaConfig::default(),
            relay: RelayConfig::default(),
            ml: MlConfig::default(),
            rotation: RotationConfig::default(),
            vault: VaultConfig::default(),
            api: ApiConfig::default(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GasConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Arbitrum dual-component gas model parameters (§7, §12.1, §12.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GasConfig {
    /// L2 execution gas estimate buffer factor.
    ///
    /// Applied as: `l2_gas_budget = l2_exec_estimate × l2_buffer_factor`.
    /// Default 1.15 = 15% headroom.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::l2_buffer_factor")]
    pub l2_buffer_factor: f64,

    /// L1 data gas estimate buffer factor.
    ///
    /// Applied to the calldata-bytes × 16 estimate.  Default 1.10 = 10%.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::l1_data_buffer_factor")]
    pub l1_data_buffer_factor: f64,

    /// Maximum priority fee submitted to the Arbitrum sequencer, in gwei.
    ///
    /// Spec §12.2: 500 gwei ceiling.  At Arbitrum's 250ms block time
    /// this is ~0.0105 ETH per block — comparable to 50 gwei on L1.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::max_priority_fee_gwei")]
    pub max_priority_fee_gwei: u64,

    /// Conservative bundle fee as a fraction of the cap (0.0–1.0).
    ///
    /// Spec §12: conservative_fee = cap × conservative_fee_fraction.
    /// Default 0.70 = 70% of cap.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::conservative_fee_fraction")]
    pub conservative_fee_fraction: f64,

    /// Whether to enable emergency bundle emission (§12.1).
    ///
    /// When true, a third bundle at 2× cap is emitted IFF
    /// `expected_profit_net > emergency_gas_cost + dynamic_min_profit`.
    /// The profit check is MANDATORY — bundles are never submitted at a
    /// loss (fix M2).
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::emergency_bundle_enabled")]
    pub emergency_bundle_enabled: bool,

    /// Whether to evaluate CHI/GST gas token redemption (§18).
    ///
    /// Applicable on Ethereum L1 (Phase 4+) only.  Must be `false` on
    /// Arbitrum and Base — EVM storage refunds do not exist there.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::gas_token_enabled")]
    pub gas_token_enabled: bool,

    /// Minimum L1 base fee in gwei above which CHI/GST redemption is
    /// evaluated (§18).  Spec recommendation: 80 gwei.
    ///
    /// Ignored when `gas_token_enabled` is false.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::gas_token_min_base_fee_gwei")]
    pub gas_token_min_base_fee_gwei: u64,
}

impl Default for GasConfig {
    fn default() -> Self {
        Self {
            l2_buffer_factor: defaults::l2_buffer_factor(),
            l1_data_buffer_factor: defaults::l1_data_buffer_factor(),
            max_priority_fee_gwei: defaults::max_priority_fee_gwei(),
            conservative_fee_fraction: defaults::conservative_fee_fraction(),
            emergency_bundle_enabled: defaults::emergency_bundle_enabled(),
            gas_token_enabled: defaults::gas_token_enabled(),
            gas_token_min_base_fee_gwei: defaults::gas_token_min_base_fee_gwei(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LaConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Liquidation Arbitrage configuration (§11).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaConfig {
    /// Maximum number of positions in the hot-tier index.
    ///
    /// Spec §11.1: ~2,000–5,000 on Arbitrum in normal markets.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_hot_tier_max_positions")]
    pub hot_tier_max_positions: usize,

    /// Maximum number of positions in the warm-tier index.
    ///
    /// Spec §11.1: ~15,000–30,000 on Arbitrum.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_warm_tier_max_positions")]
    pub warm_tier_max_positions: usize,

    /// Maximum number of positions in the cold-tier index.
    ///
    /// Spec §11.1: ~100,000–200,000.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_cold_tier_max_positions")]
    pub cold_tier_max_positions: usize,

    /// Total position index capacity (all tiers combined).
    ///
    /// Spec §11.1: ~500,000 total.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_total_position_capacity")]
    pub total_position_capacity: usize,

    /// Warm-tier oracle price move threshold that triggers immediate
    /// recompute (§11.1).  In basis points.  Default 50 = 0.5%.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::la_warm_price_move_bps")]
    pub warm_price_move_threshold_bps: u16,

    /// Warm-tier batch recompute interval in milliseconds (§11.1).
    /// Default 200ms.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::la_warm_batch_interval_ms")]
    pub warm_batch_interval_ms: u64,

    /// Cold-tier lazy recompute interval in milliseconds (§11.1).
    /// Default 2,000ms (2 seconds).
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::la_cold_recompute_interval_ms")]
    pub cold_recompute_interval_ms: u64,

    /// Archived-tier cycle length in blocks (§11.1).  Default 500 blocks.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_archived_cycle_blocks")]
    pub archived_cycle_blocks: u64,

    /// Sequencer restart deduplication window in blocks (§11.3).
    /// Default 60 blocks (~15s on Arbitrum).
    ///
    /// NOTE: this is intentionally kept in config (not only in ChainId)
    /// so that it can be tuned per-deployment without a code change.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::la_sequencer_restart_window_blocks")]
    pub sequencer_restart_window_blocks: u64,
}

impl Default for LaConfig {
    fn default() -> Self {
        Self {
            hot_tier_max_positions: defaults::la_hot_tier_max_positions(),
            warm_tier_max_positions: defaults::la_warm_tier_max_positions(),
            cold_tier_max_positions: defaults::la_cold_tier_max_positions(),
            total_position_capacity: defaults::la_total_position_capacity(),
            warm_price_move_threshold_bps: defaults::la_warm_price_move_bps(),
            warm_batch_interval_ms: defaults::la_warm_batch_interval_ms(),
            cold_recompute_interval_ms: defaults::la_cold_recompute_interval_ms(),
            archived_cycle_blocks: defaults::la_archived_cycle_blocks(),
            sequencer_restart_window_blocks: defaults::la_sequencer_restart_window_blocks(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RelayConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Relay submission configuration (§11.2, §12).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    /// Maximum bundles submitted per relay per second (§11.2, fix C2).
    /// Default 4.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::relay_max_per_second")]
    pub max_bundles_per_relay_per_second: usize,

    /// Stagger delay between sequential bundle submissions in
    /// cascade mode, in milliseconds (§11.2).  Default 10ms.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::relay_stagger_ms")]
    pub cascade_stagger_ms: u64,

    /// Maximum number of relays in the cascade submission set (§11.2).
    /// Default 4.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::relay_cascade_max")]
    pub cascade_max_relays: usize,

    /// Tie-band width for LA-inclusion-rate ranking (§11.2).
    ///
    /// Relays within this fraction of the best inclusion rate are
    /// eligible for the randomised round-robin (anti-fingerprinting).
    /// Default 0.05 = 5%.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::relay_tie_band_fraction")]
    pub inclusion_rate_tie_band_fraction: f64,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            max_bundles_per_relay_per_second: defaults::relay_max_per_second(),
            cascade_stagger_ms: defaults::relay_stagger_ms(),
            cascade_max_relays: defaults::relay_cascade_max(),
            inclusion_rate_tie_band_fraction: defaults::relay_tie_band_fraction(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// MlConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Gas model ML online learner configuration (§13).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlConfig {
    /// Learning rate for the online fee multiplier updates (§13).
    /// Default 0.01.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_learning_rate")]
    pub learning_rate: f64,

    /// Fraction of loss events held out for validation (§13.1, fix C1).
    /// Default 0.20 = 20%.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_validation_ratio")]
    pub validation_ratio: f64,

    /// Number of loss events between validation passes and checkpoint
    /// saves (§13.1).  Default 1,000.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_checkpoint_interval")]
    pub checkpoint_interval: u64,

    /// Maximum win-rate degradation below the last checkpoint before
    /// automatic model revert (§13.1, fix C1).
    ///
    /// Default 0.05 = 5 percentage points.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_revert_threshold")]
    pub revert_threshold: f64,

    /// Upper bound on fee multiplier (§13.3, fix I5).  Default 5.0.
    ///
    /// GOVERNANCE: L3 (48h timelock) — ceiling changes affect maximum
    /// gas spend; requires careful analysis.
    #[serde(default = "defaults::ml_multiplier_ceiling")]
    pub multiplier_ceiling: f64,

    /// Lower bound on fee multiplier (§13).  Default 0.3.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_multiplier_floor")]
    pub multiplier_floor: f64,

    /// Consecutive LOST_GAS_LOW events at the ceiling before triggering
    /// DEGRADED alert and model pause (§13.3, fix I5).  Default 100.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_ceiling_escalation_threshold")]
    pub ceiling_escalation_threshold: u64,

    /// Number of checkpoint files to retain on disk (§13.2, fix I1).
    /// Older files are pruned.  Default 10.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::ml_checkpoint_retention")]
    pub checkpoint_retention: usize,

    /// Directory for checkpoint files (§13.2).
    /// Default `/var/omega`.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::ml_checkpoint_dir")]
    pub checkpoint_dir: String,
}

impl Default for MlConfig {
    fn default() -> Self {
        Self {
            learning_rate: defaults::ml_learning_rate(),
            validation_ratio: defaults::ml_validation_ratio(),
            checkpoint_interval: defaults::ml_checkpoint_interval(),
            revert_threshold: defaults::ml_revert_threshold(),
            multiplier_ceiling: defaults::ml_multiplier_ceiling(),
            multiplier_floor: defaults::ml_multiplier_floor(),
            ceiling_escalation_threshold: defaults::ml_ceiling_escalation_threshold(),
            checkpoint_retention: defaults::ml_checkpoint_retention(),
            checkpoint_dir: defaults::ml_checkpoint_dir(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RotationConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Address rotation and relay reputation carryover configuration (§14).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotationConfig {
    /// Exponential decay time constant for reputation carryover, in months
    /// (§14.1, fix C4 + I4).  Default 3.
    ///
    /// The authoritative formula from the §14.1 code block is:
    ///   `carryover_pct = base_carryover × exp(-months_since_rotation / decay_rate_months)`
    ///
    /// With the default of 3 this produces:
    ///   0 months → 50.0%,  3 months → 18.4%
    ///
    /// NOTE: §14.1 refers to this as "half-life" but the spec's illustrative
    /// table values do not match a true half-life formula.  The CODE BLOCK in
    /// §14.1 is authoritative; this field is the divisor in that formula.
    /// The true half-life of the default configuration is 3 × ln(2) ≈ 2.08
    /// months, not 3 months.
    ///
    /// This value is a DIVISOR in the formula above — `validate()` now
    /// rejects a value ≤ 0.0, since zero would divide by zero (NaN) and
    /// a negative value would invert decay into growth, both silently
    /// corrupting every reputation-carryover calculation with no error
    /// raised anywhere near the actual computation.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::rotation_decay_rate_months")]
    pub reputation_decay_rate_months: f64,

    /// Base carryover fraction immediately after rotation (§14.1).
    /// Default 0.50 = 50%.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::rotation_base_carryover")]
    pub base_carryover_fraction: f64,
}

impl Default for RotationConfig {
    fn default() -> Self {
        Self {
            reputation_decay_rate_months: defaults::rotation_decay_rate_months(),
            base_carryover_fraction: defaults::rotation_base_carryover(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// VaultConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Vault and PIL treasury parameters (§15).
///
/// ## WeiAmount instead of u64
///
/// `per_transfer_cap_wei` and `daily_cap_wei` are `WeiAmount`, not a raw
/// integer — see the module doc comment for why a plain integer field
/// (of any width) can't survive a TOML round-trip at the magnitude the
/// spec actually requires (50 ETH / 500 ETH), and why the previous `u64`
/// workaround silently collapsed both caps to the same reduced value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultConfig {
    /// DAO fee in basis points (§15.1).  Default 500 = 5%.
    /// Range: 0–1,000 bps (0–10%).  Certora invariant C9 enforces
    /// the 10% ceiling on-chain.
    ///
    /// GOVERNANCE: L3 (48h timelock).
    #[serde(default = "defaults::vault_dao_fee_bps")]
    pub dao_fee_bps: u16,

    /// Required on-chain confirmation depth before Vault releases profit
    /// (§15).  Minimum 12 — enforced by the Vault contract.
    ///
    /// IMMUTABLE (Vault contract enforces this; config value must
    /// match the deployed contract or OmegaError::Config is emitted).
    #[serde(default = "defaults::vault_confirmation_depth")]
    pub confirmation_depth: u8,

    /// Maximum profit released per single Vault transfer, in ETH wei.
    /// Default: 50 ETH (§15.2), expressed as the full spec value now
    /// that `WeiAmount` removes the TOML-integer magnitude limitation.
    ///
    /// GOVERNANCE: L3 (48h timelock).
    #[serde(default = "defaults::vault_per_transfer_cap_wei")]
    pub per_transfer_cap_wei: WeiAmount,

    /// Maximum aggregate profit released per 24h rolling window, in ETH
    /// wei. Default: 500 ETH (§15.2).
    ///
    /// GOVERNANCE: L3 (48h timelock).
    #[serde(default = "defaults::vault_daily_cap_wei")]
    pub daily_cap_wei: WeiAmount,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            dao_fee_bps: defaults::vault_dao_fee_bps(),
            confirmation_depth: defaults::vault_confirmation_depth(),
            per_transfer_cap_wei: defaults::vault_per_transfer_cap_wei(),
            daily_cap_wei: defaults::vault_daily_cap_wei(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ApiConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Control-plane API configuration (§17, §17.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiConfig {
    /// WebSocket messages per minute for authenticated connections (§17.1,
    /// fix M4).  Default 300.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::api_ws_authed_rate")]
    pub ws_authenticated_msgs_per_min: u32,

    /// WebSocket messages per minute for anonymous connections (§17.1).
    /// Default 100.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::api_ws_anon_rate")]
    pub ws_anonymous_msgs_per_min: u32,

    /// TCP bind address for the control-plane HTTP server.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::api_bind_addr")]
    pub bind_addr: String,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            ws_authenticated_msgs_per_min: defaults::api_ws_authed_rate(),
            ws_anonymous_msgs_per_min: defaults::api_ws_anon_rate(),
            bind_addr: defaults::api_bind_addr(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Default value functions
//
// Each constant is a named function rather than a bare literal so that
// serde's `default = "..."` attribute can reference it and so that the
// value appears exactly once — no risk of the struct Default and the serde
// default drifting apart.
// ─────────────────────────────────────────────────────────────────────────────

mod defaults {
    use super::WeiAmount;

    // ── Top-level ─────────────────────────────────────────────────────────
    pub fn active_phase() -> u8 {
        0
    }

    // ── GasConfig ─────────────────────────────────────────────────────────
    pub fn l2_buffer_factor() -> f64 {
        1.15
    }
    pub fn l1_data_buffer_factor() -> f64 {
        1.10
    }
    /// Spec §12.2: 500 gwei ceiling.
    pub fn max_priority_fee_gwei() -> u64 {
        500
    }
    pub fn conservative_fee_fraction() -> f64 {
        0.70
    }
    pub fn emergency_bundle_enabled() -> bool {
        true
    }
    /// Spec §18: disabled by default; evaluate for Phase 4+ L1 only.
    pub fn gas_token_enabled() -> bool {
        false
    }
    /// Spec §18: revisit if L1 base fee routinely exceeds 80 gwei.
    pub fn gas_token_min_base_fee_gwei() -> u64 {
        80
    }

    // ── LaConfig ──────────────────────────────────────────────────────────
    /// Spec §11.1: ~2,000–5,000; use midpoint as default.
    pub fn la_hot_tier_max_positions() -> usize {
        5_000
    }
    /// Spec §11.1: ~15,000–30,000.
    pub fn la_warm_tier_max_positions() -> usize {
        30_000
    }
    /// Spec §11.1: ~100,000–200,000.
    pub fn la_cold_tier_max_positions() -> usize {
        200_000
    }
    /// Spec §11.1: ~500,000 total.
    pub fn la_total_position_capacity() -> usize {
        500_000
    }
    /// Spec §11.1: >0.5% price move triggers warm recompute.
    pub fn la_warm_price_move_bps() -> u16 {
        50
    }
    /// Spec §11.1: 200ms warm batch interval.
    pub fn la_warm_batch_interval_ms() -> u64 {
        200
    }
    /// Spec §11.1: 2s cold lazy interval.
    pub fn la_cold_recompute_interval_ms() -> u64 {
        2_000
    }
    /// Spec §11.1: 500-block archived cycle.
    pub fn la_archived_cycle_blocks() -> u64 {
        500
    }
    /// Spec §11.3: 60 blocks ≈ 15s on Arbitrum.
    pub fn la_sequencer_restart_window_blocks() -> u64 {
        60
    }

    // ── RelayConfig ───────────────────────────────────────────────────────
    /// Spec §11.2 fix C2: max 4 bundles/relay/second.
    pub fn relay_max_per_second() -> usize {
        4
    }
    /// Spec §11.2: 10ms stagger between bundles.
    pub fn relay_stagger_ms() -> u64 {
        10
    }
    /// Spec §11.2: up to 4 relays in cascade.
    pub fn relay_cascade_max() -> usize {
        4
    }
    /// Spec §11.2 fix I2: 5% tie band for round-robin randomisation.
    pub fn relay_tie_band_fraction() -> f64 {
        0.05
    }

    // ── MlConfig ──────────────────────────────────────────────────────────
    /// Spec §13.1: online learner learning rate.
    pub fn ml_learning_rate() -> f64 {
        0.01
    }
    /// Spec §13.1 fix C1: 20% holdout for validation.
    pub fn ml_validation_ratio() -> f64 {
        0.20
    }
    /// Spec §13.1: validate every 1,000 losses.
    pub fn ml_checkpoint_interval() -> u64 {
        1_000
    }
    /// Spec §13.1 fix C1: revert if holdout win rate drops >5%.
    pub fn ml_revert_threshold() -> f64 {
        0.05
    }
    /// Spec §13.3 fix I5: 5.0× ceiling.
    pub fn ml_multiplier_ceiling() -> f64 {
        5.0
    }
    /// Spec §13: 0.3× floor.
    pub fn ml_multiplier_floor() -> f64 {
        0.3
    }
    /// Spec §13.3 fix I5: 100 consecutive ceiling hits → DEGRADED.
    pub fn ml_ceiling_escalation_threshold() -> u64 {
        100
    }
    /// Spec §13.2 fix I1: retain last 10 checkpoints.
    pub fn ml_checkpoint_retention() -> usize {
        10
    }
    /// Spec §13.2: checkpoint directory.
    pub fn ml_checkpoint_dir() -> String {
        "/var/omega".to_string()
    }

    // ── RotationConfig ────────────────────────────────────────────────────
    /// Spec §14.1 code block: divisor in exp(-months/decay_rate).
    pub fn rotation_decay_rate_months() -> f64 {
        3.0
    }
    /// Spec §14.1 fix C4: 50% base carryover at rotation time.
    pub fn rotation_base_carryover() -> f64 {
        0.50
    }

    // ── VaultConfig ───────────────────────────────────────────────────────
    /// Spec §15.1: 500 bps = 5% DAO fee.
    pub fn vault_dao_fee_bps() -> u16 {
        500
    }
    /// Spec §15.2: minimum 12 confirmations.
    pub fn vault_confirmation_depth() -> u8 {
        12
    }
    /// Spec §15.2: 50 ETH per-transfer cap — the actual spec value, not
    /// an i64-representable approximation, now that WeiAmount stores
    /// this as a TOML string rather than a plain integer.
    pub fn vault_per_transfer_cap_wei() -> WeiAmount {
        WeiAmount::from_eth(50)
    }
    /// Spec §15.2: 500 ETH daily cap — the actual spec value.
    pub fn vault_daily_cap_wei() -> WeiAmount {
        WeiAmount::from_eth(500)
    }

    // ── ApiConfig ─────────────────────────────────────────────────────────
    /// Spec §17.1 fix M4: 300/min authenticated.
    pub fn api_ws_authed_rate() -> u32 {
        300
    }
    /// Spec §17.1 fix M4: 100/min anonymous.
    pub fn api_ws_anon_rate() -> u32 {
        100
    }
    pub fn api_bind_addr() -> String {
        "0.0.0.0:8080".to_string()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Validation
// ─────────────────────────────────────────────────────────────────────────────

impl OmegaConfig {
    /// Validate that all config fields satisfy their invariants.
    ///
    /// Called at startup and after every hot-reload.  Returns a list of
    /// validation errors; an empty list means the config is valid.
    /// Callers should emit `OmegaError::Config` and halt if any errors
    /// are returned.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // Phase gate
        if self.active_phase > 4 {
            errors.push(format!(
                "active_phase {} is invalid — must be 0–4",
                self.active_phase
            ));
        }

        // Gas model
        if !(1.0..=2.0).contains(&self.gas.l2_buffer_factor) {
            errors.push(format!(
                "gas.l2_buffer_factor {} out of range [1.0, 2.0]",
                self.gas.l2_buffer_factor
            ));
        }
        // Previously unvalidated: l1_data_buffer_factor feeds directly
        // into the same dual-component gas cost estimate as
        // l2_buffer_factor (§7) but had no range check at all — a
        // misconfigured value here (e.g. 0.0, or negative) would
        // silently under-cost every blueprint's L1 data component.
        if !(1.0..=2.0).contains(&self.gas.l1_data_buffer_factor) {
            errors.push(format!(
                "gas.l1_data_buffer_factor {} out of range [1.0, 2.0]",
                self.gas.l1_data_buffer_factor
            ));
        }
        if self.gas.max_priority_fee_gwei > 500 {
            errors.push(format!(
                "gas.max_priority_fee_gwei {} exceeds 500 gwei ceiling (§12.2)",
                self.gas.max_priority_fee_gwei
            ));
        }
        if !(0.0..=1.0).contains(&self.gas.conservative_fee_fraction) {
            errors.push(format!(
                "gas.conservative_fee_fraction {} out of range [0.0, 1.0]",
                self.gas.conservative_fee_fraction
            ));
        }

        // LA tier capacity — previously unchecked: nothing stopped
        // hot + warm + cold from exceeding total_position_capacity,
        // which is the kind of misconfiguration that produces an
        // inconsistent/unbounded index at runtime rather than a clear
        // startup error.
        let tier_sum = self
            .la
            .hot_tier_max_positions
            .saturating_add(self.la.warm_tier_max_positions)
            .saturating_add(self.la.cold_tier_max_positions);
        if tier_sum > self.la.total_position_capacity {
            errors.push(format!(
                "la: hot_tier_max_positions + warm_tier_max_positions + cold_tier_max_positions \
                 ({tier_sum}) exceeds total_position_capacity ({}) — the tier hierarchy assumes \
                 the sum leaves room for the archived tier within the total (§11.1)",
                self.la.total_position_capacity
            ));
        }

        // ML model
        if !(0.0..1.0).contains(&self.ml.validation_ratio) {
            errors.push(format!(
                "ml.validation_ratio {} out of range (0.0, 1.0)",
                self.ml.validation_ratio
            ));
        }
        if self.ml.multiplier_ceiling > 5.0 {
            errors.push(format!(
                "ml.multiplier_ceiling {} exceeds 5.0 (§13.3)",
                self.ml.multiplier_ceiling
            ));
        }
        if self.ml.multiplier_floor < 0.1 {
            errors.push(format!(
                "ml.multiplier_floor {} below 0.1 — would suppress all gas bids",
                self.ml.multiplier_floor
            ));
        }
        // Previously unchecked: floor and ceiling were each validated
        // independently, but nothing stopped floor > ceiling as a pair
        // (e.g. ceiling overridden to 2.0 while floor stays at a
        // default that's individually valid but now above it) — an
        // inverted [floor, ceiling] range downstream (e.g. a
        // `.clamp(floor, ceiling)` call) either panics or silently
        // produces a nonsensical multiplier.
        if self.ml.multiplier_floor > self.ml.multiplier_ceiling {
            errors.push(format!(
                "ml.multiplier_floor ({}) exceeds ml.multiplier_ceiling ({}) — \
                 this range is inverted",
                self.ml.multiplier_floor, self.ml.multiplier_ceiling
            ));
        }
        if self.ml.checkpoint_interval == 0 {
            errors.push("ml.checkpoint_interval must be > 0".to_string());
        }
        // Previously unvalidated.
        if !(0.0..=1.0).contains(&self.ml.learning_rate) {
            errors.push(format!(
                "ml.learning_rate {} out of range [0.0, 1.0]",
                self.ml.learning_rate
            ));
        }
        if !(0.0..=1.0).contains(&self.ml.revert_threshold) {
            errors.push(format!(
                "ml.revert_threshold {} out of range [0.0, 1.0]",
                self.ml.revert_threshold
            ));
        }

        // Rotation — previously unvalidated. reputation_decay_rate_months
        // is a DIVISOR in the carryover formula (see RotationConfig doc
        // comment); zero or negative silently corrupts every carryover
        // calculation (division by zero → NaN, or inverted decay into
        // growth) with no error anywhere near the actual computation.
        if self.rotation.reputation_decay_rate_months <= 0.0 {
            errors.push(format!(
                "rotation.reputation_decay_rate_months {} must be > 0.0 — it is a divisor \
                 in the carryover formula (§14.1); zero or negative corrupts every \
                 reputation calculation silently (NaN or inverted decay)",
                self.rotation.reputation_decay_rate_months
            ));
        }
        if !(0.0..=1.0).contains(&self.rotation.base_carryover_fraction) {
            errors.push(format!(
                "rotation.base_carryover_fraction {} out of range [0.0, 1.0]",
                self.rotation.base_carryover_fraction
            ));
        }

        // Vault
        if self.vault.dao_fee_bps > 1_000 {
            errors.push(format!(
                "vault.dao_fee_bps {} exceeds 1,000 bps (10%) ceiling (§15.1, Certora C9)",
                self.vault.dao_fee_bps
            ));
        }
        if self.vault.confirmation_depth < 12 {
            errors.push(format!(
                "vault.confirmation_depth {} is below 12 — Vault contract will reject (§15.2)",
                self.vault.confirmation_depth
            ));
        }
        // Previously unvalidated: nothing stopped either cap from being
        // zero, and nothing stopped per_transfer_cap_wei from exceeding
        // daily_cap_wei — a single transfer capped higher than the
        // supposed daily aggregate limit defeats the purpose of having
        // two separate limits (§15.2).
        if self.vault.per_transfer_cap_wei == WeiAmount::ZERO {
            errors.push("vault.per_transfer_cap_wei must be > 0".to_string());
        }
        if self.vault.daily_cap_wei == WeiAmount::ZERO {
            errors.push("vault.daily_cap_wei must be > 0".to_string());
        }
        if self.vault.per_transfer_cap_wei > self.vault.daily_cap_wei {
            errors.push(format!(
                "vault.per_transfer_cap_wei ({} wei) exceeds vault.daily_cap_wei ({} wei) — \
                 a single transfer could not legitimately exceed the daily aggregate cap (§15.2)",
                self.vault.per_transfer_cap_wei, self.vault.daily_cap_wei
            ));
        }

        // Relay
        if self.relay.cascade_max_relays == 0 {
            errors.push("relay.cascade_max_relays must be ≥ 1".to_string());
        }
        if self.relay.cascade_stagger_ms == 0 {
            errors.push(
                "relay.cascade_stagger_ms must be > 0 — zero stagger re-introduces C2 (§11.2)"
                    .to_string(),
            );
        }
        // Previously unvalidated.
        if !(0.0..=1.0).contains(&self.relay.inclusion_rate_tie_band_fraction) {
            errors.push(format!(
                "relay.inclusion_rate_tie_band_fraction {} out of range [0.0, 1.0]",
                self.relay.inclusion_rate_tie_band_fraction
            ));
        }

        // API
        if self.api.ws_authenticated_msgs_per_min == 0 {
            errors.push("api.ws_authenticated_msgs_per_min must be > 0".to_string());
        }
        // Previously only the authenticated rate was checked for zero;
        // the anonymous rate had the identical failure mode unchecked.
        if self.api.ws_anonymous_msgs_per_min == 0 {
            errors.push("api.ws_anonymous_msgs_per_min must be > 0".to_string());
        }

        errors
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let cfg = OmegaConfig::default();
        let errors = cfg.validate();
        assert!(
            errors.is_empty(),
            "Default config failed validation: {:?}",
            errors
        );
    }

    #[test]
    fn dao_fee_ceiling_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.vault.dao_fee_bps = 1_001;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("dao_fee_bps")));
    }

    #[test]
    fn priority_fee_ceiling_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.gas.max_priority_fee_gwei = 501;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("max_priority_fee_gwei")));
    }

    #[test]
    fn ml_ceiling_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.ml.multiplier_ceiling = 5.1;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("multiplier_ceiling")));
    }

    #[test]
    fn vault_confirmation_depth_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.vault.confirmation_depth = 11;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("confirmation_depth")));
    }

    #[test]
    fn zero_cascade_stagger_rejected() {
        let mut cfg = OmegaConfig::default();
        cfg.relay.cascade_stagger_ms = 0;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("cascade_stagger_ms")));
    }

    #[test]
    fn l1_data_buffer_factor_range_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.gas.l1_data_buffer_factor = 0.5;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("l1_data_buffer_factor")));
    }

    #[test]
    fn ml_floor_exceeding_ceiling_rejected() {
        let mut cfg = OmegaConfig::default();
        cfg.ml.multiplier_ceiling = 2.0;
        cfg.ml.multiplier_floor = 3.0; // individually >= 0.1, but now > ceiling
        let errors = cfg.validate();
        assert!(errors
            .iter()
            .any(|e| e.contains("multiplier_floor") && e.contains("exceeds")));
    }

    #[test]
    fn ml_learning_rate_range_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.ml.learning_rate = 1.5;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("learning_rate")));
    }

    #[test]
    fn ml_revert_threshold_range_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.ml.revert_threshold = -0.1;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("revert_threshold")));
    }

    #[test]
    fn rotation_decay_rate_must_be_positive() {
        let mut cfg = OmegaConfig::default();
        cfg.rotation.reputation_decay_rate_months = 0.0;
        let errors = cfg.validate();
        assert!(errors
            .iter()
            .any(|e| e.contains("reputation_decay_rate_months")));

        let mut cfg2 = OmegaConfig::default();
        cfg2.rotation.reputation_decay_rate_months = -1.0;
        let errors2 = cfg2.validate();
        assert!(errors2
            .iter()
            .any(|e| e.contains("reputation_decay_rate_months")));
    }

    #[test]
    fn rotation_carryover_fraction_range_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.rotation.base_carryover_fraction = 1.5;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("base_carryover_fraction")));
    }

    #[test]
    fn relay_tie_band_range_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.relay.inclusion_rate_tie_band_fraction = 1.2;
        let errors = cfg.validate();
        assert!(errors
            .iter()
            .any(|e| e.contains("inclusion_rate_tie_band_fraction")));
    }

    #[test]
    fn la_tier_capacity_ordering_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.la.total_position_capacity = 1_000; // far below hot+warm+cold defaults
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("total_position_capacity")));
    }

    #[test]
    fn api_anonymous_rate_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.api.ws_anonymous_msgs_per_min = 0;
        let errors = cfg.validate();
        assert!(errors
            .iter()
            .any(|e| e.contains("ws_anonymous_msgs_per_min")));
    }

    #[test]
    fn vault_cap_zero_rejected() {
        let mut cfg = OmegaConfig::default();
        cfg.vault.per_transfer_cap_wei = WeiAmount::ZERO;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("per_transfer_cap_wei")));
    }

    #[test]
    fn vault_per_transfer_exceeding_daily_rejected() {
        let mut cfg = OmegaConfig::default();
        cfg.vault.per_transfer_cap_wei = WeiAmount::from_eth(600);
        cfg.vault.daily_cap_wei = WeiAmount::from_eth(500);
        let errors = cfg.validate();
        assert!(errors
            .iter()
            .any(|e| e.contains("exceeds vault.daily_cap_wei")));
    }

    #[test]
    fn vault_cap_defaults_match_full_spec_values() {
        // Regression test for the bug this fix addresses: the previous
        // u64-typed defaults were both silently reduced to ~9 ETH (the
        // largest TOML-representable magnitude) instead of the spec's
        // 50 ETH / 500 ETH — and, worse, were numerically IDENTICAL to
        // each other. WeiAmount removes that constraint entirely.
        let cfg = OmegaConfig::default();
        assert_eq!(
            cfg.vault.per_transfer_cap_wei.as_wei(),
            50_000_000_000_000_000_000
        );
        assert_eq!(
            cfg.vault.daily_cap_wei.as_wei(),
            500_000_000_000_000_000_000
        );
        assert!(cfg.vault.per_transfer_cap_wei < cfg.vault.daily_cap_wei);
    }

    #[test]
    fn wei_amount_round_trips_through_real_toml() {
        // This is the test that actually proves the original bug is
        // fixed: a plain u64/u128 field would fail this exact
        // round-trip for any value beyond i64::MAX, since the TOML
        // *parser* rejects the literal before serde even runs.
        let cfg = OmegaConfig::default();
        let toml_str = toml::to_string(&cfg).expect("serialize to TOML");
        let parsed: OmegaConfig = toml::from_str(&toml_str).expect("deserialize from TOML");
        assert_eq!(
            parsed.vault.per_transfer_cap_wei,
            cfg.vault.per_transfer_cap_wei
        );
        assert_eq!(parsed.vault.daily_cap_wei, cfg.vault.daily_cap_wei);
    }

    #[test]
    fn wei_amount_deserializes_from_plain_integer_too() {
        #[derive(Deserialize)]
        struct Wrapper {
            amount: WeiAmount,
        }
        let parsed: Wrapper = serde_json::from_str(r#"{"amount": 12345}"#).unwrap();
        assert_eq!(parsed.amount.as_wei(), 12345);
    }

    #[test]
    fn wei_amount_rejects_garbage_string() {
        // `amount` is intentionally unread below: this test only proves
        // deserialization FAILS for a malformed string, so `Wrapper` is
        // never successfully constructed — there is no `Ok` value to
        // read `.amount` from. The field's presence is what drives
        // `WeiAmount`'s custom `Deserialize` impl under test here, not
        // any value read from it. See this file's module-level "Fix
        // (this revision)" note for why `#[allow(dead_code)]` is the
        // correct fix rather than changing this test's behavior.
        #[allow(dead_code)]
        #[derive(Deserialize)]
        struct Wrapper {
            amount: WeiAmount,
        }
        let result: Result<Wrapper, _> = serde_json::from_str(r#"{"amount": "not_a_number"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn unknown_top_level_field_rejected() {
        // Regression test for the top-level deny_unknown_fields gap:
        // every sub-config already rejected unknown fields, but
        // OmegaConfig itself did not.
        let bad_toml = r#"
            active_phase = 1
            bogus_top_level_field = true
        "#;
        let result: Result<OmegaConfig, _> = toml::from_str(bad_toml);
        assert!(
            result.is_err(),
            "unknown top-level field must now be rejected"
        );
    }

    #[test]
    fn default_values_match_spec() {
        let cfg = OmegaConfig::default();
        // §12.2
        assert_eq!(cfg.gas.max_priority_fee_gwei, 500);
        // §11.2 fix C2
        assert_eq!(cfg.relay.max_bundles_per_relay_per_second, 4);
        assert_eq!(cfg.relay.cascade_stagger_ms, 10);
        // §13.1 fix C1
        assert!((cfg.ml.validation_ratio - 0.20).abs() < f64::EPSILON);
        assert_eq!(cfg.ml.checkpoint_interval, 1_000);
        // §13.3 fix I5
        assert_eq!(cfg.ml.ceiling_escalation_threshold, 100);
        // §14.1 fix C4
        assert!((cfg.rotation.base_carryover_fraction - 0.50).abs() < f64::EPSILON);
        assert!((cfg.rotation.reputation_decay_rate_months - 3.0).abs() < f64::EPSILON);
        // §15.1
        assert_eq!(cfg.vault.dao_fee_bps, 500);
        assert_eq!(cfg.vault.confirmation_depth, 12);
        // §17.1 fix M4
        assert_eq!(cfg.api.ws_authenticated_msgs_per_min, 300);
        assert_eq!(cfg.api.ws_anonymous_msgs_per_min, 100);
        // §11.1
        assert_eq!(cfg.la.warm_batch_interval_ms, 200);
        assert_eq!(cfg.la.archived_cycle_blocks, 500);
        // §11.3
        assert_eq!(cfg.la.sequencer_restart_window_blocks, 60);
    }
}

'@
Set-Content -Path 'crates\omega-core\src\config.rs' -Value $content_1 -Encoding UTF8 -NoNewline

Write-Host 'Writing crates\omega-compliance\src\ofa.rs...'
$content_2 = @'
// crates/omega-compliance/src/ofa.rs
//
// Order Flow Agreement (OFA) compliance validation (spec §8).
//
// ## What OFA is
//
//   OFA is a user-protection mechanism: when a user submits a swap
//   through an OFA-compliant relay (e.g. MEV-Share), they consent to
//   searchers backrunning their transaction in exchange for a portion
//   of the extracted value.  The OFA contract specifies:
//     - Consent: the user has opted into this order flow program
//     - Slippage: the backrun must not worsen the user's slippage
//     - Order validity: the order must not be expired or malformed
//
// ## Compliance obligations (spec §8)
//
//   Every blueprint with `ofa_compliant = true` MUST pass all three
//   checks before relay submission:
//
//     1. ConsentCheck   — user has a valid, unexpired OFA consent record
//     2. SlippageCheck  — `price_impact_bps ≤ consent.max_slippage_bps`
//     3. OrderCheck     — order is well-formed and within validity window
//
//   A blueprint that fails any check is discarded with the corresponding
//   DropCode and is NOT submitted to any relay.
//
// ## Versioned rule sets (spec §8)
//
//   OFA rules are versioned.  Each `OfaRuleSet` has an activation
//   timestamp; the compliance checker uses the most recently activated
//   rule set that is ≤ the current time.  Rule set updates use the L2
//   fast-approve governance path (§5).  Downgrades are blocked — the
//   active version can only increase.
//
// ## Fix (this revision): tests::dummy_bp missing 7 ExecutionBlueprint fields
//
// `dummy_bp` predates the `signal_id` / `client_order_id` /
// `idempotency_key` fields (added for submission idempotency) AND the
// `flashloan_provider_type` / `provider_contract` / `flashloan_token`
// fields (added for real flashloan provider/pool selection — see
// `omega_core::types::blueprint`'s own module doc comment) — 7 fields
// total, matching the compiler's `E0063: missing fields client_order_id,
// flashloan_provider_type, flashloan_token and 4 other fields` exactly.
//
// None of these checks (`check_consent`/`check_slippage`/`check_order`)
// read any of the 7, so — same reasoning already established elsewhere
// in this codebase's test helpers — they're placeholder values here:
//   - `signal_id`/`client_order_id`/`idempotency_key`: deterministic
//     placeholders, not integrity-checked (no test here calls
//     `verify_hash()`/`verify_idempotency_key()`).
//   - `flashloan_provider_type`/`provider_contract`/`flashloan_token`:
//     this blueprint doesn't source a real flashloan
//     (`flashloan_provider: Address::ZERO`, matching the file's existing
//     "no flashloan" convention), so these are inert placeholders too.
//   - `max_base_fee_gwei`: set from `base_fee_at_creation * 3`, the same
//     placeholder headroom multiplier used in `sa.rs`/`la.rs`/`msa.rs`/
//     `mev.rs` pending confirmation of this field's real intended
//     semantics (per those files' own comments) — not read by any check
//     in this file either.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use omega_core::errors::DropCode;
use omega_core::types::blueprint::ExecutionBlueprint;

// ─────────────────────────────────────────────────────────────────────────────
// OfaConsentRecord
// ─────────────────────────────────────────────────────────────────────────────

/// A user's consent record for OFA participation.
///
/// Stored by the OFA relay and supplied to the compliance checker.
/// The relay is responsible for fetching and verifying the on-chain
/// consent signature; omega-compliance trusts the provided record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfaConsentRecord {
    /// User's wallet address (hex-encoded).
    pub user: String,
    /// Maximum slippage the user accepts in basis points.
    pub max_slippage_bps: u16,
    /// When this consent record expires.
    pub expires_at: DateTime<Utc>,
    /// OFA program identifier (e.g. "mev_share_v1", "flashbots_ofa_v2").
    pub program_id: String,
    /// Whether this consent is still active (not revoked).
    pub is_active: bool,
}

impl OfaConsentRecord {
    /// Returns `true` when the consent is valid at the given instant.
    ///
    /// A consent is valid when:
    ///   - `is_active` is true (not revoked)
    ///   - `expires_at` is in the future
    pub fn is_valid_at(&self, now: DateTime<Utc>) -> bool {
        self.is_active && self.expires_at > now
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OfaOrder
// ─────────────────────────────────────────────────────────────────────────────

/// A parsed OFA order from the MEV-Share SSE stream.
///
/// The order is emitted by the relay after the user's transaction is
/// included and describes the backrun opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfaOrder {
    /// Unique order identifier from the relay.
    pub order_id: String,
    /// Block number at which this order was created.
    pub created_at: u64,
    /// Block number after which this order expires.
    pub expires_at: u64,
    /// Maximum slippage the backrun may impose, in basis points.
    pub max_slippage_bps: u16,
    /// Whether the order has been filled by another searcher.
    pub is_filled: bool,
}

impl OfaOrder {
    /// Returns `true` when the order is valid at `current_block`.
    ///
    /// An order is valid when:
    ///   - It has not been filled
    ///   - `current_block ≤ expires_at`
    pub fn is_valid_at_block(&self, current_block: u64) -> bool {
        !self.is_filled && current_block <= self.expires_at
    }

    /// Returns `true` when the order is well-formed.
    ///
    /// An order is malformed when:
    ///   - `order_id` is empty
    ///   - `expires_at < created_at` (impossible validity window)
    ///   - `max_slippage_bps > 10_000` (>100% slippage is nonsensical)
    pub fn is_well_formed(&self) -> bool {
        !self.order_id.is_empty()
            && self.expires_at >= self.created_at
            && self.max_slippage_bps <= 10_000
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OfaRuleSet
// ─────────────────────────────────────────────────────────────────────────────

/// A versioned OFA compliance rule set (spec §8).
///
/// Rule set updates are applied via L2 fast-approve governance.
/// The active rule set is the most recently activated one
/// (activated_at ≤ Utc::now()).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfaRuleSet {
    /// Monotonically increasing version number.
    pub version: u32,
    /// When this rule set became active (UTC).
    pub activated_at: DateTime<Utc>,
    /// Maximum age of a consent record in seconds before it is
    /// considered stale (default 86400 = 24 hours).
    pub consent_max_age_secs: u64,
    /// Maximum order age in blocks (default 20 blocks ≈ 5s on Arbitrum).
    pub order_max_age_blocks: u64,
    /// Maximum slippage imposed by backrun, in basis points (default 50).
    pub backrun_slippage_cap_bps: u16,
}

impl Default for OfaRuleSet {
    fn default() -> Self {
        Self {
            version: 1,
            activated_at: DateTime::UNIX_EPOCH,
            consent_max_age_secs: 86_400,
            order_max_age_blocks: 20,
            backrun_slippage_cap_bps: 50,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OfaCheckError
// ─────────────────────────────────────────────────────────────────────────────

/// A typed OFA compliance failure.
///
/// Each variant maps to exactly one `DropCode` for the Loss Attribution
/// Engine.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OfaCheckError {
    #[error("OFA consent missing or expired for user {user}")]
    ConsentMissingOrExpired { user: String },

    #[error("OFA consent revoked for user {user}")]
    ConsentRevoked { user: String },

    #[error("Blueprint price_impact_bps {impact} exceeds consent max_slippage_bps {max}")]
    SlippageExceedsConsent { impact: u16, max: u16 },

    #[error("Blueprint price_impact_bps {impact} exceeds rule set backrun cap {cap}")]
    SlippageExceedsRuleCap { impact: u16, cap: u16 },

    #[error("OFA order is malformed: order_id={order_id}")]
    OrderMalformed { order_id: String },

    #[error("OFA order has expired at block {current} (expires_at={expires})")]
    OrderExpired { current: u64, expires: u64 },

    #[error("OFA order is already filled: order_id={order_id}")]
    OrderAlreadyFilled { order_id: String },
}

impl OfaCheckError {
    /// Maps this error to the `DropCode` used in the Loss Attribution Engine.
    pub fn drop_code(&self) -> DropCode {
        match self {
            OfaCheckError::ConsentMissingOrExpired { .. }
            | OfaCheckError::ConsentRevoked { .. } => DropCode::MissOfaConsent,

            OfaCheckError::SlippageExceedsConsent { .. }
            | OfaCheckError::SlippageExceedsRuleCap { .. } => DropCode::MissOfaSlippage,

            OfaCheckError::OrderMalformed { .. }
            | OfaCheckError::OrderExpired { .. }
            | OfaCheckError::OrderAlreadyFilled { .. } => DropCode::MissOfaOrder,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OfaChecker
// ─────────────────────────────────────────────────────────────────────────────

/// Stateless OFA compliance checker (spec §8).
///
/// All methods are pure functions — no I/O, no async.
pub struct OfaChecker;

impl OfaChecker {
    /// Validate OFA consent for a blueprint.
    ///
    /// Returns `Ok(())` when the consent is valid, active, and unexpired.
    /// Returns `Err(OfaCheckError)` when any condition fails.
    pub fn check_consent(
        consent: &OfaConsentRecord,
        rules: &OfaRuleSet,
        now: DateTime<Utc>,
    ) -> Result<(), OfaCheckError> {
        if !consent.is_active {
            return Err(OfaCheckError::ConsentRevoked {
                user: consent.user.clone(),
            });
        }
        if !consent.is_valid_at(now) {
            return Err(OfaCheckError::ConsentMissingOrExpired {
                user: consent.user.clone(),
            });
        }
        // Check consent age against rule set max
        let age_secs = now
            .signed_duration_since(consent.expires_at - chrono::Duration::days(1))
            .num_seconds()
            .max(0) as u64;
        if age_secs > rules.consent_max_age_secs {
            return Err(OfaCheckError::ConsentMissingOrExpired {
                user: consent.user.clone(),
            });
        }
        Ok(())
    }

    /// Validate OFA slippage constraints for a blueprint.
    ///
    /// Returns `Ok(())` when `price_impact_bps` does not exceed either
    /// the user's consent slippage cap or the rule set backrun cap.
    pub fn check_slippage(
        bp: &ExecutionBlueprint,
        consent: &OfaConsentRecord,
        rules: &OfaRuleSet,
    ) -> Result<(), OfaCheckError> {
        let impact = bp.price_impact_bps.unwrap_or(0);

        if impact > consent.max_slippage_bps {
            return Err(OfaCheckError::SlippageExceedsConsent {
                impact,
                max: consent.max_slippage_bps,
            });
        }

        if impact > rules.backrun_slippage_cap_bps {
            return Err(OfaCheckError::SlippageExceedsRuleCap {
                impact,
                cap: rules.backrun_slippage_cap_bps,
            });
        }

        Ok(())
    }

    /// Validate the OFA order at the current block.
    ///
    /// Returns `Ok(())` when the order is well-formed, unfilled, and
    /// within its validity window.
    pub fn check_order(order: &OfaOrder, current_block: u64) -> Result<(), OfaCheckError> {
        if !order.is_well_formed() {
            return Err(OfaCheckError::OrderMalformed {
                order_id: order.order_id.clone(),
            });
        }
        if order.is_filled {
            return Err(OfaCheckError::OrderAlreadyFilled {
                order_id: order.order_id.clone(),
            });
        }
        if !order.is_valid_at_block(current_block) {
            return Err(OfaCheckError::OrderExpired {
                current: current_block,
                expires: order.expires_at,
            });
        }
        Ok(())
    }

    /// Run all three OFA checks for an `ofa_compliant = true` blueprint.
    ///
    /// Short-circuits on the first failure.  The caller receives the
    /// `DropCode` to record in the loss attribution pipeline.
    pub fn validate_blueprint(
        bp: &ExecutionBlueprint,
        consent: &OfaConsentRecord,
        order: &OfaOrder,
        rules: &OfaRuleSet,
        now: DateTime<Utc>,
        current_block: u64,
    ) -> Result<(), OfaCheckError> {
        Self::check_consent(consent, rules, now)?;
        Self::check_slippage(bp, consent, rules)?;
        Self::check_order(order, current_block)?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, Bytes, B256, U256};
    use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
    use omega_core::types::flashloan_provider::FlashloanProviderType;
    use omega_core::types::lane::{Lane, Simulator};
    use uuid::Uuid;

    fn consent(active: bool, max_slippage_bps: u16, expires_minutes: i64) -> OfaConsentRecord {
        OfaConsentRecord {
            user: "0xUser".into(),
            max_slippage_bps,
            expires_at: Utc::now() + chrono::Duration::minutes(expires_minutes),
            program_id: "mev_share_v1".into(),
            is_active: active,
        }
    }

    fn order(filled: bool, created: u64, expires: u64) -> OfaOrder {
        OfaOrder {
            order_id: "ord_001".into(),
            created_at: created,
            expires_at: expires,
            max_slippage_bps: 50,
            is_filled: filled,
        }
    }

    fn rules() -> OfaRuleSet {
        OfaRuleSet::default()
    }

    /// See this file's module-level "Fix (this revision)" note for why
    /// `signal_id`/`client_order_id`/`idempotency_key`/
    /// `flashloan_provider_type`/`provider_contract`/`flashloan_token`/
    /// `max_base_fee_gwei` are placeholder values here — none of
    /// `OfaChecker`'s checks read any of them.
    fn dummy_bp(price_impact_bps: Option<u16>) -> ExecutionBlueprint {
        let signal_id = Uuid::from_bytes([0x00u8; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(StrategyId::Mev, 42161, 1, signal_id);
        ExecutionBlueprint {
            blueprint_hash: B256::ZERO,
            chain_id: 42161,
            strategy_id: StrategyId::Mev,
            lane: Lane::Normal,
            simulator: Simulator::Anvil,
            signal_state_hash: B256::ZERO,
            state_version: 1,
            signal_id,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::ZERO,
            flashloan_available: U256::ZERO,
            flashloan_provider_type: FlashloanProviderType::Balancer,
            provider_contract: Address::ZERO,
            flashloan_token: Address::ZERO,
            calldata: Bytes::new(),
            strategy_bytecode_hash: B256::ZERO,
            l2_exec_gas_estimate: 200_000,
            l1_data_gas_estimate: 4_000,
            extraction_gas: 21_000,
            expected_profit_net: U256::from(1_000_000_000_000_000_u128),
            dynamic_min_profit: U256::from(500_000_000_000_000_u128),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            slippage_bps: 30,
            base_fee_at_creation: 10,
            l1_data_fee_at_creation: 2,
            priority_fee_gwei: 100,
            max_base_fee_gwei: 30, // base_fee_at_creation * 3 — see module note
            price_impact_bps,
            ofa_compliant: true,
            expiry_block: 1_000_001,
            nonce: 1,
            confirmation_depth: 12,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec!["mev_share".into()],
            zk_proof_commitment: None,
        }
    }

    // ── ConsentCheck ─────────────────────────────────────────────────────────

    #[test]
    fn consent_valid() {
        let c = consent(true, 100, 60);
        assert!(OfaChecker::check_consent(&c, &rules(), Utc::now()).is_ok());
    }

    #[test]
    fn consent_revoked() {
        let c = consent(false, 100, 60);
        let err = OfaChecker::check_consent(&c, &rules(), Utc::now()).unwrap_err();
        assert_eq!(err.drop_code(), DropCode::MissOfaConsent);
        assert!(matches!(err, OfaCheckError::ConsentRevoked { .. }));
    }

    #[test]
    fn consent_expired() {
        let c = consent(true, 100, -1); // expired 1 minute ago
        let err = OfaChecker::check_consent(&c, &rules(), Utc::now()).unwrap_err();
        assert_eq!(err.drop_code(), DropCode::MissOfaConsent);
        assert!(matches!(err, OfaCheckError::ConsentMissingOrExpired { .. }));
    }

    // ── SlippageCheck ─────────────────────────────────────────────────────────

    #[test]
    fn slippage_within_limits() {
        let bp = dummy_bp(Some(30));
        let c = consent(true, 100, 60);
        assert!(OfaChecker::check_slippage(&bp, &c, &rules()).is_ok());
    }

    #[test]
    fn slippage_exceeds_consent() {
        let bp = dummy_bp(Some(150));
        let c = consent(true, 100, 60); // user cap = 100 bps
        let err = OfaChecker::check_slippage(&bp, &c, &rules()).unwrap_err();
        assert_eq!(err.drop_code(), DropCode::MissOfaSlippage);
        assert!(matches!(
            err,
            OfaCheckError::SlippageExceedsConsent {
                impact: 150,
                max: 100
            }
        ));
    }

    #[test]
    fn slippage_exceeds_rule_cap() {
        // Rule cap is 50 bps; user allows 200 bps
        let bp = dummy_bp(Some(80));
        let c = consent(true, 200, 60);
        let err = OfaChecker::check_slippage(&bp, &c, &rules()).unwrap_err();
        assert_eq!(err.drop_code(), DropCode::MissOfaSlippage);
        assert!(matches!(
            err,
            OfaCheckError::SlippageExceedsRuleCap {
                impact: 80,
                cap: 50
            }
        ));
    }

    #[test]
    fn no_price_impact_passes_slippage() {
        let bp = dummy_bp(None); // no AMM swaps
        let c = consent(true, 100, 60);
        assert!(OfaChecker::check_slippage(&bp, &c, &rules()).is_ok());
    }

    // ── OrderCheck ───────────────────────────────────────────────────────────

    #[test]
    fn order_valid() {
        let o = order(false, 1000, 1020);
        assert!(OfaChecker::check_order(&o, 1010).is_ok());
    }

    #[test]
    fn order_expired() {
        let o = order(false, 1000, 1005);
        let err = OfaChecker::check_order(&o, 1010).unwrap_err(); // current > expires
        assert_eq!(err.drop_code(), DropCode::MissOfaOrder);
        assert!(matches!(err, OfaCheckError::OrderExpired { .. }));
    }

    #[test]
    fn order_already_filled() {
        let o = order(true, 1000, 1020);
        let err = OfaChecker::check_order(&o, 1010).unwrap_err();
        assert_eq!(err.drop_code(), DropCode::MissOfaOrder);
        assert!(matches!(err, OfaCheckError::OrderAlreadyFilled { .. }));
    }

    #[test]
    fn order_malformed_empty_id() {
        let o = OfaOrder {
            order_id: String::new(),
            created_at: 1000,
            expires_at: 1020,
            max_slippage_bps: 50,
            is_filled: false,
        };
        let err = OfaChecker::check_order(&o, 1010).unwrap_err();
        assert_eq!(err.drop_code(), DropCode::MissOfaOrder);
        assert!(matches!(err, OfaCheckError::OrderMalformed { .. }));
    }

    // ── validate_blueprint ────────────────────────────────────────────────────

    #[test]
    fn full_validation_passes() {
        let bp = dummy_bp(Some(30));
        let c = consent(true, 100, 60);
        let o = order(false, 1_000_000, 1_000_020);
        assert!(
            OfaChecker::validate_blueprint(&bp, &c, &o, &rules(), Utc::now(), 1_000_010,).is_ok()
        );
    }

    #[test]
    fn full_validation_short_circuits_on_consent() {
        let bp = dummy_bp(Some(30));
        let c = consent(false, 100, 60); // revoked
        let o = order(false, 1_000_000, 1_000_020);
        let err = OfaChecker::validate_blueprint(&bp, &c, &o, &rules(), Utc::now(), 1_000_010)
            .unwrap_err();
        assert_eq!(err.drop_code(), DropCode::MissOfaConsent);
    }
}

'@
Set-Content -Path 'crates\omega-compliance\src\ofa.rs' -Value $content_2 -Encoding UTF8 -NoNewline

Write-Host 'Writing crates\omega-compliance\src\policy.rs...'
$content_3 = @'
// crates/omega-compliance/src/policy.rs
//
// ## Fix (this revision): asset_symbol()/notional_value() don't exist on
// ExecutionBlueprint, and can't be implemented on it
//
// `validate_blueprint` previously called `bp.asset_symbol()` and
// `bp.notional_value()` — neither method exists on
// `omega_core::types::blueprint::ExecutionBlueprint`
// (`error[E0599]: no method named ... found`). The original code's own
// comments ("Implement or adapt to your blueprint fields", "Implement
// helper if needed") mark these as unfinished stubs, not a real
// implementation that just needs wiring up.
//
// They can't be added to `ExecutionBlueprint` itself, either: that
// struct's actual fields are `flashloan_provider: Address`,
// `flashloan_amount: U256` (raw token units), and
// `expected_profit_net: U256` (wei) — there is no human-readable token
// symbol anywhere on it, and no USD-denominated price. Deriving an
// "asset symbol" from a raw contract address, or a "notional value" in
// USD without a price oracle, would mean fabricating exactly the data
// this compliance check exists to verify — an allowlist/position-size
// gate that silently guesses at the asset and dollar value it's
// checking is worse than one that fails to compile, since a wrong guess
// here fails *open* (a disallowed asset or oversized position reads as
// compliant) rather than failing loudly.
//
// This crate's own dependency list (see imports below: `omega_core`,
// `chrono`, `serde`, `thiserror` — no `omega_oracle`) confirms it has no
// price-feed access of its own to compute either value correctly.
//
// Fixed by making both values explicit, caller-supplied parameters to
// `validate_blueprint` instead of methods on the blueprint. The caller
// — whatever code sits between the oracle/pricing layer and this
// compliance gate — already has to resolve "what token does this
// blueprint touch, and what's it worth in USD" for other reasons (gas
// cost accounting, profit reporting); this makes that resolution an
// explicit, visible input to the compliance decision rather than an
// implicit method call that silently returns nothing meaningful. This
// is a breaking signature change for any existing caller of
// `validate_blueprint`; there was no way to fix the underlying missing
// data without one.
//
// ## Fix (this revision, 2): sample_blueprint missing 4
// ExecutionBlueprint fields
//
// `omega-core` added four more required fields to `ExecutionBlueprint`
// (`flashloan_provider_type`, `provider_contract`, `flashloan_token`,
// `max_base_fee_gwei`) to support real flashloan provider/pool
// selection — see that crate's `types::blueprint` module doc comment.
// `sample_blueprint` here predates them
// (`error[E0063]: missing fields flashloan_provider_type,
// flashloan_token, max_base_fee_gwei and 1 other field`).
// `ComplianceChecker::validate_blueprint` reads none of the four — its
// checks are asset/chain/position-size/time-window/strategy only — so
// these are inert placeholders, same treatment as every other
// test-only `ExecutionBlueprint` literal fixed elsewhere in this
// workspace: `flashloan_provider_type: FlashloanProviderType::Balancer`
// / `provider_contract: Address::ZERO` / `flashloan_token: Address::ZERO`
// alongside the existing `flashloan_provider: Address::ZERO` no-flashloan
// path, and `max_base_fee_gwei` derived from `base_fee_at_creation * 3`
// matching the placeholder headroom multiplier used in
// `omega-strategies`' `sa.rs`/`la.rs`/`msa.rs`/`mev.rs`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

use omega_core::types::blueprint::ExecutionBlueprint; // Adjust import as needed in your core

#[derive(Debug, Clone, Error)]
pub enum ComplianceError {
    #[error("Policy violation: {0}")]
    Violation(String),
    #[error("Configuration error: {0}")]
    Config(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TimeWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompliancePolicy {
    pub allowed_assets: Vec<String>,
    pub allowed_chains: Vec<u64>,
    pub max_position_size_usd: f64,
    pub max_leverage_bps: u16,
    pub trading_windows: Vec<TimeWindow>,
    pub cooldown_period_secs: u64,
    pub allowed_strategies: Vec<String>,
}

impl Default for CompliancePolicy {
    fn default() -> Self {
        Self {
            allowed_assets: vec!["ETH".into(), "BTC".into(), "USDC".into()],
            allowed_chains: vec![42161], // Arbitrum mainnet
            max_position_size_usd: 100_000.0,
            max_leverage_bps: 5000, // 50x example
            trading_windows: vec![],
            cooldown_period_secs: 300,
            allowed_strategies: vec!["mev".into(), "flashloan".into()],
        }
    }
}

#[derive(Debug)]
pub struct ComplianceChecker {
    policy: Arc<CompliancePolicy>,
}

impl ComplianceChecker {
    pub fn new(policy: CompliancePolicy) -> Self {
        Self {
            policy: Arc::new(policy),
        }
    }

    /// Validate a blueprint against the configured compliance policy.
    ///
    /// `asset_symbol` and `notional_value_usd` are supplied by the
    /// caller rather than read off `bp` — see this file's module-level
    /// "Fix" note for why: `ExecutionBlueprint` carries a raw
    /// `flashloan_provider` address and wei-denominated amounts, not a
    /// human-readable symbol or a USD price, so resolving either
    /// requires a price/token-metadata lookup this crate has no access
    /// to. The caller (sitting closer to the oracle/pricing layer) is
    /// expected to resolve both before calling this.
    pub fn validate_blueprint(
        &self,
        bp: &ExecutionBlueprint,
        asset_symbol: &str,
        notional_value_usd: f64,
        now: DateTime<Utc>,
    ) -> Result<(), ComplianceError> {
        // Asset permission
        if !self.policy.allowed_assets.iter().any(|a| a == asset_symbol) {
            return Err(ComplianceError::Violation(format!(
                "Asset {asset_symbol} not allowed"
            )));
        }

        // Chain permission
        if !self.policy.allowed_chains.contains(&bp.chain_id) {
            return Err(ComplianceError::Violation(format!(
                "Chain {} not allowed",
                bp.chain_id
            )));
        }

        // Position size
        if notional_value_usd > self.policy.max_position_size_usd {
            return Err(ComplianceError::Violation(
                "Exceeds max position size".into(),
            ));
        }

        // Time window
        if !self.is_in_trading_window(now) {
            return Err(ComplianceError::Violation(
                "Outside allowed trading window".into(),
            ));
        }

        // Strategy
        if !self
            .policy
            .allowed_strategies
            .contains(&bp.strategy_id.to_string())
        {
            return Err(ComplianceError::Violation("Strategy not allowed".into()));
        }

        Ok(())
    }

    fn is_in_trading_window(&self, now: DateTime<Utc>) -> bool {
        if self.policy.trading_windows.is_empty() {
            return true; // No restriction
        }
        self.policy
            .trading_windows
            .iter()
            .any(|w| w.start <= now && now <= w.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, Bytes, B256, U256};
    use omega_core::types::blueprint::StrategyId;
    use omega_core::types::flashloan_provider::FlashloanProviderType;
    use omega_core::types::lane::{Lane, Simulator};
    use uuid::Uuid;

    fn sample_blueprint(chain_id: u64) -> ExecutionBlueprint {
        let signal_id = Uuid::from_bytes([0xAAu8; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(StrategyId::Sa, chain_id, 1, signal_id);
        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO,
            chain_id,
            strategy_id: StrategyId::Sa,
            lane: Lane::Microtx,
            simulator: Simulator::Revm,
            signal_state_hash: B256::from([0xABu8; 32]),
            state_version: 1,
            signal_id,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::from(1_000_000u64),
            flashloan_available: U256::from(2_000_000u64),
            flashloan_provider_type: FlashloanProviderType::Balancer,
            provider_contract: Address::ZERO,
            flashloan_token: Address::ZERO,
            calldata: Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
            strategy_bytecode_hash: B256::from([0xCDu8; 32]),
            l2_exec_gas_estimate: 100_000,
            l1_data_gas_estimate: 5_000,
            extraction_gas: 45_000,
            expected_profit_net: U256::from(1u64),
            dynamic_min_profit: U256::from(1u64),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            slippage_bps: 20,
            base_fee_at_creation: 1,
            l1_data_fee_at_creation: 40,
            priority_fee_gwei: 10,
            max_base_fee_gwei: 3, // base_fee_at_creation * 3 — see module note
            price_impact_bps: None,
            ofa_compliant: true,
            expiry_block: 1_000,
            nonce: 1,
            confirmation_depth: 12,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec![],
            zk_proof_commitment: None,
        };
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();
        bp
    }

    #[test]
    fn allowed_asset_and_chain_and_size_passes() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        let bp = sample_blueprint(42161);
        let now = Utc::now();
        assert!(checker
            .validate_blueprint(&bp, "ETH", 50_000.0, now)
            .is_ok());
    }

    #[test]
    fn disallowed_asset_is_rejected() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        let bp = sample_blueprint(42161);
        let now = Utc::now();
        let err = checker
            .validate_blueprint(&bp, "DOGE", 1_000.0, now)
            .unwrap_err();
        assert!(matches!(err, ComplianceError::Violation(msg) if msg.contains("DOGE")));
    }

    #[test]
    fn disallowed_chain_is_rejected() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        let bp = sample_blueprint(1); // Ethereum mainnet, not in default allowed_chains
        let now = Utc::now();
        let err = checker
            .validate_blueprint(&bp, "ETH", 1_000.0, now)
            .unwrap_err();
        assert!(matches!(err, ComplianceError::Violation(_)));
    }

    #[test]
    fn oversized_position_is_rejected() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        let bp = sample_blueprint(42161);
        let now = Utc::now();
        let err = checker
            .validate_blueprint(&bp, "ETH", 1_000_000.0, now) // > 100_000 default cap
            .unwrap_err();
        assert!(matches!(err, ComplianceError::Violation(msg) if msg.contains("position size")));
    }

    #[test]
    fn empty_trading_windows_means_unrestricted() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        assert!(checker.is_in_trading_window(Utc::now()));
    }

    #[test]
    fn outside_configured_trading_window_is_rejected() {
        let mut policy = CompliancePolicy::default();
        let now = Utc::now();
        policy.trading_windows = vec![TimeWindow {
            start: now - chrono::Duration::hours(2),
            end: now - chrono::Duration::hours(1),
        }];
        let checker = ComplianceChecker::new(policy);
        let bp = sample_blueprint(42161);
        let err = checker
            .validate_blueprint(&bp, "ETH", 1_000.0, now)
            .unwrap_err();
        assert!(matches!(err, ComplianceError::Violation(msg) if msg.contains("trading window")));
    }
}

'@
Set-Content -Path 'crates\omega-compliance\src\policy.rs' -Value $content_3 -Encoding UTF8 -NoNewline

Write-Host ''
Write-Host 'Verifying...'
$check = Select-String -Path 'crates\omega-rpc\src\net.rs' -Pattern 'an InvalidUrl error must be fatal' -Quiet
if ($check) { Write-Host '  OK: crates\omega-rpc\src\net.rs' } else { Write-Host '  MISSING: crates\omega-rpc\src\net.rs' -ForegroundColor Red }
$check = Select-String -Path 'crates\omega-core\src\config.rs' -Pattern 'allow\(dead_code\)' -Quiet
if ($check) { Write-Host '  OK: crates\omega-core\src\config.rs' } else { Write-Host '  MISSING: crates\omega-core\src\config.rs' -ForegroundColor Red }
$check = Select-String -Path 'crates\omega-compliance\src\ofa.rs' -Pattern 'flashloan_provider_type' -Quiet
if ($check) { Write-Host '  OK: crates\omega-compliance\src\ofa.rs' } else { Write-Host '  MISSING: crates\omega-compliance\src\ofa.rs' -ForegroundColor Red }
$check = Select-String -Path 'crates\omega-compliance\src\policy.rs' -Pattern 'flashloan_provider_type' -Quiet
if ($check) { Write-Host '  OK: crates\omega-compliance\src\policy.rs' } else { Write-Host '  MISSING: crates\omega-compliance\src\policy.rs' -ForegroundColor Red }

Write-Host ''
Write-Host 'REMINDER: crates/omega-compliance/Cargo.toml still needs a uuid dev-dependency added manually (see chat) — this script cannot edit it without seeing the file.' -ForegroundColor Yellow