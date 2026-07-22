# omega-engine/docs/runbooks/kill-switch-tripped.md
# Runbook: Kill Switch Tripped

**Alert:** `KillSwitchTripped`
**Severity:** page
**Source:** `omega_risk_kill_switch_tripped{scope} > 0`
**Code:** `crates/omega-risk/src/kill_switch.rs`

## What this means

The kill switch is an **absolute, funds-at-risk** halt — separate from
and independent of the circuit breaker (which tracks *relative* EV-ratio
degradation, see `circuit-breaker-halted.md`). If this alert fired, real
capital stopped moving because one of three hard-coded, non-adaptive
thresholds was breached:

1. **Cumulative loss** — all-time realized loss for this scope reached
   `max_cumulative_loss_wei`.
2. **Window loss** — realized loss within the configured rolling window
   reached `max_loss_per_window_wei`. This is the one that catches a
   *fast* bleed — it can fire even when cumulative loss is nowhere near
   its own cap, because a strategy that was healthy for a long time and
   then suddenly starts losing money quickly will trip this before it
   trips the cumulative check.
3. **Consecutive failures** — `max_consecutive_failures` submissions in a
   row failed/reverted, regardless of dollar loss. This usually means the
   strategy is fundamentally broken (bad state assumptions, a changed
   contract, an exhausted approval), not just unlucky.

Or someone pulled it manually via `trip_manual()`.

**This does not auto-resolve.** By design, there is no cooldown or
self-healing — the switch stays tripped until a human calls `reset()`.
That is intentional: a bug that fooled the circuit breaker's EV model
should not be trusted to un-fool itself.

## First 5 minutes

1. **Identify the scope.** Check the `scope` label on the alert — this is
   either a specific strategy ID (`LA`, `SA`, `MSA`, `MEV`) or `GLOBAL` if
   `trip_all()` was called.
2. **Confirm it's real, not a metrics glitch.** Query
   `omega_risk_kill_switch_tripped{scope="<scope>"}` directly in
   Prometheus/Grafana. If it reads `1`, treat this as real — do not
   assume it's a false positive without evidence.
3. **Check which condition tripped.** Call
   `KillSwitchRegistry::diagnostics(scope)` — this returns status
   (including the full `TripEvent`/reason), cumulative loss, the
   consecutive-failure streak, the raw loss entries currently inside the
   configured window, and the switch's own thresholds, all captured under
   one lock so they're mutually consistent. It will be one of
   `CumulativeLoss`, `WindowLoss`, `ConsecutiveFailures`, or `Manual`.
   **Do not proceed to reset until you know which one fired** — the
   diagnosis path differs for each.
4. **Do not call `reset()` yet.** Resetting clears the tripped flag but
   deliberately does **not** clear cumulative loss or the failure streak
   (see `KillSwitch::reset` doc comment). If the underlying cause isn't
   fixed, the very next recorded outcome can retrip immediately — a
   premature reset just adds noise and can mask how bad the underlying
   problem actually is.

## Diagnosis by trip reason

### CumulativeLoss

- Call `KillSwitchRegistry::diagnostics(scope)` and check
  `cumulative_loss_wei` against `config.max_cumulative_loss_wei` — by
  definition the former is now `>=` the latter. (Equivalent to comparing
  `omega_risk_kill_switch_cumulative_loss_wei{scope}` against
  `omega_risk_kill_switch_max_cumulative_loss_wei{scope}` if you're
  working from Prometheus directly instead.)
- Look at the realized-profit history for this scope over the relevant
  time range (report data from `omega-simulation::SimulationReport` or
  `omega-testnet::TestnetReport`, whichever transport this scope runs on).
  Is the loss concentrated in a handful of large trades, or spread evenly
  across many small ones?
  - **Concentrated in a few trades** → look at those specific bundles.
    Common causes: a mispriced oracle read that made a trade look
    profitable when it wasn't, a slippage/MEV-sandwich loss on an
    unexpectedly large fill, or a reentrancy/logic bug in
    `LiquidationArb.sol` / `MevOfa.sol` that leaked value on execution.
  - **Spread evenly** → look at systemic causes: gas price regime shift
    (`omega_risk_l1_adaptive_buffer` pinned high?), a strategy whose
    profit model has quietly drifted out of date, or competition
    consistently winning races after this bot pays gas (check
    `omega_risk_checks_failed_total{drop_code="miss_competition"}` trend
    — if that's *not* firing but losses are still happening, the
    competition model itself may be under-estimating).
- Cross-reference with the circuit breaker for the same scope
  (`omega_risk_ev_ratio{strategy}`) — if EV ratio was *also* degrading
  before this tripped, the two systems are telling a consistent story and
  root cause is likely a real, sustained profitability problem, not a
  one-off bug.

### WindowLoss

- This fired despite cumulative loss (probably) still being under its own
  cap — meaning something changed recently. Call
  `KillSwitchRegistry::diagnostics(scope)` and inspect `window_losses` —
  the raw `(at, loss_wei)` entries currently inside the configured
  window, computed fresh at read time (not stale leftovers from the last
  trade) — and look for:
  - **A single large loss** — most likely an acute bug or a bad trade,
    not a trend. Check the specific bundle/transaction.
  - **Several losses in quick succession** — could be a contract upgrade
    on a dependency (DEX pool, flashloan provider, oracle) that broke an
    assumption the strategy's logic depends on. Check whether any
    external contract this strategy interacts with was upgraded/paused
    recently.
  - **A flash-crash window** — check `omega_risk_flash_crash_active{asset}`
    for the relevant asset around this time. If active, the graduated
    response (reduced size, higher profit multiplier, tighter oracle
    agreement) should have already reduced exposure — if losses still
    accumulated fast enough to trip this, the graduated response's
    parameters may need revisiting, or the crash was severe enough to
    outrun any graduated mitigation.

### ConsecutiveFailures

- This is usually the *fastest* to diagnose and the *safest* to reset
  once fixed, because it fired on failure count, not dollar loss — the
  financial damage is typically just gas.
- Check the revert reason on the most recent failed submissions. Common
  causes:
  - Stale state assumptions (a contract upgraded, an oracle format
    changed, an ABI mismatch after a redeploy).
  - Exhausted or revoked token approval.
  - RPC/node issue causing submissions to be built against stale block
    state (check heartbeat freshness for the relevant component — see
    `ComponentHeartbeatStale` alert; a struggling RPC connection often
    produces both a stale heartbeat *and* a failure streak together).
  - A relay-side change (only relevant once this scope is running against
    a real relay in `omega-testnet`/production, not `omega-simulation`).

### Manual

- Check the trip reason string — `trip_manual()` requires both an
  `operator` and a `reason` string, so this should already tell you who
  pulled it and why. Follow up with that person directly before doing
  anything else; don't reset a manual trip without their sign-off.

## Resolution

1. Confirm the root cause is understood and addressed — not just that the
   metric has stopped increasing. "It hasn't lost more money in the last
   ten minutes" is not the same as "the bug is fixed."
2. If a code fix was required, confirm it's deployed to the actual
   running process for this scope before resetting — resetting against
   the *old* code just re-triggers the same failure.
3. Call `reset(operator, reason)` with a specific, non-generic reason
   string (e.g. "fixed oracle staleness check in PR #482, redeployed
   commit abc123" — not "fixed it"). This string is what shows up in the
   audit trail: `KillSwitchRegistry::reset` increments
   `omega_risk_kill_switch_reset_total{scope}` and sets
   `omega_risk_kill_switch_reset_last_operator_info{scope, operator,
   reason}` to 1, and the `KillSwitchResetOccurred` alert (info severity,
   `ops/alerts/omega-risk.yaml`) surfaces the reset to the team
   automatically — you no longer need to post it manually.
4. Watch the scope closely for at least one full `loss_window` duration
   after reset, since `reset()` does not clear the failure streak or
   cumulative loss counters — if the fix didn't actually work, it will
   retrip, likely quickly.
5. If you're not confident the root cause is fixed but capital needs to
   keep flowing for other reasons, do **not** reset this scope — that
   defeats the purpose of an absolute safety threshold. Escalate instead.

## Escalation

- If root cause isn't identified within 30 minutes, escalate to whoever
  owns the affected strategy's logic (not just whoever owns
  infrastructure/on-call).
- If the trip reason is `CumulativeLoss` and the loss is large relative to
  the position size being run, treat this as a capital-risk incident, not
  just an engineering bug — loop in whoever owns capital allocation
  decisions before any reset, regardless of whether the technical root
  cause is understood.
- If this is a `GLOBAL` scope trip (from `trip_all()`), every strategy is
  halted — this is already the most severe state the system can be in.
  Confirm whether the trigger was itself global (e.g. suspected oracle
  manipulation affecting all strategies) or whether `trip_all()` was
  called defensively in response to a single-scope issue; the resolution
  path differs significantly between those two cases.

## Related

- `circuit-breaker-halted.md` — the relative/EV-ratio counterpart to this
  alert; check it too if `omega_risk_circuit_breaker_state` for the same
  scope also shows degradation.
- `ComponentHeartbeatStale` — check this wasn't *also* firing around the
  same time; a dead/hung process and a kill-switch trip can share a root
  cause (e.g. an RPC outage causing both stale reads and bad trades).