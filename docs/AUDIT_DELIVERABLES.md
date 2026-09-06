docs/AUDIT_DELIVERABLES.md
OmegaEngine Control-Point Audit Deliverables
Date: 2026-09-05
Scope: Full control-point audit of BrotherEnoch/omega-engine with repairs applied this session.

1. Missing control points (found)
ID

Control point

Status after repair

M1

NonceRegistry never advanced after accept/inclusion

FIXED — record_processed on execute Ok + Stage-7 inclusion

M2

ConfirmationResult lacked strategy_id / nonce / profit

FIXED — carried via BundlePayload (local metadata)

M3

Kill switch Stage-7 used coarse "global" key

FIXED — per-strategy scope

M4

AccountExposureTracker no release on success

FIXED — release() on execute Ok

M5

Max exposure silent 1 ETH placeholder in live phases

FIXED — fail-closed if unset when phase ≥ 1

M6

SA / MSA / MEV not registered in L13

FIXED — registered from IntegrityRegistry when present

M7

Strategy nonces started at 0 (fails check 15 vs latest=0)

FIXED — start at 1

M8

PositionRegistry has no live writer

RESIDUAL — requires operator watchlist/scanner (documented)

M9

On-chain realized P&L into kill switch

RESIDUAL — expected profit used as proxy

2. Broken control points (found + repaired)
ID

Issue

Repair

B1

Check 15 ineffective / permanent lock-out

NonceRegistry advance + nonces start at 1

B2

Stage-7 KS could not attribute outcomes

ConfirmationResult metadata

B3

Exposure overcount until expiry only

release on success

3. Disconnected control points
ID

Issue

Repair

D1

NonceRegistry constructed but never called advance

Wired execute Ok + Stage-7

D2

IntegrityRegistry fields assumed, not verified

Verified StrategyEntry shape; comments closed

D3

ProductionIntegrationPlan outdated (12 gaps all open)

Rewrite with CLOSED/PARTIAL/OPEN status

4. Unreachable / residual paths
LA scores 0 without PositionRegistry population (fail-closed, intentional until writer).

Strategies without manifest entries are not registered (fail-closed).

Phase gates still suppress higher-phase strategies until active_phase rises.

5. Dependency issues
Root omega-engine binary must list crates it uses directly (already documented in Cargo.toml).

SA/MSA require Arc<LiquidityRegistry> (Option B) — wired when registering from manifest.

6. Execution-path issues
Success path of ExecutionPipeline::execute now updates nonce + exposure.

Stage-7 inclusion updates KS + NonceRegistry with strategy attribution.

DAG slot ownership remains inside pipeline RAII guard (unchanged).

7. Financial-loss scenarios addressed
Scenario

Mitigation

Trade after stale nonce / replay

Check 15 + record_processed

Duplicate exposure inflation

release on success

Unlimited exposure via missing cap

phase ≥ 1 requires env cap

Kill switch ignores losses by strategy

per-strategy Stage-7 outcomes

Strategy runs without integrity entry

not registered / Stage 2b fail-closed

8. Race conditions / concurrency notes
NonceRegistry / ExposureTracker use DashMap (lock-free reads, shard locks on write).

record_processed is monotonic max (safe under concurrent Stage-7 + execute Ok).

Exposure release is FIFO match; duplicate release is no-op.

Residual: process-local state resets on restart unless exposure snapshot path configured (optional prior work).

9. Deadlocks
No new lock ordering introduced. DAG Mutex still scoped to admit/complete RAII in pipeline.

Stage-7 loop does not hold strategy/DAG locks.

10. Code modifications performed
File

Change

crates/omega-security/src/replay.rs

record_processed + tests

crates/omega-security/src/exposure.rs

release + tests

crates/omega-relay/src/client.rs

BundlePayload metadata fields

crates/omega-relay/src/confirmation.rs

PendingBundle + ConfirmationResult metadata

crates/omega-execution/src/transform.rs

Fill metadata from blueprint

crates/omega-strategies/src/{sa,msa,la,mev,cnry}.rs

Nonce start at 1

src/main.rs

Stage-7 wiring, execute Ok path, SA/MSA/MEV reg, exposure gate, assumptions closed

ProductionIntegrationPlan.md

Status rewrite (if present from prior pass)

AUDIT_DELIVERABLES.md

This document

11. New files
AUDIT_DELIVERABLES.md (this file)

12. New / strengthened tests
NonceRegistry::record_processed_advances_highest

AccountExposureTracker::release_removes_matching_amount_fifo

13. Verification checklist
cargo test -p omega-security
cargo test -p omega-relay
cargo test -p omega-strategies
cargo check --workspace
Operator before phase ≥ 1:

OMEGA_MAX_ACCOUNT_EXPOSURE_WEI

OMEGA_KILL_*

Real deployment manifest (SA/MSA/LA/MEV entries)

Relay endpoints / signing keys



