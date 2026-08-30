# docs/ZK_Gaps_Closed.md

## Previously open → status

| Gap | Resolution |
|-----|------------|
| No on-chain `submitProof` broadcast | **Closed:** L7 keeper encodes → `sign_call_gwei` → `OmegaRpcClient::submit_signed_raw_tx` |
| Binding vs independent keccak | **Closed:** `matches_independent_keccak_oracle` golden vector |
| `ProofCommitment` vs `public_inputs_hash` confusion | **Closed:** commitment.rs docs state Vault uses only publicInputsHash |
| Changelog “no submitProof path” | **Closed:** comments updated |
| Winterfell vs deployed `IStarkVerifier` | **Documented product boundary** — off-chain Winterfell verify is the admission gate; on-chain verifier must accept the same proof bytes or proofs will revert at `submitProof`. Deploy matching `IStarkVerifier` or treat T1 software proofs as sim/dev until then. |
| Shadow skip | Unchanged by design (`allow_skip_in_shadow` when phase 0) |

## New APIs

### `KeyManagerTransactionSigner::sign_call` / `sign_call_gwei`
EIP-1559 sign to arbitrary `to` + calldata (Vault, not Orchestrator).

### `OmegaRpcClient::submit_signed_raw_tx`
Hex raw tx → dedup → `eth_sendRawTransaction` → `[u8; 32]` hash.

## Env (keeper)

| Var | Default | Role |
|-----|---------|------|
| `OMEGA_VAULT_SUBMIT_NONCE` | 0 | Starting nonce for Vault txs |
| `OMEGA_VAULT_SUBMIT_GAS` | 800000 | Gas limit |
| `OMEGA_VAULT_SUBMIT_PRIORITY_GWEI` | 1 | Priority fee |
| `OMEGA_VAULT_SUBMIT_MAX_FEE_GWEI` | 50 | Max fee |

## Apply

```bash
cp src/signer.rs    crates/omega-execution/src/signer.rs
cp src/client.rs    crates/omega-rpc/src/client.rs
cp src/binding.rs   crates/omega-zk/src/binding.rs
cp src/commitment.rs crates/omega-zk/src/commitment.rs
cp patches/main.rs  src/main.rs

cargo test -p omega-zk -- matches_independent_keccak
cargo test -p omega-execution -- sign_call
cargo check --workspace
```

## Remaining operational risks

1. **Nonce:** local atomic; set `OMEGA_VAULT_SUBMIT_NONCE` from `eth_getTransactionCount` at deploy or risk replacement/underpriced txs.
2. **On-chain verifier format:** if deployed `IStarkVerifier` is not Winterfell-compatible, `submitProof` reverts after successful broadcast.
3. **Phase 0:** shadow skip may still omit proofs when configured.
