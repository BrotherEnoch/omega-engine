// crates/omega-rpc/src/net.rs
//
// Connection-boundary helpers for omega-rpc: error type, URL validation and
// redaction, chain-ID verification, numeric conversion, and RPC error classification.
//
// ## Audit findings this file closes (prior revisions)
//
//   - RpcClientError::is_fatal: `connect_with_retry` retried forever regardless of
//     cause, which gives an operator no signal that the problem needs their
//     attention, not more patience.
//   - verify_chain_id: nothing anywhere in this crate previously checked that the
//     connected endpoint's actual chain matched what was configured. A misrouted or
//     misconfigured endpoint would silently mislabel every downstream signal with
//     the wrong chain_id.
//   - redact_ws_url: most RPC providers embed an API key directly in the URL (path
//     or query string). Logging the raw URL — as the previous code did at info/warn
//     level — leaks that key into log aggregation.
//   - wei_to_gwei_saturating: a bare `as u64` cast after wei→gwei division silently
//     wraps an absurd/malformed value from the RPC endpoint into a small, WRONG
//     number — dangerous specifically because it's wrong in the unsafe direction
//     (an artificially low gas cost estimate makes a trade look more profitable than
//     it is).
//   - validate_ws_scheme (now superseded by validate_rpc_url): fail immediately and
//     clearly on an obviously wrong URL rather than relying on whatever downstream
//     error eventually surfaces.
//
// ## Fix (prior revision): clippy::bool_comparison in validate_ws_scheme_rejects_http
//
// `assert!(!err.is_fatal() == false || err.is_fatal());` was leftover edit debris —
// a tautology flagged by clippy's `bool_comparison`. Removed; the next line already
// asserts the real invariant.
//
// ## Changes (this revision): HTTP transport support
//
//   - `validate_rpc_url` replaces `validate_ws_scheme` at every call site: it accepts
//     ws://, wss://, http:// and https:// and returns the implied `RpcTransport`.
//     `validate_ws_scheme` is kept (unused, `#[allow(dead_code)]`) so any module in
//     the crate that still imports it keeps compiling.
//   - `scrub_urls`: errors from reqwest embed the full request URL ("error sending
//     request for url (https://host/v2/KEY)"). Every error string that reaches a log
//     line from the HTTP path goes through this first.
//   - `classify_rpc_error`: distinguishes rate-limit and range-too-large failures
//     (string matching — providers do not share a structured error format) so the
//     pollers can back off or split a range instead of treating both as a dead
//     connection.

use alloy::providers::Provider;

use crate::transport::RpcTransport;

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

/// Validates an RPC URL's scheme and returns the transport it implies.
/// Accepts ws://, wss://, http:// and https:// (case-insensitive); anything else is a
/// fatal `InvalidUrl` — retrying the same misconfigured URL can never succeed.
pub(crate) fn validate_rpc_url(url: &str) -> Result<RpcTransport, RpcClientError> {
    let u = url.trim().to_ascii_lowercase();
    let ok = ["ws://", "wss://", "http://", "https://"]
        .iter()
        .any(|scheme| u.starts_with(scheme));
    if ok {
        Ok(RpcTransport::detect(url))
    } else {
        Err(RpcClientError::InvalidUrl {
            url: redact_ws_url(url),
            reason: "must start with ws://, wss://, http:// or https://".to_string(),
        })
    }
}

/// Rejects a URL that doesn't start with `ws://` or `wss://`.
///
/// Superseded by `validate_rpc_url` (HTTP transport support). Kept so any module that
/// still imports it keeps compiling.
#[allow(dead_code)]
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
/// Despite the name this works for any scheme (http/https too).
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

/// Replaces every URL embedded in free text with its redacted form.
///
/// Library errors (reqwest, tungstenite, alloy transport) routinely include the full
/// request URL — API key and all — in their Display output. Anything from those
/// errors that reaches a log line or an error message should pass through here.
pub(crate) fn scrub_urls(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut rest = msg;
    while let Some(pos) = rest.find("://") {
        let before = &rest[..pos];
        // Walk back over the scheme's alphabetic characters.
        let scheme_start = before
            .char_indices()
            .rev()
            .find(|(_, c)| !c.is_ascii_alphabetic())
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        out.push_str(&rest[..scheme_start]);

        let after = &rest[pos + 3..];
        let end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | '>' | ','))
            .unwrap_or(after.len());
        let url = &rest[scheme_start..pos + 3 + end];
        out.push_str(&redact_ws_url(url));
        rest = &rest[pos + 3 + end..];
    }
    out.push_str(rest);
    out
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
    let actual = provider.get_chain_id().await.map_err(|e| {
        RpcClientError::ConnectFailed(format!(
            "eth_chainId failed: {}",
            scrub_urls(&e.to_string())
        ))
    })?;
    if actual != expected {
        return Err(RpcClientError::ChainIdMismatch { expected, actual });
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// RPC error classification
// ─────────────────────────────────────────────────────────────────────────────

/// Coarse category of an RPC failure, as far as the pollers care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RpcErrorKind {
    /// The provider is throttling us (HTTP 429 or equivalent). Back off and retry.
    RateLimited,
    /// An eth_getLogs query was too wide or matched too many results. Split the range.
    RangeTooLarge,
    /// Anything else.
    Other,
}

/// Classifies an RPC error message. Providers share no structured error format, so
/// this is substring matching; range errors are checked first because some providers
/// phrase them with words that also look like rate limiting.
pub(crate) fn classify_rpc_error(msg: &str) -> RpcErrorKind {
    let m = msg.to_ascii_lowercase();

    const RANGE_MARKERS: [&str; 10] = [
        "block range",
        "range is too large",
        "range too large",
        "too many blocks",
        "more than 10000",
        "query returned more than",
        "logs matched by query",
        "response size",
        "query timeout exceeded",
        "exceed maximum block range",
    ];
    if RANGE_MARKERS.iter().any(|p| m.contains(p)) {
        return RpcErrorKind::RangeTooLarge;
    }

    const RATE_MARKERS: [&str; 7] = [
        "429",
        "rate limit",
        "rate-limit",
        "ratelimit",
        "too many requests",
        "request limit",
        "throttl",
    ];
    if RATE_MARKERS.iter().any(|p| m.contains(p)) {
        return RpcErrorKind::RateLimited;
    }

    RpcErrorKind::Other
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
    fn redact_works_for_https() {
        assert_eq!(
            redact_ws_url("https://arb-mainnet.g.alchemy.com/v2/KEY"),
            "https://arb-mainnet.g.alchemy.com/<redacted>"
        );
    }

    #[test]
    fn scrub_urls_redacts_embedded_urls() {
        let s =
            scrub_urls("error sending request for url (https://h.example.com/v2/KEY): timed out");
        assert!(!s.contains("KEY"));
        assert!(s.contains("https://h.example.com/<redacted>"));
        assert!(s.contains("timed out"));
    }

    #[test]
    fn scrub_urls_handles_multiple_urls_and_none() {
        let s = scrub_urls("a wss://x.io/k1 b https://y.io/k2?z=1 c");
        assert!(!s.contains("k1") && !s.contains("k2"));
        assert_eq!(scrub_urls("no urls here"), "no urls here");
    }

    #[test]
    fn validate_rpc_url_accepts_all_four_schemes() {
        assert_eq!(
            validate_rpc_url("ws://localhost:8546").unwrap(),
            RpcTransport::Ws
        );
        assert_eq!(
            validate_rpc_url("wss://node.example.com/k").unwrap(),
            RpcTransport::Ws
        );
        assert_eq!(
            validate_rpc_url("http://localhost:8545").unwrap(),
            RpcTransport::Http
        );
        assert_eq!(
            validate_rpc_url("HTTPS://node.example.com/k").unwrap(),
            RpcTransport::Http
        );
    }

    #[test]
    fn validate_rpc_url_rejects_other_schemes_as_fatal() {
        let err = validate_rpc_url("ftp://node.example.com").unwrap_err();
        assert!(matches!(err, RpcClientError::InvalidUrl { .. }));
        assert!(
            err.is_fatal(),
            "an InvalidUrl error must be fatal — retrying the same misconfigured URL can never succeed"
        );
        assert!(validate_rpc_url("node.example.com").is_err());
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
        assert!(err.is_fatal());
    }

    #[test]
    fn classify_detects_rate_limiting() {
        assert_eq!(
            classify_rpc_error("HTTP status 429 Too Many Requests"),
            RpcErrorKind::RateLimited
        );
        assert_eq!(
            classify_rpc_error("daily request limit reached"),
            RpcErrorKind::RateLimited
        );
        assert_eq!(
            classify_rpc_error("Rate limit exceeded"),
            RpcErrorKind::RateLimited
        );
    }

    #[test]
    fn classify_detects_range_too_large() {
        assert_eq!(
            classify_rpc_error("query returned more than 10000 results"),
            RpcErrorKind::RangeTooLarge
        );
        assert_eq!(
            classify_rpc_error("eth_getLogs block range too large, max 500"),
            RpcErrorKind::RangeTooLarge
        );
        assert_eq!(
            classify_rpc_error("Log response size exceeded"),
            RpcErrorKind::RangeTooLarge
        );
    }

    #[test]
    fn classify_range_wins_over_rate_when_both_words_appear() {
        assert_eq!(
            classify_rpc_error("exceed maximum block range (rate limit tier)"),
            RpcErrorKind::RangeTooLarge
        );
    }

    #[test]
    fn classify_other() {
        assert_eq!(
            classify_rpc_error("connection reset by peer"),
            RpcErrorKind::Other
        );
        assert_eq!(classify_rpc_error(""), RpcErrorKind::Other);
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
