// contracts/src/verifiers/SP1StarkVerifierAdapter.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {IStarkVerifier} from "../OmegaVault.sol";

/// @dev Minimal interface for SP1's real on-chain verifier / gateway. Confirmed directly
///      against succinctlabs/sp1-contracts (ISP1Verifier.sol) via live search at the time
///      this file was written -- not recalled from training data, given how load-bearing
///      getting this exactly right is. verifyProof takes exactly these three arguments, in
///      this order, and returns NOTHING: success is "call did not revert", failure is a
///      revert (either WrongVerifierSelector if routed to the wrong sub-verifier, or an
///      invalid-proof rejection from inside the real verifier). This is intentionally NOT
///      ABI-identical to OmegaVault's own IStarkVerifier -- bridging that gap is this
///      entire file's job.
///      Both SP1VerifierGateway and each individual SP1Verifier implement this same
///      interface per Succinct's own docs, so this adapter works unchanged whichever one
///      you point it at.
interface ISP1Verifier {
    function verifyProof(
        bytes32 programVKey,
        bytes calldata publicValues,
        bytes calldata proofBytes
    ) external view;
}

/// @title SP1StarkVerifierAdapter
/// @notice Implements OmegaVault's IStarkVerifier interface by wrapping a real SP1
///         verifier/gateway, so OmegaVault.submitProof() can call this unchanged.
///
/// TWO REAL DECISIONS MADE HERE, FLAGGED RATHER THAN MADE SILENTLY:
///
///   1. BOOL vs REVERT: SP1's verifyProof() reverts on failure; IStarkVerifier.verify() must
///      return false instead (per how OmegaVault.submitProof() checks its return value and
///      raises its OWN InvalidProof() error). This adapter uses try/catch to translate a
///      revert into `return false`, so it's OmegaVault's InvalidProof() that ultimately
///      surfaces to callers, not SP1's internal revert reason. That seemed like the more
///      correct choice -- the caller-facing contract's own error semantics should win, not
///      a foreign error type leaking through -- but it IS a choice. If you'd rather see
///      SP1's specific revert reason directly (useful mid-integration, for debugging why a
///      proof was rejected), say so and I'll remove the try/catch and let it propagate
///      instead.
///
///   2. THE `proof` ENCODING: IStarkVerifier.verify() has one opaque `bytes proof` field,
///      but SP1 needs publicValues and proofBytes as two SEPARATE parameters -- it hashes
///      publicValues internally and checks that digest against one embedded in proofBytes;
///      they are not concatenable or otherwise mergeable into one blob without an explicit
///      split. This adapter defines `proof` as `abi.encode(bytes publicValues, bytes
///      proofBytes)`. Whatever off-chain relayer calls OmegaVault.submitProof() MUST encode
///      it this way -- not just concatenate the two byte arrays -- or every call here
///      reverts at the length/decode check below before ever reaching SP1's own verifier.
///
/// WHAT THIS FILE DOES NOT AND CANNOT DECIDE, BECAUSE THE THING IT DEPENDS ON DOESN'T EXIST
/// YET (per this conversation -- no SP1 program has been written):
///
///   Your actual SP1 program (the Rust code that runs inside the zkVM and gets compiled to
///   an ELF) MUST commit `(blueprintHash, publicInputsHash)` as its first two public-output
///   words, in exactly that order, via SP1's `sp1_zkvm::io::commit(...)` -- this adapter
///   checks the decoded publicValues against the blueprintHash/publicInputsHash OmegaVault
///   passed in, BEFORE calling into SP1's cryptographic verification, so a mismatch fails
///   fast with a clear reason. Your program is free to commit additional public outputs
///   after those first two words (the check below only reads a 64-byte prefix); what
///   computation the rest of the program actually performs and attests to -- presumably
///   proving `netProfit` was computed correctly off-chain -- is real circuit design I have
///   no basis to write without knowing what that computation is.
///
///   `programVKey` is an immutable set at deploy time, NOT something I can supply. It's a
///   hash Succinct's own tooling derives from your compiled program's actual ELF binary
///   (`cargo prove vkey -elf <path>`, or `client.setup(ELF)` via the sp1-sdk crate, per their
///   docs). It does not exist until your program exists and is compiled.
///
///   `_sp1Verifier` (constructor arg): point this at Succinct's canonical
///   SP1_VERIFIER_GATEWAY address for your target chain (their own docs recommend the
///   gateway over a single fixed-version verifier, since it auto-routes by SP1 version) --
///   see https://docs.succinct.xyz/docs/sp1/verification/contract-addresses for the current
///   per-chain address. I am not asserting a specific address here for the same reason I
///   haven't asserted Aave/Balancer/Morpho addresses anywhere else in this conversation --
///   it's exactly the kind of external, chain-specific, potentially-stale-in-my-training-data
///   value I keep declining to fabricate, and this one is no different.
contract SP1StarkVerifierAdapter is IStarkVerifier {
    ISP1Verifier public immutable sp1Verifier;
    bytes32      public immutable programVKey;

    error ZeroAddress();
    error PublicValuesTooShort(uint256 actual, uint256 required);

    constructor(address _sp1Verifier, bytes32 _programVKey) {
        if (_sp1Verifier == address(0)) revert ZeroAddress();
        sp1Verifier = ISP1Verifier(_sp1Verifier);
        programVKey = _programVKey;
    }

    /// @notice Conforms to OmegaVault's IStarkVerifier interface.
    /// @param  proof             abi.encode(bytes publicValues, bytes proofBytes) -- see
    ///                           file header, decision 2, for why this specific encoding.
    /// @param  blueprintHash     Passed through from OmegaVault -- must match the SP1
    ///                           program's first committed public-output word.
    /// @param  publicInputsHash  Passed through from OmegaVault -- must match the SP1
    ///                           program's second committed public-output word.
    function verify(
        bytes calldata proof,
        bytes32 blueprintHash,
        bytes32 publicInputsHash
    ) external view override returns (bool) {
        (bytes memory publicValues, bytes memory proofBytes) =
            abi.decode(proof, (bytes, bytes));

        if (publicValues.length < 64)
            revert PublicValuesTooShort(publicValues.length, 64);

        // Enforce the program<->adapter ABI contract from the file header BEFORE spending
        // gas on SP1's actual cryptographic verification -- a mismatch here means either a
        // stale/wrong proof was supplied or the SP1 program itself doesn't conform to the
        // committed-output contract this adapter expects, and either way it should fail
        // fast with a clear reason rather than an opaque SP1-side rejection.
        (bytes32 committedBlueprintHash, bytes32 committedPublicInputsHash) =
            abi.decode(publicValues, (bytes32, bytes32));
        if (committedBlueprintHash != blueprintHash) return false;
        if (committedPublicInputsHash != publicInputsHash) return false;

        // SP1's verifyProof() reverts on failure, returns nothing on success -- translate to
        // the bool IStarkVerifier expects. See file header, decision 1, for why try/catch
        // rather than letting the revert propagate.
        try sp1Verifier.verifyProof(programVKey, publicValues, proofBytes) {
            return true;
        } catch {
            return false;
        }
    }
}