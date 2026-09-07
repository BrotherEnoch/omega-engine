# docs/ControlPointStatus_Complete_(2026-09-07).md

## 1. Missing control points
| Item | Status |
|------|--------|
| Kill switch thresholds | **Present** in omega-risk (cumulative / window / consecutive) |
| Kill switch explicit env @ phase≥1 | **FIXED** this pass |
| LaReorgRiskEvent → KS | **Present** |
| LaReorgRiskEvent → LA position rescore | **FIXED** (`invalidate_chain`) |
| Manifest completeness @ phase≥1 | **FIXED** |
| rollout_tier → risk_score | **Present** on main |
| LA TokenPriceLookup | **Present** on main |
| MSA/SA select_provider | **Present** on main |

## 2. Broken control points
| Item | Status |
|------|--------|
| DAG complete via unwrap poison panic | **FIXED** — `dag_complete_slot` recovers |
| Silent kill defaults on live phase | **FIXED** |

## 3. Disconnected control points
| Item | Status |
|------|--------|
| rollout_tier | **Connected** to composite risk_score |
| reorg events | **Connected** to KS + PositionRegistry |

## 4. Unreachable / intentional dead paths
| Path | Status |
|------|--------|
| Phase 0 relay suppress | Intentional |
| CNRY/MEV zero flashloan | Intentional product paths |
| LA without positions/prices | Fail-closed intentional |
| DAG `ready()` unused | Intentional — admit always uses `&[]` deps; DAG is capacity/slot tracker |

## 5. Execution path
```
score → build_blueprint → dag.admit → (hot|ZK) → execute(DagSlotGuard)
  early fail → dag_complete_slot
  success → execute owns complete via RAII Drop
Stage-7 reconcile → KS + NonceRegistry
Reorg event → KS + invalidate_chain
```

## 6. Dependencies
- omega-strategies → omega-flashloan, omega-positions (no omega-oracle; price injected)
- Root binary lists direct crate deps
- No new crate cycles introduced

## 7. Financial loss scenarios
| Scenario | Mitigation |
|----------|------------|
| Trade after KS trip | Stage 2a guard |
| Unlimited exposure | env cap phase≥1 |
| Stale LA after reorg | invalidate_chain |
| Duplicate / replay | idempotency + nonce check 15 |
| Zero flashloan live | strategies fail + Orchestrator revert |
| Silent kill defaults | phase≥1 require explicit env |
| DAG slot leak | DagSlotGuard + dag_complete_slot on early exit |

## 8. DAG functionality
| API | Wired | Notes |
|-----|-------|-------|
| admit | Yes | score_and_admit before dispatch |
| complete | Yes | early paths + DagSlotGuard Drop |
| ready | Not called | deps always empty; capacity via counts |
| cycle detect | Yes | inside admit |
| slot capacity | Yes | microtx/normal counters |
| poison recovery | Yes | DagSlotGuard + dag_complete_slot |

DAG is fully functional for its production role (capacity accounting + cycle/dup rejection + RAII release).
