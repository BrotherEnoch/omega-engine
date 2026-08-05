// contracts/src/ReconciliationRotationGate.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/**
 * @title ReconciliationRotationGate
 * @notice Transaction-scoped and persistent reconciliation controls for the omega-engine.
 * @dev Leverages Solidity 0.8.24 transient storage (EIP-1153) for flash-loan tracking,
 *      which auto-clears at the end of the transaction — eliminating stale-state bugs.
 *      Addresses audit findings: C1 (pre-rotation), C2 (vault/orchestrator accounting),
 *      C3 (dust), C4 (ZK binding).
 *
 * CHANGES vs the previously-circulated draft of this library:
 *   1. FIXED (would not compile) — `requirePreRotationReconciliation`,
 *      `requireVaultReconciliation`, and `requirePostExecutionReconciliation` were declared
 *      `view` but each contains `emit`. Solidity treats event emission as a state mutation
 *      for view-purity purposes and rejects it at compile time. All three are now ordinary
 *      (non-view) internal functions.
 *   2. ADDED — `pendingVaultProofsCount(state)` view helper so a contract can expose its own
 *      gate's pending-proof count to a *different* contract (e.g. Vault exposing it to
 *      Orchestrator). Named distinctly from the `pendingVaultProofs` struct field it reads —
 *      giving a library function the same name as a struct field it's called on causes an
 *      ambiguous-lookup compile error. The gate only ever sees its own contract's
 *      storage/transient state — a caller in another contract can't reach into it directly,
 *      so the owning contract needs a getter built on top of this.
 *   3. NOTE for integrators — `using Gate for Gate.GateState;` alone is NOT sufficient to
 *      call `requireVaultReconciliation` / `requirePostExecutionReconciliation` as
 *      `token.requireVaultReconciliation(...)`, since their first parameter is `IERC20`, not
 *      `GateState`. You additionally need `using Gate for IERC20;` in the consuming contract.
 */
library ReconciliationRotationGate {
    /*//////////////////////////////////////////////////////////////
                             CUSTOM ERRORS
    //////////////////////////////////////////////////////////////*/
    error Gate_PendingFlashloans(uint256 active);
    error Gate_PendingVaultProofs(uint256 pending);
    error Gate_VaultBalanceShortfall(uint256 expected, uint256 actual);
    error Gate_OutputDeltaMismatch(uint256 expected, uint256 actual, uint256 tolerance);
    error Gate_ProofInputsNotBound(bytes32 blueprintHash);
    error Gate_ProofInputsMismatch(bytes32 blueprintHash, bytes32 expected, bytes32 provided);
    error Gate_ProofInputsAlreadyBound(bytes32 blueprintHash);
    error Gate_NoActiveFlashloans();
    error Gate_NoPendingProofs();
    error Gate_ZeroAddress();

    /*//////////////////////////////////////////////////////////////
                                EVENTS
    //////////////////////////////////////////////////////////////*/
    event ReconciliationPassed(bytes32 indexed operationId, uint256 timestamp);
    event ReconciliationFailed(bytes32 indexed operationId, bytes reason, uint256 timestamp);
    event VaultReconciled(address indexed vault, uint256 actual, uint256 expected);
    event ProofInputsBound(bytes32 indexed blueprintHash, bytes32 publicInputsHash);

    /*//////////////////////////////////////////////////////////////
                            PERSISTENT STATE
    //////////////////////////////////////////////////////////////*/
    struct GateState {
        uint256 pendingVaultProofs;
        mapping(bytes32 => bytes32) boundProofInputs; // blueprintHash => publicInputsHash
        mapping(bytes32 => bool) proofInputsBound;
    }

    /*//////////////////////////////////////////////////////////////
                        TRANSIENT STATE (EIP-1153)
    //////////////////////////////////////////////////////////////*/
    /// @dev Auto-clears at end of tx — perfect for flash-loan scope tracking. Transient
    ///      storage is scoped per-contract-address (like regular storage), so two different
    ///      contracts using this same library and the same constant slot do NOT collide.
    ///      NOTE: inline assembly only accepts direct numeric-literal constants (or
    ///      references to such), not arbitrary compile-time expressions — a `keccak256(...)
    ///      - 1` expression assigned to a constant, as in an earlier draft of this library,
    ///      fails to compile ("Only direct number constants ... are supported by inline
    ///      assembly"). The value below is the precomputed literal for
    ///      `keccak256("omega.gate.transient.flashloans") - 1` (the "- 1" follows the
    ///      unstructured-storage convention of avoiding a slot that's the exact hash of a
    ///      known preimage).
    bytes32 private constant TRANSIENT_FLASHLOAN_SLOT =
        0x386aac88e71f905ec4a10ff22a18cccdd43d9cb7b4844be1955c50b481f23c0c;

    /*//////////////////////////////////////////////////////////////
                     PRE-ROTATION RECONCILIATION
    //////////////////////////////////////////////////////////////*/
    /**
     * @notice Ensures no mid-flight operations exist in THIS contract before key/strategy
     *         rotation. Does not — cannot — see another contract's gate state; if a rotation
     *         also depends on another contract's pending state (e.g. Orchestrator rotation
     *         caring about Vault's pending proofs), the caller must additionally check that
     *         contract's own exposed getter (see `pendingVaultProofs` below).
     * @dev MUST be called inside `finalizeKeyRotation` and strategy upgrades.
     */
    function requirePreRotationReconciliation(
        GateState storage state,
        bytes32 operationId
    ) internal {
        uint256 activeFlashloans = _getTransientFlashloans();
        if (activeFlashloans > 0) {
            emit ReconciliationFailed(operationId, "PENDING_FLASHLOANS", block.timestamp);
            revert Gate_PendingFlashloans(activeFlashloans);
        }

        if (state.pendingVaultProofs > 0) {
            emit ReconciliationFailed(operationId, "PENDING_VAULT_PROOFS", block.timestamp);
            revert Gate_PendingVaultProofs(state.pendingVaultProofs);
        }

        emit ReconciliationPassed(operationId, block.timestamp);
    }

    /*//////////////////////////////////////////////////////////////
                      VAULT FINANCIAL RECONCILIATION
    //////////////////////////////////////////////////////////////*/
    /**
     * @notice Ensures the calling contract's actual token balance covers `expectedBalance`.
     * @dev Called before any `releaseProfit` / fee distribution. Requires
     *      `using Gate for IERC20;` at the call site (see library-level note above).
     */
    function requireVaultReconciliation(
        IERC20 token,
        uint256 expectedBalance,
        bytes32 operationId
    ) internal {
        uint256 actualBalance = token.balanceOf(address(this));
        if (actualBalance < expectedBalance) {
            emit ReconciliationFailed(operationId, "VAULT_SHORTFALL", block.timestamp);
            revert Gate_VaultBalanceShortfall(expectedBalance, actualBalance);
        }

        emit VaultReconciled(address(this), actualBalance, expectedBalance);
        emit ReconciliationPassed(operationId, block.timestamp);
    }

    /*//////////////////////////////////////////////////////////////
                    POST-EXECUTION DELTA RECONCILIATION
    //////////////////////////////////////////////////////////////*/
    /**
     * @notice Verifies actual token balance meets an expected output threshold within
     *         `tolerance`. Called after strategy execution, before forwarding profit onward.
     * @dev Requires `using Gate for IERC20;` at the call site.
     */
    function requirePostExecutionReconciliation(
        IERC20 token,
        uint256 expectedOutput,
        uint256 tolerance,
        bytes32 operationId
    ) internal {
        uint256 actualOutput = token.balanceOf(address(this));
        if (actualOutput + tolerance < expectedOutput) {
            emit ReconciliationFailed(operationId, "OUTPUT_DELTA", block.timestamp);
            revert Gate_OutputDeltaMismatch(expectedOutput, actualOutput, tolerance);
        }

        emit ReconciliationPassed(operationId, block.timestamp);
    }

    /*//////////////////////////////////////////////////////////////
                        ZK PROOF INPUT BINDING
    //////////////////////////////////////////////////////////////*/
    /**
     * @notice Binds a blueprint hash to its ZK proof public inputs, ONE TIME ONLY.
     * @dev Called at deposit/submission time; verified again at release time. Reverts on a
     *      second binding attempt for the same blueprintHash — a blueprint's committed
     *      economics (amount, recipient, whatever the caller folded into publicInputsHash)
     *      must not be silently replaced after a proof may already have been produced or
     *      verified against the first binding. Callers that need to update an amount must
     *      mint a new blueprintHash instead of rebinding an existing one.
     */
    function bindProofInputs(
        GateState storage state,
        bytes32 blueprintHash,
        bytes32 publicInputsHash
    ) internal {
        if (blueprintHash == bytes32(0) || publicInputsHash == bytes32(0)) {
            revert Gate_ZeroAddress();
        }
        if (state.proofInputsBound[blueprintHash]) {
            revert Gate_ProofInputsAlreadyBound(blueprintHash);
        }
        state.boundProofInputs[blueprintHash] = publicInputsHash;
        state.proofInputsBound[blueprintHash] = true;
        emit ProofInputsBound(blueprintHash, publicInputsHash);
    }

    /**
     * @notice Verifies that a proof's public inputs match the on-chain binding.
     */
    function requireProofInputMatch(
        GateState storage state,
        bytes32 blueprintHash,
        bytes32 publicInputsHash
    ) internal view {
        if (!state.proofInputsBound[blueprintHash]) {
            revert Gate_ProofInputsNotBound(blueprintHash);
        }
        bytes32 expected = state.boundProofInputs[blueprintHash];
        if (expected != publicInputsHash) {
            revert Gate_ProofInputsMismatch(blueprintHash, expected, publicInputsHash);
        }
    }

    /*//////////////////////////////////////////////////////////////
                  TRANSIENT FLASHLOAN TRACKING (EIP-1153)
    //////////////////////////////////////////////////////////////*/
    function trackFlashloanOpen() internal {
        uint256 current = _getTransientFlashloans();
        _setTransientFlashloans(current + 1);
    }

    function trackFlashloanClose() internal {
        uint256 current = _getTransientFlashloans();
        if (current == 0) revert Gate_NoActiveFlashloans();
        _setTransientFlashloans(current - 1);
    }

    /*//////////////////////////////////////////////////////////////
                     TRANSIENT VAULT PROOF TRACKING
    //////////////////////////////////////////////////////////////*/
    function trackVaultProofOpen(GateState storage state) internal {
        state.pendingVaultProofs++;
    }

    function trackVaultProofClose(GateState storage state) internal {
        if (state.pendingVaultProofs == 0) revert Gate_NoPendingProofs();
        state.pendingVaultProofs--;
    }

    /*//////////////////////////////////////////////////////////////
                         CROSS-CONTRACT VIEW HELPERS
    //////////////////////////////////////////////////////////////*/
    /// @dev Intended to back a `public`/`external view` getter on the owning contract so
    ///      OTHER contracts (which cannot read this private-mapping-containing struct
    ///      directly) can check it, e.g. Orchestrator checking Vault's pending proof count
    ///      before finalizing a key rotation.
    function pendingVaultProofsCount(GateState storage state) internal view returns (uint256) {
        return state.pendingVaultProofs;
    }

    /// @dev Exposes the transient flashloan-in-progress counter for verification/tooling
    ///      getters on the owning contract (e.g. Orchestrator's `activeFlashloanCount()`,
    ///      used by the revert-path regression spec to assert the counter always returns to
    ///      zero after any completed transaction).
    function activeFlashloanCount() internal view returns (uint256) {
        return _getTransientFlashloans();
    }

    /*//////////////////////////////////////////////////////////////
                         TRANSIENT HELPERS
    //////////////////////////////////////////////////////////////*/
    function _getTransientFlashloans() private view returns (uint256 value) {
        assembly {
            value := tload(TRANSIENT_FLASHLOAN_SLOT)
        }
    }

    function _setTransientFlashloans(uint256 value) private {
        assembly {
            tstore(TRANSIENT_FLASHLOAN_SLOT, value)
        }
    }
}
