// crates\omega-control-contracts\src\ws.rs
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
pub const WS_CHANNEL_CAPACITY: usize = 512;
pub const AUTHED_LIMIT_PER_MINUTE: u32 = 300;
pub const ANON_LIMIT_PER_MINUTE: u32 = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WsEvent {
    HealthTransition {
        layer: String,
        from: String,
        to: String,
        reason: String,
        timestamp: DateTime<Utc>,
    },
    ModelPauseChanged {
        paused: bool,
        timestamp: DateTime<Utc>,
    },
    BlacklistReloaded {
        entry_count: usize,
        timestamp: DateTime<Utc>,
    },
    ConfigReloaded {
        timestamp: DateTime<Utc>,
    },
    CeilingEscalation {
        consecutive_hits: u64,
        paused: bool,
        timestamp: DateTime<Utc>,
    },
    // ── Trading-engine telemetry (bridged from OmegaEvent via obs_bridge) ──
    GasModelReverted {
        checkpoint_version: u64,
        win_rate: f64,
        sample_count: u64,
        timestamp: DateTime<Utc>,
    },
    GasModelCeilingEscalation {
        feature_key: String,
        ceiling_hit_count: u64,
        timestamp: DateTime<Utc>,
    },
    EmergencyBundleSkipped {
        blueprint_hash: String,
        emergency_fee_gwei: f64,
        reason: String,
        timestamp: DateTime<Utc>,
    },
    ProfitSplit {
        blueprint_hash: String,
        pil_share_wei: String,
        dao_fee_wei: String,
        timestamp: DateTime<Utc>,
    },
    LaReorgRisk {
        tx_hash: String,
        orphaned_block: u64,
        reorg_depth: u64,
        timestamp: DateTime<Utc>,
    },
    // CHANGE: `profit_net_eth: f64` -> `profit_net_wei: String`. This is a
    // financial event of the same character as `ProfitSplit` right above it —
    // ProfitSplit already correctly avoids f64 for wei-precision amounts
    // (18-decimal token values routinely exceed what f64 can represent
    // exactly; f64's exact-integer range tops out around 9e15, and realistic
    // wei amounts blow past that constantly). BlueprintConfirmed was the one
    // inconsistent sibling still using a lossy float for the same category of
    // data. BREAKING: field renamed and retyped, not just a value change —
    // any consumer reading `profit_net_eth` needs updating to parse
    // `profit_net_wei` as a decimal string instead.
    BlueprintConfirmed {
        blueprint_hash: String,
        strategy_id: String,
        block_number: u64,
        profit_net_wei: String,
        timestamp: DateTime<Utc>,
    },
}

/// NOTE: send this over `wss://` only. The auth token here is equivalent in
/// sensitivity to `CONTROL_PLANE_BEARER_SECRET` (see the earlier `.env`
/// review in this project) — sent as a plaintext frame after connection
/// rather than a header, which is fine over TLS and a real leak over plain
/// `ws://`. Nothing in this type can enforce which transport it's sent over;
/// that's a deployment-configuration concern, not something fixable here —
/// flagging it rather than silently assuming it's already handled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsClientFrame {
    Auth { token: String },
}

/// NOTE: `AuthFailed` carrying `rate_limit`/`window_secs` looks odd at first —
/// a rejected client doesn't have an active session with limits to report —
/// but reads as intentional: telling even a rejected/anonymous client what
/// the anonymous-tier limit is (e.g. so a UI can show "100/min without
/// login, log in for 300/min"). Left unchanged; noting the reasoning so it
/// doesn't look like a copy-paste artifact to the next person reading this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsAuthFrame {
    AuthOk { rate_limit: u32, window_secs: u64 },
    AuthFailed { rate_limit: u32, window_secs: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blueprint_confirmed_uses_wei_string_like_profit_split() {
        let ev = WsEvent::BlueprintConfirmed {
            blueprint_hash: "0xabc".into(),
            strategy_id: "simple_arb".into(),
            block_number: 12345,
            profit_net_wei: "1500000000000000000".into(), // 1.5 ETH, exact
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"profit_net_wei\":\"1500000000000000000\""));
        assert!(!json.contains("profit_net_eth"));
    }

    #[test]
    fn profit_split_and_blueprint_confirmed_agree_on_precision_strategy() {
        let profit_split = WsEvent::ProfitSplit {
            blueprint_hash: "0x1".into(),
            pil_share_wei: "950000000000000000".into(),
            dao_fee_wei: "50000000000000000".into(),
            timestamp: Utc::now(),
        };
        let blueprint_confirmed = WsEvent::BlueprintConfirmed {
            blueprint_hash: "0x1".into(),
            strategy_id: "la".into(),
            block_number: 1,
            profit_net_wei: "1000000000000000000".into(),
            timestamp: Utc::now(),
        };
        // Both must round-trip as strings, not JSON numbers, for wei-precision fields.
        let s1 = serde_json::to_value(&profit_split).unwrap();
        let s2 = serde_json::to_value(&blueprint_confirmed).unwrap();
        assert!(s1["pil_share_wei"].is_string());
        assert!(s2["profit_net_wei"].is_string());
    }
}
