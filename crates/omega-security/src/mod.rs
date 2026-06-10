// crates/omega-security/src/tests/mod.rs
//
// Integration test suite for omega-security.
//
// Tests here exercise the interaction between modules:
//
//   1. Sign → Verify → Replay guard: full blueprint submission security pipeline.
//   2. Key rotation → dual-window → both keys valid → rotation completes.
//   3. Nonce progression → validation → advance → re-validation.
//   4. OFA compliance: all four rules exercised together.
//   5. Integrity: freeze then full_integrity_check rejects frozen strategy.
//   6. Signer uses KeyManager: rotation changes which key signs.
//   7. Replay guard concurrent safety: 16 threads race on same hash, one wins.
//   8. SecurityError::is_halt_worthy() categorisation.
//   9. Metrics: register_all() and increment paths smoke-tested.
//  10. End-to-end: sign → replay check → nonce validate → OFA check → integrity check.

use std::sync::Arc;
use secp256k1::{SecretKey, Secp256k1};

use crate::error::SecurityError;
use crate::integrity::{IntegrityRegistry, StrategyEntry, StrategyFreezeGuard};
use crate::key_manager::{KeyManager, KeyRotationState, ROTATION_WINDOW_BLOCKS};
use crate::metrics;
use crate::ofa::{
    default_rule_set, OfaComplianceInput, OfaRuleRegistry, OfaRuleSet, OfaRule,
};
use crate::replay::{NonceRegistry, ReplayGuard};
use crate::signer::{BlueprintSigner, keccak256, secret_key_to_address};

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn sk(byte: u8) -> SecretKey { SecretKey::from_slice(&[byte; 32]).unwrap() }

fn make_signer(byte: u8) -> (BlueprintSigner, [u8; 20]) {
    let secp = Secp256k1::new();
    let secret = sk(byte);
    let addr = secret_key_to_address(&secp, &secret);
    let km = Arc::new(KeyManager::from_secret_key(secret));
    (BlueprintSigner::new(Arc::clone(&km)), addr)
}

fn blueprint_hash(n: u8) -> [u8; 32] {
    keccak256(&[n; 64])
}

fn sa_entry() -> StrategyEntry {
    StrategyEntry {
        strategy_id:      "SA".into(),
        bytecode_hash:    [0xab; 32],
        contract_address: [0x01; 20],
        min_phase:        1,
    }
}

// ─── 1. Sign → Verify → Replay pipeline ──────────────────────────────────────

#[test]
fn sign_verify_replay_full_pipeline() {
    let (signer, addr) = make_signer(0x10);
    let guard          = ReplayGuard::new();
    let hash           = blueprint_hash(1);

    // Step 1: sign
    let bundle = signer.sign(&hash).unwrap();
    assert_eq!(bundle.blueprint_hash, hash);

    // Step 2: verify signature
    assert!(signer.verify(&bundle, &addr).is_ok());

    // Step 3: replay guard — first submission passes
    assert!(guard.check_and_mark(&hash, 42161).is_ok());

    // Step 4: second submission is a replay
    assert!(matches!(
        guard.check_and_mark(&hash, 42161),
        Err(SecurityError::ReplayDetected { .. })
    ));
}

// ─── 2. Key rotation ─────────────────────────────────────────────────────────

#[test]
fn dual_key_window_both_keys_valid_then_rotation_completes() {
    let secp    = Secp256k1::new();
    let km      = Arc::new(KeyManager::from_secret_key(sk(0x01)));
    let signer  = BlueprintSigner::new(Arc::clone(&km));

    let active_addr  = km.active_address();
    let new_sk       = sk(0x02);
    let pending_addr = secret_key_to_address(&secp, &new_sk);

    // Initiate rotation at block 1000
    let state = km.initiate_rotation(new_sk, 1000).unwrap();
    assert!(matches!(state, KeyRotationState::Rotating { .. }));

    // Both keys must be accepted within the window
    let window_block = 1000 + ROTATION_WINDOW_BLOCKS / 2;
    assert!(km.accepts_address(&active_addr,  window_block));
    assert!(km.accepts_address(&pending_addr, window_block));

    // Pending key rejected AFTER window close
    let after_window = 1000 + ROTATION_WINDOW_BLOCKS + 1;
    assert!(!km.accepts_address(&pending_addr, after_window));

    // Advance past window → promotion
    km.on_new_block(after_window);
    assert_eq!(km.active_address(), pending_addr);
    assert!(!km.rotation_state().is_rotating());

    // Sign with new key; verify using new address
    let hash   = blueprint_hash(2);
    let bundle = signer.sign(&hash).unwrap();
    assert!(signer.verify(&bundle, &pending_addr).is_ok());
}

#[test]
fn cancel_rotation_restores_single_key_state() {
    let km = Arc::new(KeyManager::from_secret_key(sk(0x05)));
    let original_addr = km.active_address();
    km.initiate_rotation(sk(0x06), 500).unwrap();
    assert!(km.rotation_state().is_rotating());
    km.cancel_rotation();
    assert!(!km.rotation_state().is_rotating());
    assert_eq!(km.active_address(), original_addr);
}

// ─── 3. Nonce progression ─────────────────────────────────────────────────────

#[test]
fn nonce_lifecycle_validate_advance_validate() {
    let reg = NonceRegistry::new();

    // LA on Arbitrum, starting nonce
    assert!(reg.validate("LA", 42161, 0).is_ok());
    reg.advance("LA", 42161).unwrap();
    assert!(reg.validate("LA", 42161, 1).is_ok());

    // Old nonce rejected
    assert!(matches!(
        reg.validate("LA", 42161, 0),
        Err(SecurityError::NonceMismatch { .. })
    ));

    // Advance again
    reg.advance("LA", 42161).unwrap();
    assert_eq!(reg.next_nonce("LA", 42161), 2);
}

#[test]
fn nonces_scoped_per_chain_and_strategy() {
    let reg = NonceRegistry::new();
    reg.advance("SA", 42161).unwrap(); // Arbitrum SA → nonce 1
    assert_eq!(reg.next_nonce("SA", 42161), 1);
    assert_eq!(reg.next_nonce("SA", 1),     0); // Ethereum SA still at 0
    assert_eq!(reg.next_nonce("LA", 42161), 0); // LA still at 0
}

#[test]
fn on_chain_sync_overrides_local_nonce() {
    let reg = NonceRegistry::new();
    reg.advance("MSA", 42161).unwrap();
    assert_eq!(reg.next_nonce("MSA", 42161), 1);

    reg.on_chain_nonce_sync("MSA", 42161, 42);
    assert_eq!(reg.next_nonce("MSA", 42161), 42);
    assert!(reg.validate("MSA", 42161, 42).is_ok());
}

// ─── 4. OFA compliance combination ───────────────────────────────────────────

#[test]
fn all_four_ofa_rules_enforced() {
    let reg = OfaRuleRegistry::with_default_rules();

    // Each rule independently triggers its error
    let test_cases: Vec<(Box<dyn Fn(&mut OfaComplianceInput)>, &str)> = vec![
        (Box::new(|i: &mut OfaComplianceInput| i.has_consent_sig = false),       "missing_consent"),
        (Box::new(|i: &mut OfaComplianceInput| i.excess_slippage_bps = 51),      "slippage"),
        (Box::new(|i: &mut OfaComplianceInput| i.user_tx_is_first = false),      "order"),
        (Box::new(|i: &mut OfaComplianceInput| i.target_relay = "public".into()), "relay"),
    ];

    for (mutate, desc) in test_cases {
        let mut input = OfaComplianceInput::compliant("0xtest", "MEV");
        mutate(&mut input);
        assert!(
            !reg.check(&input).is_compliant(),
            "rule '{}' should fail", desc
        );
    }
}

#[test]
fn ofa_hot_swap_takes_immediate_effect() {
    let reg = OfaRuleRegistry::new();
    let input = OfaComplianceInput::compliant("0xabc", "MEV");

    // Not loaded yet → passes with warning
    assert!(reg.check(&input).is_compliant());

    // Load strict rules
    reg.load_rules(default_rule_set());
    assert!(reg.check(&input).is_compliant()); // still compliant

    // Upgrade to version 2 with tighter slippage
    reg.load_rules(OfaRuleSet {
        version:        2,
        effective_date: "2027-01-01".into(),
        rules:          vec![OfaRule::EnforceUserSlippage { max_excess_bps: 0 }],
    });
    assert_eq!(reg.current_version(), Some(2));

    let mut tight_input = input.clone();
    tight_input.excess_slippage_bps = 1;
    assert!(!reg.check(&tight_input).is_compliant());
}

// ─── 5. Integrity: freeze then reject ────────────────────────────────────────

#[test]
fn frozen_strategy_rejects_all_blueprints() {
    let reg = IntegrityRegistry::new();
    reg.register(sa_entry());

    // Unfrozen → passes
    assert!(reg.full_integrity_check("SA", &[0xab; 32]).is_ok());

    // Freeze
    reg.freeze("SA");

    // Both correct and incorrect hashes rejected after freeze
    assert!(matches!(
        reg.full_integrity_check("SA", &[0xab; 32]),
        Err(SecurityError::StrategyFrozen { .. })
    ));
    assert!(matches!(
        reg.full_integrity_check("SA", &[0xff; 32]),
        Err(SecurityError::StrategyFrozen { .. })
    ));
}

#[test]
fn freeze_guard_hot_reads_registry_state() {
    let reg   = IntegrityRegistry::new();
    reg.register(sa_entry());
    let guard = StrategyFreezeGuard::new(Arc::clone(&reg));

    assert!(!guard.is_frozen("SA"));
    reg.freeze("SA");
    // Guard reflects change immediately (shared Arc)
    assert!(guard.is_frozen("SA"));
    assert!(matches!(guard.check("SA"), Err(SecurityError::StrategyFrozen { .. })));
}

// ─── 6. Signer tracks key rotation ───────────────────────────────────────────

#[test]
fn signer_uses_new_key_after_rotation_completes() {
    let secp       = Secp256k1::new();
    let km         = Arc::new(KeyManager::from_secret_key(sk(0x07)));
    let signer     = BlueprintSigner::new(Arc::clone(&km));

    let old_addr   = km.active_address();
    let new_sk     = sk(0x08);
    let new_addr   = secret_key_to_address(&secp, &new_sk);

    km.initiate_rotation(new_sk, 200).unwrap();
    km.on_new_block(200 + ROTATION_WINDOW_BLOCKS + 1); // complete

    // Sign with new key
    let bundle = signer.sign(&blueprint_hash(7)).unwrap();
    assert_eq!(bundle.signer_address, new_addr);
    assert!(signer.verify(&bundle, &new_addr).is_ok());
    assert!(signer.verify(&bundle, &old_addr).is_err());
}

// ─── 7. Concurrent replay guard ──────────────────────────────────────────────

#[test]
fn concurrent_replay_guard_exactly_one_thread_wins() {
    use std::sync::Mutex;
    use std::thread;

    let guard  = Arc::new(ReplayGuard::new());
    let wins   = Arc::new(Mutex::new(0u32));
    let hash   = blueprint_hash(9);

    let handles: Vec<_> = (0..32).map(|_| {
        let g = Arc::clone(&guard);
        let w = Arc::clone(&wins);
        thread::spawn(move || {
            if g.check_and_mark(&hash, 42161).is_ok() {
                *w.lock().unwrap() += 1;
            }
        })
    }).collect();

    for h in handles { h.join().unwrap(); }
    assert_eq!(*wins.lock().unwrap(), 1, "exactly one thread must win the replay race");
}

// ─── 8. SecurityError categorisation ─────────────────────────────────────────

#[test]
fn halt_worthy_errors_are_correctly_identified() {
    let halt_worthy = [
        SecurityError::ReplayDetected { hash: "0x".into(), chain_id: 1 },
        SecurityError::BytecodeMismatch { strategy_id: "SA".into() },
        SecurityError::ChainIdMismatch  { bp_chain: 1, expected_chain: 42161 },
    ];
    for e in &halt_worthy {
        assert!(e.is_halt_worthy(), "{} should be halt-worthy", e);
    }

    let non_halt = [
        SecurityError::MissingConsentSig { blueprint_hash: "0x".into() },
        SecurityError::SlippageExceeded  { excess_bps: 51, max_bps: 50 },
        SecurityError::BundleOrderViolation,
        SecurityError::NonPrivateRelay   { relay: "public".into() },
    ];
    for e in &non_halt {
        assert!(!e.is_halt_worthy(), "{} should NOT be halt-worthy", e);
    }
}

#[test]
fn ofa_errors_are_correctly_categorised() {
    let ofa_errors = [
        SecurityError::MissingConsentSig { blueprint_hash: "0x".into() },
        SecurityError::SlippageExceeded  { excess_bps: 51, max_bps: 50 },
        SecurityError::BundleOrderViolation,
        SecurityError::NonPrivateRelay   { relay: "x".into() },
    ];
    for e in &ofa_errors {
        assert!(e.is_ofa_violation(), "{} should be OFA violation", e);
    }

    let non_ofa = [
        SecurityError::ReplayDetected { hash: "0x".into(), chain_id: 1 },
        SecurityError::BytecodeMismatch { strategy_id: "SA".into() },
        SecurityError::NoActiveKey,
    ];
    for e in &non_ofa {
        assert!(!e.is_ofa_violation(), "{} should NOT be OFA violation", e);
    }
}

// ─── 9. Metrics smoke test ────────────────────────────────────────────────────

#[test]
fn metrics_register_and_increment_without_panic() {
    metrics::register_all();

    // Exercise every metric counter at least once
    let (signer, addr) = make_signer(0x20);
    let _ = signer.sign(&blueprint_hash(20));

    let guard = ReplayGuard::new();
    let _ = guard.check_and_mark(&blueprint_hash(21), 42161);
    let _ = guard.check_and_mark(&blueprint_hash(21), 42161); // triggers REPLAY_ATTEMPTS

    let km = Arc::new(KeyManager::from_secret_key(sk(0x21)));
    let _ = km.initiate_rotation(sk(0x22), 0); // triggers KEY_ROTATIONS

    let reg = IntegrityRegistry::new();
    reg.register(sa_entry());
    let _ = reg.check_bytecode("SA", &[0xff; 32]); // triggers BYTECODE_FAILURES
    reg.freeze("SA");                               // triggers STRATEGY_FREEZES
    let _ = reg.check_frozen("SA");                 // triggers FROZEN_STRATEGY_ATTEMPTS

    let ofa = OfaRuleRegistry::with_default_rules();
    let bad = {
        let mut i = OfaComplianceInput::compliant("0x", "MEV");
        i.has_consent_sig = false;
        i
    };
    ofa.check(&bad); // triggers OFA_CHECKS
}

// ─── 10. Full end-to-end security pipeline ────────────────────────────────────

#[test]
fn end_to_end_security_pipeline_passes() {
    metrics::register_all();

    let chain_id = 42161u64;
    let hash     = blueprint_hash(42);

    // Signer
    let (signer, addr) = make_signer(0x42);
    let bundle = signer.sign(&hash).unwrap();
    signer.verify(&bundle, &addr).unwrap();

    // Replay guard
    let replay = ReplayGuard::new();
    replay.check_and_mark(&hash, chain_id).unwrap();

    // Nonce registry
    let nonces = NonceRegistry::new();
    nonces.validate("LA", chain_id, 0).unwrap();
    nonces.advance("LA", chain_id).unwrap();

    // OFA compliance
    let ofa = OfaRuleRegistry::with_default_rules();
    let input = OfaComplianceInput::compliant(&hex::encode(hash), "MEV");
    assert!(ofa.check(&input).is_compliant());

    // Integrity
    let integrity = IntegrityRegistry::new();
    integrity.register(StrategyEntry {
        strategy_id:      "LA".into(),
        bytecode_hash:    [0x42; 32],
        contract_address: [0x03; 20],
        min_phase:        3,
    });
    integrity.full_integrity_check("LA", &[0x42; 32]).unwrap();
}

#[test]
fn end_to_end_replay_on_second_submission_is_blocked() {
    let hash   = blueprint_hash(43);
    let replay = ReplayGuard::new();

    replay.check_and_mark(&hash, 42161).unwrap();

    // Second submission must fail
    assert!(matches!(
        replay.check_and_mark(&hash, 42161),
        Err(SecurityError::ReplayDetected { .. })
    ));
}

#[test]
fn end_to_end_bytecode_tampering_is_detected() {
    let reg = IntegrityRegistry::new();
    reg.register(StrategyEntry {
        strategy_id:      "MEV".into(),
        bytecode_hash:    [0x77; 32],
        contract_address: [0x04; 20],
        min_phase:        4,
    });

    // Tampered bytecode (different hash)
    assert!(matches!(
        reg.full_integrity_check("MEV", &[0x88; 32]),
        Err(SecurityError::BytecodeMismatch { .. })
    ));
}