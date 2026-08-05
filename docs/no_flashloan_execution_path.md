# no_flashloan_execution_path.md
# Design decision: does Omega need a "no flashloan" execution path?

**Status:** open, blocking. Must be resolved before `ExecutionBlueprint`
field additions or the `blueprintCalldata` encoder are designed.

**Trigger:** `MsaStrategy::build_blueprint` currently writes
`flashloan_provider: Address::ZERO, flashloan_amount: U256::ZERO`. `SA`
does the same. Neither strategy calls `omega-flashloan::select_provider`.
Confirmed against real `OmegaOrchestrator.sol` source: there is no
Orchestrator-side handling for this case — `execute()` reverts on
`flashloanToken == address(0)` before `_executeFlashloan` is ever reached,
and `_executeFlashloan` itself has no zero-amount branch for any provider.
So today, MSA and SA cannot produce a blueprint that survives `execute()`,
full stop — independent of any other gap in this thread.

---

## Option A — MSA/SA are self-funded; add a real "no flashloan" path

**Premise:** some strategies don't need borrowed capital — e.g. a
multi-step arb that only rebalances an already-held position, or a
Phase-1 Microtx strategy sized small enough to run from treasury capital
directly.

**What it requires, concretely:**

1. **Solidity:** add `FlashloanProviderType.None` (or equivalent sentinel)
   as a 4th enum variant. In `_executeFlashloan`, add a branch that skips
   all three provider calls and calls the strategy directly against
   whatever capital already sits at `stratAddr` or the Orchestrator
   itself — this needs its own design pass, since none of the existing
   three branches model "strategy already has what it needs."
2. **Solidity:** the `flashloanToken == address(0)` revert in `execute()`
   would need to become conditional on provider type — a `None` blueprint
   still needs a valid token address for the `TokenMismatchWithVault`
   check and for `minNetProfit`'s denomination, so this isn't just "skip
   the check," it's "the field still means something, just not 'what to
   borrow.'"
3. **Rust:** `ExecutionBlueprint` needs a `FlashloanProviderType`-shaped
   field, not just the existing `flashloan_provider: Address` — otherwise
   there's no way to distinguish "intentionally self-funded" from
   "forgot to call select_provider," which is exactly the ambiguity that
   caused this design question to surface in the first place.
4. **Capital source question, unresolved by this document:** if MSA
   doesn't borrow, where does its capital come from? Held directly by the
   strategy contract? Pulled from the Vault? This is a real open question
   this write-up doesn't answer — it would need its own design pass
   against `omega-strategies` and the Vault's actual balance-holding
   model before Option A is implementable, not just specified.

**Cost:** real Solidity contract change, plus an unresolved capital-source
question. Not a small addition.

---

## Option B — MSA/SA are incomplete stubs; route them through selection like LA

**Premise:** every strategy that touches an external DEX/pool needs
borrowed capital to be capital-efficient (the whole point of a
zero-capital flashloan execution engine, per the Orchestrator's own
header). MSA/SA's zeros are simply unfinished wiring, not an intentional
design choice — consistent with `MsaStrategy`'s constructor not even
accepting a flashloan provider argument, which reads more like "not
wired up yet" than "deliberately excluded."

**What it requires, concretely:**

1. Give `MsaStrategy` (and `SaStrategy`, if it needs flashloan capital
   too — worth confirming, since Phase 1 Microtx might be small enough to
   be genuinely self-funded even under this option) a `LiquidityRegistry`
   handle, the same way LA will need one.
2. Call `select_provider` inside `build_blueprint`, same as the LA fix.
3. No Orchestrator changes needed — `_executeFlashloan` already handles
   all three real provider types correctly.

**Cost:** no contract changes. Same wiring work as LA, done three times
instead of once.

---

## Recommendation: default to Option B unless a capital-source answer for Option A already exists

Reasoning:

- Option B requires zero Solidity changes to an already-verified,
  already-deployed contract. Option A requires modifying `execute()` and
  `_executeFlashloan` — the exact function this whole thread has spent
  many turns getting a *verified* read of. Re-opening it for a new branch
  reintroduces exactly the kind of unverified-assumption risk this thread
  hit twice already (the fabricated base-fee guard, the fabricated
  reconciliation layer).
- The Orchestrator's own header explicitly frames it as a "zero-capital
  flashloan execution engine" — self-funded execution isn't obviously
  in scope for what this contract was built to do, and building a
  self-funded path in without instructions to would be scope invention on
  the same order as the fabricated Solidity fields earlier in this
  thread.
- Option A's capital-source question is a second unresolved design
  question, not a detail — recommending Option A without an answer to
  "where does the capital come from" would just move the blocking
  decision one layer down rather than resolve it.

**This recommendation is conditional, not final** — if there's a real
reason MSA needs to run self-funded (a business/economic reason I don't
have visibility into: gas savings on small trades, avoiding flashloan fee
drag on thin margins, or an existing off-chain plan for where that capital
sits), that would flip this to Option A, and the capital-source question
above would need to be answered before implementation, not deferred.

---

## Immediate next step regardless of which option is chosen

Confirm whether `SaStrategy` (Phase 1 Microtx) also needs real flashloan
capital, or whether its `<200k gas` sub-hot-path profile means it's
plausibly, genuinely self-funded by design (small enough trades that
treasury capital covers it without borrowing). This determines whether
Option B needs to wire up two strategies (MSA + SA) or just one (MSA),
and whether Option A's "self-funded" branch needs to support one caller
or two.