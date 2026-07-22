# omega-engine/docs/runbooks/circuit-breaker-halted.md
# Runbook: Circuit Breaker Halted

**Alert:** `CircuitBreakerHalted`
**Severity:** page
**Source:** `omega_risk_circuit_breaker_state{strategy} == 3`
**Code:** `crates/omega-risk/src/circuit_breakers.rs`
**Spec reference:** Section 19 (Adaptive EV-Weighted Rollout)

## What this means

The circuit breaker tracks **relative** performance — the ratio of
observed to expected profit (`EV ratio`) over a rolling 72-block window —
per strategy. This is a different signal from the kill switch (see
`kill-switch-tripped.md`), which tracks **absolute** funds-at-risk
independent of what was expected. A strategy can trip this without ever
tripping the kill switch (e.g. it's losing less than expected but still
losing, over a sustained window), and vice versa (a single acute bug can
blow the kill switch's absolute threshold before the EV ratio has enough
samples in its window to reflect it).

State machine (spec S19):

| EV ratio | State | Operational? | Recovery path |
|---|---|---|---|
| ≥ 0.85 | Healthy | Yes | — |
| 0.70 – 0.85 | Investigate | Yes | Auto-resolves if ratio recovers |
| 0.50 – 0.70 | AutoPaused | **No** | L2 fast-approve (`resume_l2()`) |
| < 0.50 | Halted | **No** | L3 governance (`clear_halt_l3()`) |

This runbook covers **Halted** (EV ratio < 0.50). If you're looking at
`CircuitBreakerAutoPaused` instead, the diagnosis steps below still apply
but the recovery bar is lower — see the L2 section near the end.

## First 5 minutes

1. **Identify the strategy.** Check the `strategy` label
   (`SA`/`MSA`/`LA`/`MEV`).
2. **Confirm current state and ratio.** Query
   `omega_risk_circuit_breaker_state{strategy="<id>"}` (should read `3`)
   and `omega_risk_ev_ratio{strategy="<id>"}` for the current value.
3. **Check whether the kill switch also tripped for this strategy.**
   Query `omega_risk_kill_switch_tripped{scope="<id>"}`. If both fired,
   treat this as one incident with two independent confirmations, not two
   separate problems — start with `kill-switch-tripped.md`'s diagnosis
   steps since it tracks the more concrete signal (real wei lost, not a
   ratio).
4. **Do not call `clear_halt_l3()` yet.** Unlike the kill switch's
   `reset()`, this call additionally **clears the EV window** — the
   strategy starts fresh at ratio 1.0 with zero history. Clearing before
   understanding root cause means you lose the exact data you'd want to
   diagnose *why* it halted, and the strategy immediately resumes trading
   with no memory of the problem that just occurred.

## Diagnosis

The EV ratio is `sum(observed_profit) / sum(expected_profit)` over the
last 72 blocks. A ratio below 0.50 means realized profit was less than
half of what was expected, summed across that window. Work through these
in order:

1. **Is this a strategy-logic problem or a market-conditions problem?**
   - Pull the individual `(observed, expected)` pairs feeding the current
     window (via whatever your control-plane exposes from
     `StrategyCircuitBreaker`'s internal window, or from the underlying
     report data if this strategy runs through `omega-simulation` /
     `omega-testnet`).
   - **Consistently and uniformly below expected** (e.g. every trade
     realizes ~40% of its predicted profit) → suggests the profitability
     *model* itself is miscalibrated — check `dynamic_min_profit`'s cost
     inputs (`crates/omega-risk/src/gas_model.rs`) against real recent gas
     costs, and check the competition model
     (`crates/omega-risk/src/competition.rs`) against actual observed
     competition — if bots are winning more races than the competition
     model assumes, expected profit will systematically overstate reality.
   - **A few trades realized deeply negative outcomes, most were fine** →
     look at those specific trades individually. This pattern points to
     an acute bug or an edge case (e.g. a specific pool's liquidity was
     thinner than assumed, a specific asset's oracle was briefly stale) —
     not a systemic miscalibration.
2. **Check competition and gas context around the same window.**
   - `omega_risk_checks_failed_total{strategy="<id>", drop_code="miss_competition"}`
     — is this strategy still winning enough races to matter, or is it
     spending gas on trades that then lose to a faster bot? A high
     competition-drop rate *before* execution, combined with a low EV
     ratio *after* execution, suggests the checks are correctly filtering
     out easy losses but the ones that do get through are systematically
     the ones this strategy shouldn't have taken.
   - `omega_risk_l1_adaptive_buffer{chain_id}` — was gas unusually
     volatile during this window? High volatility raises the min-profit
     floor going forward, but historical trades in the current window were
     scored against whatever buffer was active *at the time* — a rapid
     spike could mean recent trades were scored too optimistically.
3. **Check for a flash-crash correlation.** Same as the kill-switch
   runbook: query `omega_risk_flash_crash_active{asset}` for the assets
   this strategy trades. A flash crash's graduated response (tighter
   oracle agreement, reduced size, higher profit multiplier) is meant to
   protect against exactly this, but a severe or fast-moving crash can
   still degrade EV ratio before the graduated response fully takes
   effect.
4. **Check whitelist/whitelist-adjacent changes.** If
   `BytecodeWhitelist` was recently updated (`update()`/`register()` in
   `crates/omega-risk/src/whitelist.rs`), confirm the currently-executing
   bytecode actually matches what was intended — a whitelist update
   pointing at the wrong hash wouldn't fail checks (since the deployed
   contract presumably still matches *some* approved hash), but could mean
   an unintended contract version is live.

## Resolution

### If Halted (EV ratio < 0.50) — L3 governance required

1. Confirm root cause per the diagnosis steps above. "Halted" is the most
   severe state this breaker has — treat the L3 bar seriously; this
   should not be a rubber-stamp.
2. Get sign-off from whoever your L3 governance process designates
   (per spec S19 — this is deliberately a higher bar than the kill
   switch's single-operator `reset()`, since clearing a halt also wipes
   the EV history).
3. If a fix was required (model recalibration, bug fix, whitelist
   correction), confirm it's deployed and verified before clearing.
4. Call `clear_halt_l3(operator, reason)` with a specific, non-generic
   reason string — same discipline as the kill switch's `reset()`. This
   resets state to **Investigate** (not Healthy) and clears the window —
   the strategy resumes with zero EV history and will need to accumulate
   a fresh window before its state can improve past Investigate on its
   own. The call increments `omega_risk_circuit_breaker_l3_clear_total{strategy}`
   and sets `omega_risk_circuit_breaker_l3_clear_last_operator_info{strategy,
   operator, reason}` to 1; the `CircuitBreakerL3ClearOccurred` alert
   (info severity, `ops/alerts/omega-risk.yaml`) surfaces the clear to
   the team automatically — no need to announce it manually.
5. Watch closely. Because the window was cleared, the breaker has no
   memory of the problem — if the underlying cause wasn't actually fixed,
   it will take a fresh 72 blocks (or however many produce a below-0.85
   ratio) to re-trip, which is slower feedback than the kill switch's
   near-immediate retrip on `reset()`. Don't mistake that delay for
   confirmation the fix worked.

### If AutoPaused (EV ratio 0.50–0.70) instead — L2 fast-approve

- Lower bar than Halted, by design (spec S19) — a single on-call engineer
  with L2 authority can call `resume_l2()`.
- Still do the diagnosis steps above first; "fast-approve" means a faster
  process, not skipping diagnosis.
- Call `resume_l2(operator, reason)` — same audit-trail discipline as the
  L3 path below. This moves state to **Investigate** and does **not**
  clear the EV window — the ratio will continue reflecting recent history
  until it ages out of the 72-block window naturally, a meaningfully
  different behavior from `clear_halt_l3()`'s hard reset. The call
  increments `omega_risk_circuit_breaker_l2_resume_total{strategy}` and
  sets `omega_risk_circuit_breaker_l2_resume_last_operator_info{strategy,
  operator, reason}` to 1; the `CircuitBreakerL2ResumeOccurred` alert
  (info severity, `ops/alerts/omega-risk.yaml`) surfaces the resume to
  the team automatically.

## Escalation

- If you can't distinguish "model miscalibration" from "acute bug" within
  30 minutes, escalate to the strategy's logic owner — this diagnosis
  fundamentally requires domain knowledge of what "expected profit" was
  supposed to mean for this strategy.
- If the same strategy has halted more than once in a short period after
  being cleared, that's a signal the previous root-cause diagnosis was
  wrong or incomplete — treat a repeat halt as higher priority than the
  first one, not routine.

## Related

- `kill-switch-tripped.md` — the absolute/funds-at-risk counterpart;
  check whether the kill switch also tripped for this same strategy.
- `CircuitBreakerAutoPaused` / `CircuitBreakerInvestigate` — lower-severity
  states in the same state machine; this runbook's diagnosis steps apply
  to those too, just with a lower recovery bar.