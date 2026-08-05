// crates/omega-rpc/src/chainlink_agg.rs
//
// AggregatorV3 via SolCall + Provider::call — no generic contract Instance.
//
// ## Why this shape, not sol!(#[sol(rpc)]) + Instance::new
//
// An earlier revision used `AggregatorV3Interface::new(feed, provider)`,
// which requires `provider: P where P: Provider` — a generic, Sized type
// parameter. `OmegaRpcClient::get_or_connect()` returns `Arc<dyn
// Provider>`, and `dyn Provider` is not `Sized`; the blanket `impl<U:
// Provider> Provider for Arc<U>` needs the inner `U` to itself satisfy
// `Provider`, which a bare trait object doesn't for this purpose.
// Confirmed via `cargo check -p omega-rpc` this session (E0277/E0599).
//
// Fixed by skipping the generic Instance wrapper entirely: `sol!`
// without `#[sol(rpc)]` still generates `SolCall`-implementing structs
// (`decimalsCall`, `latestRoundDataCall`) that only need
// `.abi_encode()`/`::abi_decode_returns()` — no generic `Provider`
// bound. Combined with `provider.call(&tx)`, a `&self` trait method
// (same object-safe category as `get_logs`/`get_block_by_number`,
// already proven to work on `Arc<dyn Provider>` in client.rs), this
// avoids the `Sized` problem entirely.
//
// ## Confirmed this revision (via real cargo check -p omega-rpc output,
// not guessed)
//
//   - `TransactionRequest::with_to`/`::with_input` exist (from
//     `alloy-network-0.3.6`'s `TransactionBuilder` trait) — CONFIRMED
//     they exist as methods, per the compiler's own error text quoting
//     their real signatures. NOT yet confirmed: the correct import path
//     to bring that trait into scope. The compiler's first `help:`
//     suggestion (`alloy::alloy_network::TransactionBuilder`) did not
//     resolve (E0432) — that suggestion is generated from the trait's
//     defining crate name, not a verified facade path, and was wrongly
//     treated as confirmed in an earlier revision of this file. See the
//     correction note at the top of the import list below.
//   - `decimalsCall::abi_decode_returns` does NOT yield a bare `u8` for
//     Solidity's unnamed `returns (uint8)` — it yields a generated
//     `decimalsReturn` wrapper struct. Since the Solidity declaration
//     gave the return value no name, alloy's codegen convention numbers
//     it positionally: `._0`. Confirmed by the compiler's own error
//     (E0308: "expected `u8`, found `decimalsReturn`").
//   - `alloy::providers::Provider` does not need to be explicitly
//     imported to call `.call()` on a `provider: Arc<dyn Provider>`
//     value — trait-object method dispatch doesn't require the trait in
//     scope the way generic/blanket-impl dispatch does. Removed as
//     genuinely unused (compiler warning, not a guess).
//
// OmegaRpcClient::fetch_chainlink_round is defined ONLY in this file.
// Do not also define it in client.rs (E0592 duplicate inherent method).

// ## Correction (this revision): the compiler's own `help:` suggestion
// from the prior attempt (`use alloy::alloy_network::TransactionBuilder;`)
// did NOT resolve (E0432) — that was a mechanically-generated hint based
// on the trait's *defining* crate name, not a verified public path
// through the `alloy` facade crate, and treating it as confirmed was a
// mistake worth naming plainly rather than quietly correcting. Trying
// `alloy::network::TransactionBuilder` instead — alloy's facade crate
// conventionally re-exports `alloy_network` as `network`, but this is
// informed, not confirmed, the same epistemic status the wrong path had
// last attempt. If this also fails to resolve, the real fix is whatever
// `cargo doc -p alloy --open` (or `cargo tree -p alloy -e features`)
// shows as the actual re-export path — a second guess isn't a
// substitute for checking that directly if this one is also wrong.
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, Bytes, I256, U256};
use alloy::rpc::types::TransactionRequest;
use alloy::sol;
use alloy::sol_types::SolCall;

use crate::client::OmegaRpcClient;

// No #[sol(rpc)] — only Call/Return types are needed, not a generic
// contract Instance (see module-level note above).
sol! {
    function latestRoundData()
        external
        view
        returns (
            uint80 roundId,
            int256 answer,
            uint256 startedAt,
            uint256 updatedAt,
            uint80 answeredInRound
        );

    function decimals() external view returns (uint8);
}

/// One successful AggregatorV3 read, scaled to USD f64.
#[derive(Debug, Clone)]
pub struct ChainlinkRound {
    pub token: String,
    pub price_usd: f64,
    pub updated_at: u64,
    pub block_number: u64,
    pub round_id: u64,
}

/// Decode `answer` with feed `decimals` → USD. Rejects non-positive / non-finite.
pub fn scale_chainlink_answer(answer: I256, decimals: u8) -> Option<f64> {
    if answer <= I256::ZERO {
        return None;
    }
    let mag: U256 = answer.into_raw();
    let raw = mag.to_string().parse::<f64>().ok()?;
    let price = raw / 10_f64.powi(decimals as i32);
    (price.is_finite() && price > 0.0).then_some(price)
}

impl OmegaRpcClient {
    /// Rate-limited `latestRoundData` + `decimals` for one feed, via raw
    /// ABI-encoded `eth_call` rather than a generic contract Instance —
    /// see this file's module-level note for why.
    pub async fn fetch_chainlink_round(
        &self,
        feed: Address,
        token: &str,
    ) -> anyhow::Result<ChainlinkRound> {
        self.gated_read(None, || async move {
            let provider = self.get_or_connect().await?;

            // ── decimals() ──────────────────────────────────────────────
            let dec_data: Bytes = decimalsCall {}.abi_encode().into();
            let dec_tx = TransactionRequest::default()
                .with_to(feed)
                .with_input(dec_data);
            let dec_raw = provider
                .call(&dec_tx)
                .await
                .map_err(|e| anyhow::anyhow!("chainlink decimals eth_call failed: {e}"))?;
            let decimals: u8 = decimalsCall::abi_decode_returns(&dec_raw, true)
                .map_err(|e| anyhow::anyhow!("chainlink decimals decode failed: {e}"))?
                ._0;

            // ── latestRoundData() ───────────────────────────────────────
            let round_data: Bytes = latestRoundDataCall {}.abi_encode().into();
            let round_tx = TransactionRequest::default()
                .with_to(feed)
                .with_input(round_data);
            let round_raw = provider
                .call(&round_tx)
                .await
                .map_err(|e| anyhow::anyhow!("chainlink latestRoundData eth_call failed: {e}"))?;
            let round = latestRoundDataCall::abi_decode_returns(&round_raw, true)
                .map_err(|e| anyhow::anyhow!("chainlink latestRoundData decode failed: {e}"))?;

            // Named multi-return field access. latestRoundData's five
            // return values ARE named in the Solidity declaration
            // (roundId, answer, ...), unlike decimals()'s unnamed single
            // return above — so these should resolve as named fields,
            // not positionally. Still worth confirming on the next
            // compile pass, same as everything else in this file was
            // confirmed rather than assumed.
            let answer = round.answer;
            let updated_at: u64 = round
                .updatedAt
                .try_into()
                .map_err(|_| anyhow::anyhow!("updatedAt does not fit u64"))?;

            let price_usd = scale_chainlink_answer(answer, decimals).ok_or_else(|| {
                anyhow::anyhow!("chainlink answer non-positive or non-finite for {token}")
            })?;

            // Proven path — same get_block_by_number(Latest, false) call
            // fetch_fee_snapshot already uses successfully.
            let block = provider
                .get_block_by_number(alloy::eips::BlockNumberOrTag::Latest, false)
                .await
                .map_err(|e| anyhow::anyhow!("eth_getBlockByNumber failed: {e}"))?
                .ok_or_else(|| anyhow::anyhow!("latest block not found"))?;
            let block_number = block.header.number;

            if round.answeredInRound < round.roundId {
                tracing::warn!(
                    token,
                    "chainlink answeredInRound < roundId — possible incomplete round"
                );
            }

            let round_id = u64::try_from(round.roundId).unwrap_or(u64::MAX);

            Ok(ChainlinkRound {
                token: token.to_owned(),
                price_usd,
                updated_at,
                block_number,
                round_id,
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_rejects_zero_answer() {
        assert_eq!(scale_chainlink_answer(I256::ZERO, 8), None);
    }

    #[test]
    fn scale_rejects_negative_answer() {
        let neg = I256::try_from(-1_i64).unwrap();
        assert_eq!(scale_chainlink_answer(neg, 8), None);
    }

    #[test]
    fn scale_applies_decimals() {
        // 180000000000 with 8 decimals = 1800.0
        let answer = I256::try_from(180_000_000_000_i64).unwrap();
        let price = scale_chainlink_answer(answer, 8).unwrap();
        assert!((price - 1800.0).abs() < 1e-6);
    }
}
