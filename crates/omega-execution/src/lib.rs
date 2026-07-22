// crates/omega-execution/src/lib.rs
//
// omega-execution — bridges ExecutionBlueprint (omega-dag/omega-hot-path/
// omega-zk) to omega-relay::MultiRelayClient, gated by omega-risk's kill
// switch and 15 pre-trade checks. See ExecutionPipelineSpecification.md
// for the full design; this crate is the "THIS DOCUMENT: the missing
// stage" box from that document's §2 diagram, and resolves that
// document's §12 open questions — see pipeline.rs's module doc comment
// for each resolution and its rationale.
//
// ## What this crate does NOT implement (by design, not oversight)
//
// - Raw Arbitrum/Ethereum transaction signing. See `signer::
//   TransactionSigner`'s doc comment: no implementation of this trait
//   exists anywhere in the omega-engine workspace. `ExecutionPipeline`
//   is generic over `S: TransactionSigner` specifically so it can be
//   fully built and tested today without fabricating one.
// - Relay client construction. See `relay_factory::RelayClientFactory`'s
//   doc comment: `HttpRelayClient::new` has zero production call sites
//   anywhere in the workspace, confirmed by exhaustive grep. Building
//   real relay clients needs a secrets source (endpoint URLs, RelayAuth)
//   and a translation from `omega_core::config::RelayConfig` to
//   `omega_relay::config::RelayConfig` — see production-integration-plan.md
//   Gaps 2/3/4 for the full breakdown.
// - Flashloan provider address -> protocol-name resolution (needed for
//   the no-self-flash pre-trade check). No such table exists anywhere in
//   the workspace; `pipeline::resolve_flashloan_provider_id` fails closed
//   for any non-zero flashloan address rather than silently defeating
//   that safety check with an unmatchable placeholder string.
// - Stage 7 (confirmation reconciliation). Already fully implemented as
//   `omega_relay::MultiRelayClient::reconcile_inclusions` — this crate
//   doesn't wrap it, since it just needs a periodic caller-owned
//   `tokio::time::interval` loop (belongs in the binary), and because
//   wiring its output into `KillSwitchRegistry::record_outcome` requires
//   confirming `ConfirmationResult`'s exact field set against
//   `confirmation.rs`, which was not read while building this crate.
// - Construction of `KillSwitchRegistry` / `MultiRelayClient` /
//   `ExecutionDag` with real production values — those require real
//   deployment configuration (relay endpoints + auth, kill-switch
//   thresholds, DAG slot capacities) this crate has no basis to invent.
//   `ExecutionPipeline::new` takes them as already-constructed `Arc`s.
//
// See production-integration-plan.md for the complete, sequenced gap
// inventory (12 items) covering everything above plus items outside this
// crate entirely (deployment manifests, main.rs wiring order, reorg-event
// receiver ownership).

pub mod background_tasks;
pub mod config_translation;
pub mod error;
pub mod idempotency;
pub mod pipeline;
pub mod relay_factory;
pub mod signer;
pub mod transform;

pub use background_tasks::{run_idempotency_eviction_loop, run_reorg_event_drain_loop};
pub use config_translation::{
    translate_relay_config, RelayBootstrapInputs, TranslatedRelayConfig, UnmappedRelayConfigField,
};
pub use error::ExecutionError;
pub use idempotency::IdempotencyCache;
pub use pipeline::{ExecutionOutcome, ExecutionPipeline};
pub use relay_factory::{RelayClientFactory, UnconfiguredRelayClientFactory};
pub use signer::{SignedTransaction, TransactionSigner, UnconfiguredSigner};
pub use transform::{build_bundle_payload, ARBITRUM_BLOCK_TIME_MS};