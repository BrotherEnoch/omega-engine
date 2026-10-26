# SP1 proof safety status

## Status: proof production disabled

The guest currently panics before producing public values. The host utility only
derives a verification-key hash and cannot generate proofs. The insecure development
no-op guest feature and fabricated sample attestation were removed.

This fail-closed state is intentional. The former guest accepted any non-zero
signature bytes without verifying them and accepted caller-supplied blueprint and
public-input hashes without deriving them from the executed trade. The sample
profit arithmetic also omitted execution fees, flash-loan costs, token decimals,
and strategy-specific route semantics. A proof from that circuit must not
authorize profit release or be used for live trading.

## Required before re-enabling proof production

1. Define the canonical, domain-separated oracle message, including chain, token
   pair, price, timestamp/expiry, and replay protection. Pin the actual public
   verification key in the guest build. Verify the signature inside the guest;
   host-side verification alone is not part of the STARK proof.
2. Recompute the Orchestrator blueprint hash from the exact calldata and bind
   the proof's public values to the Vault's computePublicInputsHash inputs.
   The proof must show that the claimed trade data corresponds to the calldata
   that the Orchestrator executed.
3. Prove conservative net profit using the actual strategy/route semantics,
   output-token decimals, swap fees, flash-loan premiums, and gas/L1 data costs.
   Reject unsupported strategy types rather than applying a generic price ratio.
4. Add independent positive and negative vectors and cross-check the guest,
   host encoder, verifier adapter, and deployed Vault. Rebuild the ELF and update
   the immutable on-chain PROGRAM_VKEY only after those checks and review.

The existing PROGRAM_VKEY values are not production approval. Re-enabling the
guest requires a new reviewed implementation and a newly derived vkey.

