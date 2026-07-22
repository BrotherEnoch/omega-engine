# README.md
# What changed, and what you still have to decide

All eight files (the 5 strategies + Orchestrator + Vault + OPIL token) were rewritten and
actually compiled together as one project with solc 0.8.24 against OpenZeppelin 5.0.2 — not
just reviewed by eye. The compile log is clean, zero errors, zero warnings.

## The big one

**The flashloan callback didn't exist.** The Orchestrator's docs said the flashloan provider
"calls back into this contract via `executeWithFlashloan()`" — but no such function existed
anywhere, on any of the original files. The old `_executeFlashloan` just measured this
contract's ETH balance before/after calling a `flashloan()` function that didn't even specify
which ERC20 to borrow, called the difference "profit," and stopped. There was no actual
borrow, no transfer to the strategy, no repayment, and no way for any of it to work under a
real flashloan provider's calling convention.

Related to that: the strategy contracts computed a profit number but never transferred a
single token back to the Orchestrator, and the Vault's `receivePendingProfit` only
incremented a counter — it never actually received tokens either. Three different files, each
individually "correct" in isolation, none of them actually moving money end-to-end.

This is now implemented for real, against Balancer V2's Vault interface
(`flashLoan`/`receiveFlashLoan`) specifically — see the design note at the top of
`OmegaOrchestrator.sol` for exactly why Balancer, and what switching providers would require.
Every strategy now ends by transferring its output token back to the Orchestrator; the
Orchestrator repays the flashloan provider and forwards real tokens (with an approval, pulled
via `transferFrom`) into the Vault.

**This also means the blueprint's on-chain layout changed** — it now includes an explicit
`address flashloanToken` field, because a flashloan provider has to be told what to lend.
Whatever off-chain system signs and submits blueprints needs to be updated to match the new
`abi.encode(...)` layout documented at the top of `OmegaOrchestrator.sol`.

## Other real bugs fixed

- **OpilToken.sol wouldn't have compiled at all.** OpenZeppelin 5.x removed
  `_beforeTokenTransfer`/`_afterTokenTransfer` and made `_mint`/`_burn` non-overridable;
  everything now goes through one `_update` function. Rewritten around that.
- **A governance griefing hole in OPIL's vote lock.** The old design reset a holder's entire
  7-day vote lock on *any* incoming transfer, including a 1-wei transfer from anyone. That
  means anyone could zero out a large holder's voting power right before a vote, for the cost
  of a dust transfer — cheaper than the flash-loan attack the lock was built to prevent. Fixed
  with a balance-weighted-average lock timestamp instead of a blanket reset.
- **A silent unit mismatch that would have broken historical vote lookups.** OZ 5.x's
  `ERC20Votes` defaults to block-number-based checkpoints, but the lock check compared that
  against a `block.timestamp`-based value — apples to oranges, `getPastVotes` would have
  returned 0 almost always. Fixed by switching the token to timestamp-mode checkpoints.
- **OmegaVault's `receivePendingProfit` never moved tokens**, just incremented a mapping —
  `releaseProfit` would then try to pay out of a balance the Vault never actually had. Fixed
  to pull real tokens via `transferFrom` in the same call.
- **Missing signing domain separation.** Blueprints were signed over
  `keccak256(blueprintCalldata)` alone, with no reference to which specific contract deployment
  they were for. Since this system explicitly has a canary deployment alongside a production
  one, a validly-signed blueprint for one could in principle be replayed on the other if they
  share a signing key. Fixed by folding the contract address and chain ID into the signed hash.
- **Hand-rolled `ecrecover`** replaced with OpenZeppelin's `ECDSA.recover`, which rejects
  malformed and malleable signatures.
- **An unused `EXECUTOR_ROLE`** was granted but never checked anywhere in the original
  Orchestrator — removed, since a dead access-control role sitting in the code is itself a
  trap for whoever reads it later and assumes it does something.
- OZ 5.x moved `ReentrancyGuard`/`Pausable` from `security/` to `utils/` — old import paths
  wouldn't have resolved.

## Things I flagged instead of guessing

- The Orchestrator only supports **one flashloan provider and one profit token per
  deployment**. The strategy docs mention sourcing loans from "Balancer or Uniswap" depending
  on the target — that flexibility isn't built here; switching providers is an admin call, not
  a per-transaction choice, and a genuinely different provider (Uniswap V3's flash has a
  completely different callback shape) isn't supported without more code.
- **`DEPTH_UPDATER_ROLE` is not granted to anyone at deploy time.** Someone with
  `DEFAULT_ADMIN_ROLE` has to call `grantRole(DEPTH_UPDATER_ROLE, <relayer address>)` after
  deployment, or confirmation depth can never advance and no profit can ever be released.
  Noted directly in the Vault's constructor comment so it isn't a silent gap.
- Morpho market params and the Uniswap V3 router version are still your calls, as flagged in
  the previous round.

## What I still can't do for you

This is a substantially more complete system than what I started with, but "compiles clean
and the logic is internally consistent" is still not "audited." A missing callback function,
a token transfer nobody wrote, and a diamond-inheritance conflict that would have failed to
compile are all things that only surfaced because I actually wired the files together and
compiled them as one project — a professional audit firm will go further than that, and on
code that moves real capital through five external protocols, that step isn't optional.