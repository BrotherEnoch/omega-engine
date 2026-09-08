// crates/omega-execution/src/lib.rs
//
// omega-execution — bridges ExecutionBlueprint to omega-relay::MultiRelayClient,
// gated by omega-risk's kill switch and 15 pre-trade checks.

pub mod audit;
pub mod background_tasks;
pub mod config_translation;
pub mod context_assembly;
pub mod error;
pub mod flashloan_provider_table;
pub mod idempotency;
pub mod inflight;
pub mod pipeline;
pub mod relay_factory;
pub mod signer;
pub mod stage7;
pub mod transform;

pub use audit::{log_confirmed, log_rejected, log_submitted, BlueprintAuditView, RejectReason};
pub use background_tasks::{run_idempotency_eviction_loop, run_reorg_event_drain_loop};
pub use config_translation::{
    translate_relay_config, RelayBootstrapInputs, TranslatedRelayConfig, UnmappedRelayConfigField,
};
pub use context_assembly::{
    assemble_check_context, AccountExposureSource, AssemblyInput, CompetitionInputs, GasReadout,
    LatestNonceSource, LiveContextHandles, RiskScoreSource, StrategyLimits,
};
pub use error::ExecutionError;
pub use flashloan_provider_table::{
    arbitrum_flashloan_provider_table, resolve_flashloan_provider_id as resolve_provider_address,
};
pub use idempotency::IdempotencyCache;
pub use inflight::{
    default_journal_path, open_default_journal, InFlightJournal, InFlightRecord, InFlightStatus,
};
pub use pipeline::{ExecutionOutcome, ExecutionPipeline};
pub use relay_factory::{RelayClientFactory, UnconfiguredRelayClientFactory};
pub use signer::{SignedTransaction, TransactionSigner, UnconfiguredSigner};
pub use stage7::{
    process_confirmation_results, process_confirmation_results_async,
    process_confirmation_results_with_lookup, run_stage7_interval_loop,
    run_stage7_reconciliation_loop, AsyncRealizedProfitLookup, NoRealizedProfitLookup,
    ProcessedConfirmation, RealizedProfitLookup, Stage7Config,
};
pub use transform::{build_bundle_payload, ARBITRUM_BLOCK_TIME_MS};
