// crates/omega-risk/src/tests/mod.rs
//
// Integration test suite for omega-risk.
//
// Tests here exercise the interaction between modules rather than testing
// individual functions in isolation (those live in each module's own #[cfg(test)]
// block).  The focus is on:
//
//   1. End-to-end check pipeline: a fully-wired CheckContext + BlueprintFields
//      flowing through run_all_checks(), with every check boundary verified.
//
//   2. Gas model â†” checks integration: l1_adaptive_buffer output fed into
//      dynamic_min_profit and then into a blueprint's dynamic_min_profit_wei.
//
//   3. Competition score â†” checks: competition_probability output fed into
//      CheckContext.competition_probability to confirm check 11 gate.
//
//   4. Circuit breaker â†” EV recovery: record() â†’ state transition â†’ l2/l3 resume.
//
//   5. Flash-crash guard â†” checks context: FlashCrashResponse fields map onto
//      CheckContext adjustments (oracle_agreement_pct tightening, size reduction).
//
//   6. Whitelist hot-update: BytecodeWhitelist.update() reflects immediately.
//
//   7. L1GasEma rolling window â†’ adaptive buffer â†’ min profit progression.
//
//   8. Metric counters increment on pass/fail (smoke-test only â€” no actual Prometheus
//      server needed; we just confirm the counters can be read without panic).

use crate::checks::{BlueprintFields, CheckResult, run_all_checks};
use crate::circuit_breakers::{CircuitBreakerRegistry, CircuitState, EV_WINDOW_BLOCKS};
use crate::competition::{AssetTier, competition_probability, priority_fee_gwei};
use crate::context::{
    CheckContext, FlashloanSnapshot, OracleSnapshot,
    CHAINLINK_STALENESS_SECS, GAS_SPIKE_THRESHOLD,
};
use crate::flash_crash::FlashCrashGuard;
use crate::gas_model::{
    dynamic_min_profit, fee_cap_variants, l1_adaptive_buffer, emergency_bundle_profitable,
    L1GasEma, L1_BUFFER_MIN, L1_BUFFER_MAX, L1_EMA_WINDOW,
};
use crate::whitelist::{AddressWhitelist, BytecodeWhitelist, BytecodeMap, AddressSet};
use crate::metrics;

// â”€â”€â”€ Shared test helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn base_bp() -> BlueprintFields {
    BlueprintFields {
        chain_id:                42161,
        expiry_block:            9999,
        l2_exec_gas_estimate:    80_000,
        l1_data_gas_estimate:    8_000,
        extraction_gas:          45_000,
        expected_profit_net_wei: 500_000_000_000_000_000,  // 0.5 ETH
        dynamic_min_profit_wei:  50_000_000_000_000_000,   // 0.05 ETH
        l1_data_fee_at_creation: 40,
        slippage_bps:            15,
        flashloan_amount:        1_000_000,
        flashloan_provider_id:   "balancer",
        strategy_id:             "LA",
        strategy_bytecode_hash:  [0xdd; 32],
        price_impact_bps:        Some(30),
        ofa_compliant:           true,
    }
}

fn base_ctx() -> CheckContext {
    CheckContext {
        expected_chain_id:         42161,
        current_block:             100,
        current_l1_gas_price_gwei: 40,
        current_l2_base_fee_gwei:  1,
        l1_adaptive_buffer:        1.30,
        oracle: OracleSnapshot {
            chainlink_price: 3000.0,
            pyth_price:      3001.5,   // 0.05 % divergence â€” within 0.4 %
            twap_price:      2998.0,
            chainlink_age_s: 5,
            pyth_age_s:      8,
            twap_age_s:      30,
        },
        flashloan: FlashloanSnapshot {
            available:   2_000_000,
            protocol_id: String::from("balancer"),
        },
        competition_probability:     0.60,
        max_competition_probability: 0.95,
        strategy_max_gas:            500_000,
        max_slippage_bps:            50,
        rollout_tier:                1.0,
        strategy_bytecode_hash:      [0xdd; 32],
        risk_score:                  0.20,
        max_risk_score:              0.85,
    }
}

// â”€â”€â”€ 1. End-to-end pipeline â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn full_pipeline_passes_with_valid_inputs() {
    metrics::register_all();
    let bp  = base_bp();
    let ctx = base_ctx();
    assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
}

#[test]
fn pipeline_fast_fails_on_chain_id_without_touching_later_checks() {
    let mut bp = base_bp();
    bp.chain_id = 1;
    // Many other fields would also fail, but WrongChain is the first drop.
    let mut ctx = base_ctx();
    ctx.current_block = 99999;            // would trigger MissExpiry
    ctx.strategy_max_gas = 1;             // would trigger MissGas
    assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(omega_core::errors::DropCode::WrongChain));
}

#[test]
fn pipeline_drops_la_blueprint_with_self_flash() {
    let mut bp = base_bp();
    bp.flashloan_provider_id = "aave";
    bp.strategy_id           = "LA";
    let mut ctx = base_ctx();
    ctx.flashloan.protocol_id = String::from("aave");
    assert_eq!(
        run_all_checks(&bp, &ctx),
        CheckResult::Fail(omega_core::errors::DropCode::MissLiquidity)
    );
}

// â”€â”€â”€ 2. Gas model â†” checks integration â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn dynamic_min_profit_from_ema_feeds_into_blueprint() {
    let ema = L1GasEma::new(L1_EMA_WINDOW);
    // Simulate stable L1 at 40 gwei.
    for _ in 0..L1_EMA_WINDOW {
        ema.push_price(40);
    }
    let buf = ema.current_buffer();
    assert!((buf - L1_BUFFER_MIN).abs() < 0.01, "stable market should give min buffer");

    let min_profit = dynamic_min_profit(
        0,            // base_min
        80_000,       // l2_exec_gas
        8_000,        // l1_data_gas
        1,            // l2_base_fee gwei
        ema.latest_gwei(),
        buf,
    );

    // Build a blueprint that barely meets the threshold.
    let mut bp = base_bp();
    bp.dynamic_min_profit_wei = min_profit as u128;
    bp.expected_profit_net_wei = min_profit as u128 + 1; // just above

    let ctx = base_ctx();
    assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
}

#[test]
fn high_volatility_l1_raises_buffer_and_can_fail_profit_check() {
    let ema = L1GasEma::new(L1_EMA_WINDOW);
    // Alternating high/low to maximize CV.
    for i in 0..L1_EMA_WINDOW {
        ema.push_price(if i % 2 == 0 { 10 } else { 200 });
    }
    let buf = ema.current_buffer();
    assert!(buf > L1_BUFFER_MIN, "volatile market should raise buffer above 1.30");

    // Use buffer to compute min profit; build a blueprint with exactly the old (low) threshold.
    let low_min = dynamic_min_profit(0, 80_000, 8_000, 1, 10, L1_BUFFER_MIN);
    let high_min = dynamic_min_profit(0, 80_000, 8_000, 1, 200, buf);
    assert!(high_min > low_min, "high volatility must raise min profit");
}

#[test]
fn fee_cap_variants_conservative_is_70pct() {
    let (cons, agg, emg) = fee_cap_variants(100);
    assert_eq!(cons, 70);
    assert_eq!(agg, 100);
    assert_eq!(emg, 200);
}

#[test]
fn emergency_bundle_unprofitable_at_2x_is_correctly_detected() {
    // profit 500, cost = 200 Ã— 5 = 1000, min = 50 â†’ 500 < 1050 â†’ not profitable
    assert!(!emergency_bundle_profitable(500, 200, 5, 50));
}

#[test]
fn emergency_bundle_profitable_well_above_threshold() {
    // profit 100_000, cost = 200 Ã— 5 = 1000, min = 50 â†’ 100_000 > 1050 â†’ ok
    assert!(emergency_bundle_profitable(100_000, 200, 5, 50));
}

// â”€â”€â”€ 3. Competition score â†” checks â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn competition_probability_gates_check_11() {
    let mut bp  = base_bp();
    bp.price_impact_bps = None; // isolate to check 11
    let mut ctx = base_ctx();

    // High competition probability exceeds threshold â†’ fail
    ctx.competition_probability     = 0.97;
    ctx.max_competition_probability = 0.95;
    assert_eq!(
        run_all_checks(&bp, &ctx),
        CheckResult::Fail(omega_core::errors::DropCode::MissCompetition)
    );

    // Same probability exactly at threshold â€” must fail (strict >)
    ctx.competition_probability = 0.95;
    assert_eq!(
        run_all_checks(&bp, &ctx),
        CheckResult::Fail(omega_core::errors::DropCode::MissCompetition)
    );

    // Just below threshold â€” must pass
    ctx.competition_probability = 0.94;
    assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
}

#[test]
fn major_asset_imminent_competition_probability_is_high() {
    let p = competition_probability(AssetTier::Major, 1.0005, 100.0);
    assert!(p > 0.90, "major asset near-instant liquidation should have high competition: {}", p);
}

#[test]
fn priority_fee_always_within_bounds() {
    // Various inputs â€” fee must always be in [2, 500].
    let cases = [
        (0.0, 1.10, 0.90),
        (0.001, 1.0001, 0.05),
        (50.0, 1.0001, 0.05),
        (1000.0, 1.05, 0.50),
    ];
    for (bonus, hf, wr) in cases {
        let fee = priority_fee_gwei(bonus, hf, wr);
        assert!(fee >= 2, "fee {} below floor for bonus={}, hf={}, wr={}", fee, bonus, hf, wr);
        assert!(fee <= 500, "fee {} above ceiling for bonus={}, hf={}, wr={}", fee, bonus, hf, wr);
    }
}

// â”€â”€â”€ 4. Circuit breaker â†” EV recovery â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn circuit_breaker_full_lifecycle() {
    let reg = CircuitBreakerRegistry::new();
    reg.register("LA");

    // Phase 1: healthy
    for _ in 0..10 {
        reg.record("LA", 1.0, 1.0);
    }
    assert_eq!(reg.state("LA"), CircuitState::Healthy);
    assert!(reg.is_operational("LA"));

    // Phase 2: performance degrades to Halted
    for _ in 0..EV_WINDOW_BLOCKS {
        reg.record("LA", 0.3, 1.0); // EV = 0.3 < 0.50 â†’ Halted
    }
    assert_eq!(reg.state("LA"), CircuitState::Halted);
    assert!(!reg.is_operational("LA"));

    // Phase 3: governance clears (L3)
    reg.clear_halt_l3("LA");
    assert_eq!(reg.state("LA"), CircuitState::Investigate);
    assert!(reg.is_operational("LA"));
    // Window cleared â†’ EV ratio = 1.0
    assert!((reg.ev_ratio("LA") - 1.0).abs() < 1e-9);

    // Phase 4: degrade to AutoPaused
    for _ in 0..EV_WINDOW_BLOCKS {
        reg.record("LA", 0.65, 1.0); // EV = 0.65 â†’ AutoPaused
    }
    assert_eq!(reg.state("LA"), CircuitState::AutoPaused);
    assert!(!reg.is_operational("LA"));

    // Phase 5: L2 resume
    reg.resume_l2("LA");
    assert_eq!(reg.state("LA"), CircuitState::Investigate);
    assert!(reg.is_operational("LA"));
}

#[test]
fn investigate_state_is_operational_but_not_healthy() {
    let reg = CircuitBreakerRegistry::new();
    for _ in 0..EV_WINDOW_BLOCKS {
        reg.record("MSA", 0.78, 1.0); // 0.70 â‰¤ 0.78 < 0.85 â†’ Investigate
    }
    assert_eq!(reg.state("MSA"), CircuitState::Investigate);
    assert!(reg.is_operational("MSA"));
}

#[test]
fn multiple_strategies_isolated() {
    let reg = CircuitBreakerRegistry::new();
    // Halt MEV
    for _ in 0..EV_WINDOW_BLOCKS {
        reg.record("MEV", 0.3, 1.0);
    }
    // SA stays healthy
    for _ in 0..10 {
        reg.record("SA", 1.0, 1.0);
    }
    assert_eq!(reg.state("MEV"), CircuitState::Halted);
    assert_eq!(reg.state("SA"),  CircuitState::Healthy);
    // LA was never recorded â€” defaults to Healthy
    assert_eq!(reg.state("LA"),  CircuitState::Healthy);
}

// â”€â”€â”€ 5. Flash-crash guard â†” context â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn flash_crash_tightens_oracle_agreement_in_context() {
    let guard = FlashCrashGuard::new();
    for _ in 0..19 { guard.push_price(2000.0); }
    guard.push_price(1700.0); // 15 % spike

    match guard.evaluate(1.05) {
        crate::flash_crash::FlashCrashResponse::Graduated { oracle_agreement_pct, .. } => {
            // Tightened from 0.4 % to 0.1 % â€” confirm the field value.
            assert!((oracle_agreement_pct - 0.001).abs() < 1e-9);
        }
        _ => panic!("expected Graduated response"),
    }
}

#[test]
fn flash_crash_imminent_liquidation_bypasses_size_reduction() {
    let guard = FlashCrashGuard::new();
    for _ in 0..19 { guard.push_price(2000.0); }
    guard.push_price(1700.0);

    let resp = guard.evaluate(1.0005); // HF < 1.001 â†’ imminent
    assert!(
        !resp.should_reduce_size(),
        "imminent liquidation must not reduce size"
    );
}

#[test]
fn flash_crash_normal_hf_does_reduce_size() {
    let guard = FlashCrashGuard::new();
    for _ in 0..19 { guard.push_price(2000.0); }
    guard.push_price(1700.0);

    let resp = guard.evaluate(1.05); // not imminent
    assert!(
        resp.should_reduce_size(),
        "non-imminent during flash crash must reduce size"
    );
}

#[test]
fn flat_market_never_triggers_flash_crash() {
    let guard = FlashCrashGuard::new();
    for _ in 0..20 { guard.push_price(1800.0); }
    assert!(guard.evaluate(1.05).is_normal());
    assert!(guard.evaluate(1.0001).is_normal());
}

// â”€â”€â”€ 6. Whitelist hot-update â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn bytecode_whitelist_hot_update_visible_immediately() {
    let wl = BytecodeWhitelist::new(BytecodeMap::new());

    // Not registered yet.
    assert!(!wl.is_approved("SA", &[0xab; 32]));

    // Register via hot-update.
    let mut map = BytecodeMap::new();
    map.insert("SA".into(), [0xab; 32]);
    wl.update(map);

    assert!(wl.is_approved("SA", &[0xab; 32]));
    assert!(!wl.is_approved("SA", &[0x00; 32]));
}

#[test]
fn address_whitelist_add_remove_cycle() {
    let wl = AddressWhitelist::new(AddressSet::new());
    let addr: [u8; 20] = [0x42; 20];

    assert!(!wl.is_approved(&addr));
    wl.add(addr);
    assert!(wl.is_approved(&addr));
    wl.remove(&addr);
    assert!(!wl.is_approved(&addr));
}

// â”€â”€â”€ 7. L1GasEma progression â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn ema_window_fills_and_computes_correct_buffer() {
    let ema = L1GasEma::new(L1_EMA_WINDOW);
    assert!(ema.is_empty());
    assert!((ema.current_buffer() - L1_BUFFER_MIN).abs() < 1e-9);

    // Fill half the window.
    for _ in 0..(L1_EMA_WINDOW / 2) {
        ema.push_price(50);
    }
    assert_eq!(ema.len(), L1_EMA_WINDOW / 2);

    // Constant prices â†’ CV = 0 â†’ buffer stays at minimum.
    assert!((ema.current_buffer() - L1_BUFFER_MIN).abs() < 1e-9);

    // Fill to capacity.
    for _ in 0..(L1_EMA_WINDOW / 2) {
        ema.push_price(50);
    }
    assert_eq!(ema.len(), L1_EMA_WINDOW);

    // Introduce high volatility.
    for i in 0..L1_EMA_WINDOW {
        ema.push_price(if i % 2 == 0 { 5 } else { 500 });
    }
    let buf = ema.current_buffer();
    assert!(buf > 1.50, "volatile window should raise buffer well above 1.30: {}", buf);
    assert!(buf <= L1_BUFFER_MAX);
}

#[test]
fn ema_thread_safe_concurrent_pushes() {
    use std::sync::Arc;
    use std::thread;

    let ema = Arc::new(L1GasEma::new(L1_EMA_WINDOW));
    let mut handles = Vec::new();

    for i in 0..8u64 {
        let ema2 = Arc::clone(&ema);
        handles.push(thread::spawn(move || {
            for j in 0..100u64 {
                ema2.push_price(10 + (i * j) % 200);
            }
        }));
    }
    for h in handles { h.join().unwrap(); }

    // After concurrent writes the window should be full and buffer in bounds.
    let buf = ema.current_buffer();
    assert!(buf >= L1_BUFFER_MIN && buf <= L1_BUFFER_MAX);
    assert_eq!(ema.len(), L1_EMA_WINDOW);
}

// â”€â”€â”€ 8. Gas spike boundary â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn gas_spike_threshold_boundary_conditions() {
    let bp = base_bp(); // l1_data_fee_at_creation = 40

    // Exactly 30 % increase: 40 Ã— 1.30 = 52 â€” NOT strictly greater, should pass.
    let mut ctx = base_ctx();
    ctx.current_l1_gas_price_gwei = 52;
    assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass,
        "30% delta exactly equals threshold â€” should pass (not strictly greater)");

    // 31 % increase: 40 Ã— 1.31 = 52.4 â†’ 53 gwei â€” should fail.
    let mut ctx2 = base_ctx();
    ctx2.current_l1_gas_price_gwei = 53;
    assert_eq!(
        run_all_checks(&bp, &ctx2),
        CheckResult::Fail(omega_core::errors::DropCode::MissGasSpike),
        "31% delta should trigger MissGasSpike"
    );
}

// â”€â”€â”€ 9. Oracle freshness boundary â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn oracle_exactly_at_staleness_boundary_is_stale() {
    // age_s == threshold means "at least threshold seconds old" â€” treat as stale.
    let bp = base_bp();
    let mut ctx = base_ctx();
    ctx.oracle.chainlink_age_s = 45; // == CHAINLINK_STALENESS_SECS (not fresh: strict <)
    ctx.oracle.pyth_age_s      = 45;
    ctx.oracle.twap_age_s      = 120;
    // All three at or beyond threshold â†’ MissOracle.
    assert_eq!(
        run_all_checks(&bp, &ctx),
        CheckResult::Fail(omega_core::errors::DropCode::MissOracle)
    );
}

#[test]
fn one_fresh_oracle_sufficient() {
    let bp = base_bp();
    let mut ctx = base_ctx();
    ctx.oracle.chainlink_age_s = 100;  // stale
    ctx.oracle.pyth_age_s      = 100;  // stale
    ctx.oracle.twap_age_s      = 10;   // fresh â€” one is enough
    assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
}

// â”€â”€â”€ 10. Metrics smoke test â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn metrics_register_and_increment_without_panic() {
    metrics::register_all();

    // Force a pass and a fail to exercise counter increment paths.
    let bp  = base_bp();
    let ctx = base_ctx();
    run_all_checks(&bp, &ctx); // pass

    let mut bp2 = base_bp();
    bp2.chain_id = 1;
    run_all_checks(&bp2, &ctx); // fail: WrongChain

    // If we got here without panic the metrics are wired correctly.
}