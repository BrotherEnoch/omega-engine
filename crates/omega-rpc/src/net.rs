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
        assert_eq!(redact_ws_url("wss://node.example.com"), "wss://node.example.com/<redacted>");
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
        assert!(!err.is_fatal() == false || err.is_fatal()); // sanity: is_fatal() must be true
        assert!(err.is_fatal());
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
        let err = RpcClientError::ChainIdMismatch { expected: 42161, actual: 1 };
        assert!(err.is_fatal());
    }

    #[test]
    fn connect_failed_is_not_fatal() {
        let err = RpcClientError::ConnectFailed("timeout".to_string());
        assert!(!err.is_fatal());
    }
}