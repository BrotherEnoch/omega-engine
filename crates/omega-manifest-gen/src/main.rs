// crates/omega-manifest-gen/src/main.rs
//
// gen-manifest — C4-A tooling: produces a real config/deployment_manifest.toml
// from LIVE, chain-verified data. Never fabricates a bytecode hash or accepts
// one supplied by the caller — every hash in the output came from a real
// `eth_getCode` read against the RPC endpoint the caller pointed this tool at,
// hashed with the same function the EVM itself uses for `EXTCODEHASH`.
//
// ## Why this exists (see the "C4-A" thread in src/main.rs's own doc comments)
//
// `IntegrityRegistry` — and therefore check 4 (bytecode whitelist) — is
// permanently empty in every environment until a real
// `config/deployment_manifest.toml` exists. That file cannot be honestly
// generated without a real deployment: guessing a plausible address or
// hash would defeat the exact control the manifest exists to enforce (see
// `omega_security::integrity`'s own validation, which src/main.rs's "C4"
// doc comment describes as rejecting malformed hex, wrong length, and
// all-zero placeholder data). This tool is the missing piece BETWEEN a
// real deployment and that file: it takes a real deployed address, reads
// the real bytecode the chain is currently running at that address, and
// writes exactly that.
//
// ## Two input modes
//
//   --manual <path>            A hand-written JSON or TOML list of
//                               {strategy_id, contract_address, min_phase}
//                               triples — see `ManualEntry` below. JSON
//                               uses a top-level array; TOML uses
//                               `[[entries]]` because TOML documents are
//                               table-shaped at the root.
//
//   --forge-broadcast <path> \
//   --strategy-map <path>      `forge script --broadcast`'s own
//                               `run-latest.json` output, cross-referenced
//                               against a small caller-supplied map of
//                               `{"ContractName": {"strategy_id": "...",
//                               "min_phase": N}}` — forge knows contract
//                               names and addresses, not this codebase's
//                               strategy_id/min_phase semantics, so the
//                               map is how the two get connected.
//
// Both modes produce the same intermediate `Candidate` list, which then
// goes through IDENTICAL on-chain verification and validation before
// anything is written.
//
// ## `--manual` TOML shape
//
// JSON manual files deserialize directly into `Vec<ManualEntry>` because
// JSON permits a bare array at the document root. TOML does not: a TOML
// document root is a table. For that reason `.toml` manual files use an
// explicit `[[entries]]` array of tables, deserialized through
// `ManualTomlFile`. This behavior is covered by unit tests; bare TOML
// array expressions and unrelated table arrays such as `[[x]]` are
// intentionally rejected rather than guessed.
//
// ## What is and is NOT verified here
//
// VERIFIED, for real, against the RPC endpoint supplied:
//   - `eth_chainId` matches `--chain-id` (via OmegaRpcClient's own
//     `verify_chain_id`, unconditionally applied on every connect — see
//     omega-rpc's client.rs).
//   - Each `contract_address` has NON-EMPTY deployed code right now.
//   - `bytecode_hash` is `keccak256(runtime_code)` — the SAME value the
//     EVM computes for `EXTCODEHASH` on a non-empty-code account (per the
//     Yellow Paper / EIP-1052: EXTCODEHASH of an existing, non-empty-code
//     account equals keccak256 of that account's code; the empty-code
//     special case, `keccak256("")`, is excluded here entirely by the
//     non-empty-code check above). This is the SAME value
//     `OmegaOrchestrator.sol`'s `execute()` compares via
//     `stratAddr.codehash` — see this file's module-level doc comment.
//
// NOT verified, and NOT this tool's job:
//   - That `contract_address` is actually the CORRECT/intended strategy
//     contract, as opposed to some other real contract at that address.
//     That's an operational/deployment-process guarantee this tool has no
//     way to check — it can only tell you "this address has this code
//     right now," not "this is the contract you meant to deploy."
//   - `min_phase`'s correctness — taken as given from the input file,
//     never inferred or defaulted (see `ManualEntry`/`StrategyMapValue`:
//     both REQUIRE this field, on purpose — a missing phase gate is a
//     security-relevant omission this tool refuses to silently default
//     to 0 for).
//
// ## Assumptions about omega_security's manifest shape — STATUS AS OF THIS
// REVISION: field names/types and strategy_id casing both CONFIRMED
// against real source; both also now real-compiler-confirmed (see below)
//
// This tool was originally written WITHOUT access to
// `crates/omega-security/src/integrity.rs`'s real source, and everything
// below was inferred from doc-comment prose in src/main.rs. That real
// source, plus `crates/omega-core/src/types/blueprint.rs` and
// `crates/omega-core/src/types/strategy_registry_key.rs`, have since been
// reviewed directly, and this crate now compiles clean under
// `cargo build --workspace --release`, `cargo check --workspace`,
// `cargo test --workspace`, and `cargo clippy --all-targets -D warnings`.
// Status, itemized:
//
//   1. FIELD NAMES/TYPES — CONFIRMED. `DeploymentManifest { strategies:
//      Vec<StrategyDeployment> }` and `StrategyDeployment { strategy_id:
//      String, bytecode_hash: String, contract_address: String, min_phase:
//      u8 }` match this file's local mirror (`ManifestFile`/
//      `ManifestEntry` below) exactly — same field names, same types, no
//      `#[serde(rename = "...")]` anywhere in the real struct definitions.
//      This tool still deliberately does NOT import
//      `omega_security::DeploymentManifest` directly and construct it —
//      it continues to serialize the independent local mirror struct, so
//      a future drift between the two (if either is edited without the
//      other) produces a bad-but-inspectable TOML file rather than a
//      compile error inside a crate this tool doesn't depend on. That
//      design choice stands even though the immediate reason for it
//      (an unverified guess) no longer applies.
//   2. STRATEGY_ID CASING — CONFIRMED, directly, not just inferred from
//      test fixtures. `omega_core::types::blueprint::StrategyId`'s real
//      `impl std::fmt::Display` maps `Sa → "SA"`, `Cnry → "CNRY"`,
//      `Msa → "MSA"`, `La → "LA"`, `Mev → "MEV"` — all uppercase, no
//      underscores or other separators. This is independently pinned
//      against regression by `strategy_registry_key.rs`'s own
//      `display_strings_are_pinned` test (which asserts these exact
//      literal strings) and `registry_keys_are_stable` (which asserts
//      `keccak256` of those same literals) — that file's own header notes
//      changing `Display`'s output is a load-bearing, deliberately
//      hard-to-silently-change on-chain-registry-key invariant.
//   3. `Provider::get_code_at(address)`'s signature (no block-tag
//      argument) — previously flagged as unverified against a real
//      compile, unlike every other alloy-facing call in this codebase.
//      Now CONFIRMED: this crate builds clean under the pinned alloy
//      version with exactly this call shape.
//   4. Still NOT this tool's job, unchanged: confirming
//      `contract_address` is the INTENDED strategy contract (see "What is
//      and is NOT verified here" above) and confirming `min_phase`'s
//      correctness.
//
// Before trusting a generated manifest in production, still run
// `main.rs`'s own `load_deployment_manifest` + `strategy_entries_from_manifest`
// against it (e.g. a small integration test, or running the engine pointed
// at it in a non-production environment) — this file's own validation
// (hex, length, non-placeholder, and now casing) reduces but does not
// eliminate the need for that end-to-end check (it still can't confirm
// `contract_address` is the *intended* contract, per item 4 above).

use std::collections::HashMap;
use std::path::PathBuf;

use alloy_primitives::{keccak256, Address};
use anyhow::{bail, Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// CLI
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "gen-manifest",
    about = "Generate a real, chain-verified config/deployment_manifest.toml (C4-A)"
)]
struct Cli {
    /// Manual input file (JSON or TOML, detected by extension) — a list
    /// of {strategy_id, contract_address, min_phase}. JSON uses a top-level
    /// array; TOML uses [[entries]]. Mutually exclusive with
    /// --forge-broadcast/--strategy-map.
    #[arg(long)]
    manual: Option<PathBuf>,

    /// forge's own `run-latest.json` broadcast output. Requires
    /// --strategy-map. Mutually exclusive with --manual.
    #[arg(long)]
    forge_broadcast: Option<PathBuf>,

    /// Maps forge contract names to {strategy_id, min_phase} — required
    /// alongside --forge-broadcast. JSON or TOML, detected by extension.
    #[arg(long)]
    strategy_map: Option<PathBuf>,

    /// WebSocket RPC endpoint. Falls back to $ARBITRUM_RPC_URL (the same
    /// env var src/main.rs's own engine requires) if not given.
    #[arg(long)]
    rpc_url: Option<String>,

    /// Expected chain ID — verified for real against the RPC endpoint's
    /// own eth_chainId via OmegaRpcClient (see module doc comment).
    #[arg(long, default_value_t = 42_161)]
    chain_id: u64,

    /// Output path for the generated manifest.
    #[arg(long, default_value = "config/deployment_manifest.toml")]
    output: PathBuf,

    /// Overwrite an existing output file. Without this flag, an existing
    /// file at --output causes this tool to abort rather than silently
    /// replace a manifest that may currently be authorizing production
    /// strategies.
    #[arg(long, default_value_t = false)]
    force: bool,

    /// Resolve and validate everything, print the result, but do not
    /// write the output file.
    #[arg(long, default_value_t = false)]
    dry_run: bool,

    /// Allow strategy_id values outside the known set (SA, MSA, LA, MEV
    /// — case-sensitive, see `KNOWN_STRATEGY_IDS`'s own doc comment on
    /// why case matters). Without this flag, an unrecognized strategy_id
    /// aborts the run rather than silently writing a manifest entry that
    /// will never match anything `resolve_strategy_bytecode_hash` looks
    /// up.
    #[arg(long, default_value_t = false)]
    allow_unknown_strategy_id: bool,
}

/// Strategy IDs this tool recognizes without --allow-unknown-strategy-id.
///
/// CASE: uppercase (`"SA"`, `"MSA"`, `"LA"`, `"MEV"`) — CONFIRMED directly
/// against `omega_core::types::blueprint::StrategyId`'s real
/// `impl std::fmt::Display`, which maps each variant to exactly these
/// strings (`Sa → "SA"`, `Msa → "MSA"`, `La → "LA"`, `Mev → "MEV"`,
/// `Cnry → "CNRY"`). That mapping is independently pinned against
/// regression by `strategy_registry_key.rs`'s own
/// `display_strings_are_pinned` and `registry_keys_are_stable` tests —
/// that file's header notes changing `Display`'s output would silently
/// break every strategy's on-chain registry key, so this casing is about
/// as unlikely to drift as any value in this codebase gets.
///
/// `resolve_strategy_bytecode_hash` in src/main.rs does an exact string
/// comparison (`e.strategy_id == id_str` where `id_str =
/// strategy_id.to_string()`) against this same `Display` output — so this
/// list is not just plausible, it is known to match exactly what that
/// lookup requires. See `known_strategy_ids_are_uppercase_and_exclude_cnry`
/// in this file's own test module for a regression guard on the casing
/// property itself.
///
/// CNRY is deliberately excluded from this list — NOT an oversight, and
/// NOT overridden by `integrity.rs`'s own `manifest_phase0_includes_cnry_only`
/// test happening to use `"CNRY"` as a sample manifest string (that test
/// only exercises generic manifest parsing/phase-filtering logic; it
/// establishes nothing about whether CNRY needs or uses a real deployment
/// entry in production). Per `crates/omega-strategies/src/cnry.rs`'s own
/// module doc comment, `CnryStrategy::build_blueprint` always returns
/// `Err` before constructing a real blueprint, and `expected_bytecode_hash()`
/// returns a fixed `B256::ZERO` sentinel that is never sourced from
/// `IntegrityRegistry`. `src/main.rs`'s own scoring loop additionally
/// skips canary strategies outright (`if strategy.strategy_id().is_canary()
/// { continue; }`) before reaching the code path
/// (`resolve_strategy_bytecode_hash`) that would ever consult a manifest
/// entry for it. CNRY is not part of the IntegrityRegistry-checked
/// strategy set this manifest exists to authorize; a manifest entry for it
/// would be inert. Pass --allow-unknown-strategy-id if you have a specific
/// reason to register it anyway.
const KNOWN_STRATEGY_IDS: [&str; 4] = ["SA", "MSA", "LA", "MEV"];

// ─────────────────────────────────────────────────────────────────────────────
// Input types
// ─────────────────────────────────────────────────────────────────────────────

/// One entry in a `--manual` input file.
#[derive(Debug, Clone, Deserialize)]
struct ManualEntry {
    strategy_id: String,
    contract_address: String,
    /// REQUIRED, never defaulted — see module doc comment on why this
    /// tool refuses to guess a phase gate.
    min_phase: u8,
}

/// TOML wrapper for `--manual foo.toml`.
///
/// JSON can represent `Vec<ManualEntry>` directly at the document root;
/// TOML cannot, so TOML manual files use:
///
/// ```toml
/// [[entries]]
/// strategy_id = "SA"
/// contract_address = "0x..."
/// min_phase = 1
/// ```
#[derive(Debug, Clone, Deserialize)]
struct ManualTomlFile {
    entries: Vec<ManualEntry>,
}

/// One entry in a `--strategy-map` file, keyed by forge contract name.
#[derive(Debug, Clone, Deserialize)]
struct StrategyMapValue {
    strategy_id: String,
    min_phase: u8,
}

/// Minimal subset of forge's `run-latest.json` broadcast output — only
/// the fields this tool actually reads. Real forge output has many more
/// fields (receipts, gas, etc.); deserializing into a struct with only
/// these fields and no `#[serde(deny_unknown_fields)]` means the extra
/// fields are simply ignored, which is what we want here.
///
/// FIELD NAMES: forge's broadcast JSON uses camelCase
/// (`contractName`, `contractAddress`, `transactionType`) — confirmed
/// against forge's documented broadcast artifact format, not re-verified
/// against a live forge run in this session. If a real `run-latest.json`
/// fails to parse against this struct, check those exact key names
/// first.
#[derive(Debug, Clone, Deserialize)]
struct ForgeBroadcast {
    transactions: Vec<ForgeTransaction>,
}

#[derive(Debug, Clone, Deserialize)]
struct ForgeTransaction {
    #[serde(rename = "contractName")]
    contract_name: Option<String>,
    #[serde(rename = "contractAddress")]
    contract_address: Option<String>,
    #[serde(rename = "transactionType")]
    transaction_type: Option<String>,
}

/// A fully-resolved (but not yet chain-verified) candidate manifest
/// entry, after either input mode has been normalized.
#[derive(Debug, Clone)]
struct Candidate {
    strategy_id: String,
    contract_address: Address,
    min_phase: u8,
}

// ─────────────────────────────────────────────────────────────────────────────
// Output types (local mirror — confirmed to match the real
// omega_security::integrity::{DeploymentManifest, StrategyDeployment}
// field-for-field; kept as an independent struct anyway — see the
// module-level doc comment, item 1, for why)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestEntry {
    strategy_id: String,
    /// `0x` + 64 lowercase hex chars — keccak256 of the real, live
    /// runtime bytecode this tool read via eth_getCode.
    bytecode_hash: String,
    /// EIP-55 checksummed `0x...` address.
    contract_address: String,
    min_phase: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestFile {
    strategies: Vec<ManifestEntry>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Input loading
// ─────────────────────────────────────────────────────────────────────────────

/// Loads either JSON or TOML based on file extension, deserializing into
/// `T`. `.json` -> serde_json, `.toml` -> toml; anything else is an
/// error rather than a guess.
fn load_structured<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    match path.extension().and_then(|e| e.to_str()) {
        Some("json") => serde_json::from_str(&raw)
            .with_context(|| format!("parsing {} as JSON", path.display())),
        Some("toml") => {
            toml::from_str(&raw).with_context(|| format!("parsing {} as TOML", path.display()))
        }
        other => bail!(
            "{}: unrecognized extension {:?} — use .json or .toml",
            path.display(),
            other
        ),
    }
}

fn load_manual_candidates(path: &PathBuf) -> Result<Vec<Candidate>> {
    let entries: Vec<ManualEntry> = match path.extension().and_then(|e| e.to_str()) {
        Some("json") => load_structured(path)?,
        Some("toml") => {
            let manual: ManualTomlFile = load_structured(path)?;
            manual.entries
        }
        other => bail!(
            "{}: unrecognized extension {:?} — use .json or .toml",
            path.display(),
            other
        ),
    };
    entries
        .into_iter()
        .map(|e| {
            let contract_address = e
                .contract_address
                .parse::<Address>()
                .with_context(|| format!("parsing contract_address for {}", e.strategy_id))?;
            Ok(Candidate {
                strategy_id: e.strategy_id,
                contract_address,
                min_phase: e.min_phase,
            })
        })
        .collect()
}

fn load_forge_candidates(broadcast_path: &PathBuf, map_path: &PathBuf) -> Result<Vec<Candidate>> {
    let broadcast: ForgeBroadcast = load_structured(broadcast_path)?;
    let map: HashMap<String, StrategyMapValue> = load_structured(map_path)?;

    let mut candidates = Vec::new();
    for tx in broadcast.transactions {
        // Only CREATE (deployment) transactions carry a meaningful
        // contractAddress for our purposes — skip anything else (calls,
        // etc.) rather than guess.
        let is_create = tx
            .transaction_type
            .as_deref()
            .map(|t| t.eq_ignore_ascii_case("CREATE") || t.eq_ignore_ascii_case("CREATE2"))
            .unwrap_or(false);
        if !is_create {
            continue;
        }

        let (Some(name), Some(addr_str)) = (tx.contract_name, tx.contract_address) else {
            continue;
        };

        let Some(mapped) = map.get(&name) else {
            tracing::warn!(
                contract_name = %name,
                "forge broadcast deployed a contract with no entry in --strategy-map \
                 — skipped, not guessed at"
            );
            continue;
        };

        let contract_address = addr_str
            .parse::<Address>()
            .with_context(|| format!("parsing contractAddress for {name}"))?;

        candidates.push(Candidate {
            strategy_id: mapped.strategy_id.clone(),
            contract_address,
            min_phase: mapped.min_phase,
        });
    }

    if candidates.is_empty() {
        bail!(
            "no CREATE transactions in {} matched an entry in {} — nothing to do",
            broadcast_path.display(),
            map_path.display()
        );
    }

    Ok(candidates)
}

/// Resolves the caller's chosen input mode (--manual XOR
/// --forge-broadcast+--strategy-map) into a candidate list, or a
/// descriptive error for every other combination of the three flags.
///
/// Split out from `main()` as its own pure function specifically so this
/// branch logic — six distinct combinations of three `Option` flags — is
/// unit-testable without needing a live RPC endpoint or real CLI
/// invocation. Behavior is unchanged from having this inline in `main()`.
fn load_candidates_from_cli(cli: &Cli) -> Result<Vec<Candidate>> {
    match (&cli.manual, &cli.forge_broadcast, &cli.strategy_map) {
        (Some(manual), None, None) => load_manual_candidates(manual),
        (None, Some(broadcast), Some(map)) => load_forge_candidates(broadcast, map),
        (None, Some(_), None) => bail!("--forge-broadcast requires --strategy-map"),
        (None, None, Some(_)) => bail!("--strategy-map requires --forge-broadcast"),
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            bail!("--manual is mutually exclusive with --forge-broadcast/--strategy-map")
        }
        (None, None, None) => {
            bail!(
                "provide either --manual <path> or --forge-broadcast <path> --strategy-map <path>"
            )
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Validation
// ─────────────────────────────────────────────────────────────────────────────

fn validate_candidates(candidates: &[Candidate], allow_unknown: bool) -> Result<()> {
    if candidates.is_empty() {
        bail!("no candidate entries to process");
    }

    let mut seen_ids = std::collections::HashSet::new();
    for c in candidates {
        if c.strategy_id.trim().is_empty() {
            bail!(
                "empty strategy_id for contract_address {}",
                c.contract_address
            );
        }
        if !seen_ids.insert(c.strategy_id.clone()) {
            bail!(
                "duplicate strategy_id {:?} — a manifest entry is looked up by \
                 strategy_id alone (see resolve_strategy_bytecode_hash in \
                 src/main.rs), so a duplicate here is ambiguous, not just \
                 redundant",
                c.strategy_id
            );
        }
        if c.contract_address.is_zero() {
            bail!(
                "strategy_id {:?}: contract_address is the zero address",
                c.strategy_id
            );
        }
        if !allow_unknown && !KNOWN_STRATEGY_IDS.contains(&c.strategy_id.as_str()) {
            bail!(
                "strategy_id {:?} is not in the known set {:?} (case-sensitive — see \
                 KNOWN_STRATEGY_IDS's own doc comment on why case matters). Pass \
                 --allow-unknown-strategy-id if this is intentional.",
                c.strategy_id,
                KNOWN_STRATEGY_IDS
            );
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Chain verification
// ─────────────────────────────────────────────────────────────────────────────

/// Reads real runtime bytecode via `eth_getCode` and returns
/// `(bytecode_hash, code_len)`. Fails (does not fabricate a fallback
/// value) if the read errors OR the returned code is empty — an empty
/// read means either the address has no contract deployed at all, or a
/// typo/wrong-chain address, and registering that would be exactly the
/// kind of fabricated-looking-real data this tool exists to prevent.
///
/// Not covered by this file's own test suite — see the test module's
/// note on why (requires a live or mocked RPC endpoint).
async fn fetch_bytecode_hash(
    rpc: &omega_rpc::OmegaRpcClient,
    address: Address,
) -> Result<([u8; 32], usize)> {
    // `get_or_connect()` is the same public method every other read in
    // this workspace's OmegaRpcClient goes through (fetch_fee_snapshot,
    // fetch_logs, fetch_chainlink_round, fetch_l1_base_fee_estimate_gwei,
    // fetch_aave_available, ...) — reusing it here keeps this tool
    // inside the same "single shared connection, chain-ID verified on
    // every (re)connect" model as the rest of the engine, rather than
    // opening an independent, unverified connection.
    let provider = rpc
        .get_or_connect()
        .await
        .context("connecting to RPC endpoint")?;

    // `Provider::get_code_at(address)` — CONFIRMED against a real
    // `cargo build --workspace --release` / `cargo check --workspace`
    // pass under the pinned alloy version (previously flagged in this
    // file as an unverified guess; see this file's module-level doc
    // comment, item 3).
    let code = provider
        .get_code_at(address)
        .await
        .with_context(|| format!("eth_getCode failed for {address}"))?;

    if code.is_empty() {
        bail!(
            "{address} has NO deployed code (eth_getCode returned empty) — refusing \
             to register a strategy against an address with nothing deployed. \
             Check the address and the chain this RPC endpoint is actually pointed at."
        );
    }

    // keccak256(runtime_code) == EXTCODEHASH for any account with
    // non-empty code (EIP-1052) — the empty-code special case
    // (keccak256("") for accounts with no code) is excluded by the
    // is_empty() check above, so this is unconditionally the right hash
    // for every candidate that reaches this line. This is the SAME
    // value OmegaOrchestrator.sol's execute() compares via
    // `stratAddr.codehash` — see this file's module-level doc comment.
    let hash = keccak256(code.as_ref());
    Ok((hash.0, code.len()))
}

// ─────────────────────────────────────────────────────────────────────────────
// main
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(true).init();

    let cli = Cli::parse();

    let candidates = load_candidates_from_cli(&cli)?;
    validate_candidates(&candidates, cli.allow_unknown_strategy_id)?;

    // ── Output path guard ──────────────────────────────────────────────────
    if !cli.dry_run && cli.output.exists() && !cli.force {
        bail!(
            "{} already exists — refusing to overwrite a manifest that may \
             currently be authorizing production strategies. Pass --force if \
             you really mean to replace it, or --dry-run to preview without \
             writing.",
            cli.output.display()
        );
    }

    // ── Connect ────────────────────────────────────────────────────────────
    let rpc_url = cli
        .rpc_url
        .clone()
        .or_else(|| std::env::var("ARBITRUM_RPC_URL").ok())
        .context("no --rpc-url given and ARBITRUM_RPC_URL is not set")?;

    tracing::info!(chain_id = cli.chain_id, "connecting to RPC endpoint");
    let rpc = omega_rpc::OmegaRpcClient::connect(omega_rpc::RpcClientConfig::new(
        &rpc_url,
        50, // low rps — this tool makes a handful of calls total, not a
        // sustained load; not the same DEFAULT_RPS the live engine
        // uses in src/main.rs, which is tuned for continuous operation.
        cli.chain_id,
    ))
    .await
    .context(
        "connecting to RPC endpoint (this also verifies eth_chainId matches --chain-id \
         — see OmegaRpcClient::connect)",
    )?;

    // ── Resolve each candidate against real chain state ───────────────────
    let mut entries = Vec::with_capacity(candidates.len());
    for c in &candidates {
        tracing::info!(
            strategy_id = %c.strategy_id,
            address = %c.contract_address,
            "reading live bytecode"
        );
        let (hash, len) = fetch_bytecode_hash(&rpc, c.contract_address)
            .await
            .with_context(|| format!("resolving strategy_id {:?}", c.strategy_id))?;

        tracing::info!(
            strategy_id = %c.strategy_id,
            code_len = len,
            bytecode_hash = %format!("0x{}", hex::encode(hash)),
            "resolved"
        );

        entries.push(ManifestEntry {
            strategy_id: c.strategy_id.clone(),
            bytecode_hash: format!("0x{}", hex::encode(hash)),
            contract_address: c.contract_address.to_checksum(None),
            min_phase: c.min_phase,
        });
    }

    let manifest = ManifestFile {
        strategies: entries,
    };

    let toml_out = toml::to_string_pretty(&manifest).context("serializing manifest to TOML")?;

    // Round-trip sanity check: re-parse what we just produced with the
    // SAME local mirror struct. This catches this tool producing
    // syntactically-broken TOML, which would otherwise fail silently
    // until someone tried to load it with the real engine. It does NOT
    // independently re-verify against the real
    // omega_security::DeploymentManifest shape beyond what's already
    // documented at the module level as confirmed field-for-field.
    let _: ManifestFile =
        toml::from_str(&toml_out).context("round-trip re-parse of generated TOML failed")?;

    if cli.dry_run {
        println!("{toml_out}");
        tracing::info!("--dry-run: not writing to disk");
        return Ok(());
    }

    if let Some(parent) = cli.output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&cli.output, &toml_out)
        .with_context(|| format!("writing {}", cli.output.display()))?;

    tracing::info!(
        path = %cli.output.display(),
        count = manifest.strategies.len(),
        "manifest written"
    );
    tracing::warn!(
        "Field names/types and strategy_id casing have both been confirmed \
         against the real omega_security::integrity and \
         omega_core::types::blueprint source (see this file's module-level \
         doc comment). Still recommended before trusting this file in \
         production: run it through src/main.rs's own \
         load_deployment_manifest + strategy_entries_from_manifest in a \
         non-production environment, since this tool cannot confirm each \
         contract_address is the strategy you actually intended to deploy."
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────
//
// Scope: everything below is testable without a live or mocked RPC
// endpoint — input parsing, candidate validation, CLI mode selection, and
// manifest (de)serialization. `fetch_bytecode_hash` and the RPC connect
// call in `main()` are deliberately NOT covered here: exercising them
// honestly needs either a live Arbitrum RPC endpoint or a mock
// `Provider`, neither of which this file wires up. Adding a fake success
// path for those would be exactly the kind of "looks tested but isn't"
// gap the rest of this codebase goes out of its way to avoid — see this
// file's own "never fabricate" framing throughout. If real coverage of
// that path is wanted, it needs a real integration-test harness (e.g.
// against Anvil), not a unit test in this module.
//
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    // ── Test helpers ───────────────────────────────────────────────────────

    /// Writes `content` to a uniquely-named file in the OS temp directory
    /// with the given extension, and returns its path. Exists so
    /// `load_structured`/`load_manual_candidates`/`load_forge_candidates`
    /// (all of which take a `&PathBuf` and read from disk) can be
    /// exercised without adding a `tempfile`-style crate as a new
    /// dev-dependency. Uniqueness comes from process ID + a per-process
    /// atomic counter + a nanosecond timestamp, which is enough to avoid
    /// collisions across concurrently-running `cargo test` threads
    /// without needing external synchronization.
    fn write_temp_file(content: &str, ext: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "gen_manifest_test_{}_{}_{}.{}",
            std::process::id(),
            n,
            nanos,
            ext
        ));
        let mut f = std::fs::File::create(&path).expect("create temp file for test");
        f.write_all(content.as_bytes())
            .expect("write temp file for test");
        path
    }

    fn candidate(strategy_id: &str, addr_byte: u8, min_phase: u8) -> Candidate {
        Candidate {
            strategy_id: strategy_id.to_string(),
            contract_address: Address::from([addr_byte; 20]),
            min_phase,
        }
    }

    fn base_cli() -> Cli {
        Cli {
            manual: None,
            forge_broadcast: None,
            strategy_map: None,
            rpc_url: None,
            chain_id: 42_161,
            output: PathBuf::from("config/deployment_manifest.toml"),
            force: false,
            dry_run: false,
            allow_unknown_strategy_id: false,
        }
    }

    // ── KNOWN_STRATEGY_IDS ───────────────────────────────────────────────────

    #[test]
    fn known_strategy_ids_are_uppercase() {
        for id in KNOWN_STRATEGY_IDS {
            assert_eq!(
                id.to_string(),
                id.to_uppercase(),
                "{id} must be uppercase — see KNOWN_STRATEGY_IDS's own doc comment"
            );
        }
    }

    #[test]
    fn known_strategy_ids_are_pairwise_distinct() {
        let mut seen = std::collections::HashSet::new();
        for id in KNOWN_STRATEGY_IDS {
            assert!(
                seen.insert(id),
                "duplicate entry in KNOWN_STRATEGY_IDS: {id}"
            );
        }
    }

    #[test]
    fn known_strategy_ids_excludes_cnry() {
        assert!(
            !KNOWN_STRATEGY_IDS.contains(&"CNRY"),
            "CNRY is deliberately excluded — see KNOWN_STRATEGY_IDS's own doc comment"
        );
    }

    // ── validate_candidates ────────────────────────────────────────────────

    #[test]
    fn validate_candidates_rejects_empty_list() {
        assert!(validate_candidates(&[], false).is_err());
    }

    #[test]
    fn validate_candidates_accepts_all_known_strategy_ids() {
        let candidates = vec![
            candidate("SA", 0x01, 1),
            candidate("MSA", 0x02, 2),
            candidate("LA", 0x03, 3),
            candidate("MEV", 0x04, 4),
        ];
        assert!(validate_candidates(&candidates, false).is_ok());
    }

    #[test]
    fn validate_candidates_rejects_unknown_strategy_id_by_default() {
        let candidates = vec![candidate("CNRY", 0x01, 0)];
        assert!(validate_candidates(&candidates, false).is_err());
    }

    #[test]
    fn validate_candidates_allows_unknown_strategy_id_with_flag() {
        let candidates = vec![candidate("CNRY", 0x01, 0)];
        assert!(validate_candidates(&candidates, true).is_ok());
    }

    #[test]
    fn validate_candidates_rejects_wrong_casing() {
        // Casing is load-bearing (see KNOWN_STRATEGY_IDS's own doc
        // comment) — a lowercase strategy_id must not silently pass just
        // because it "looks like" a known one.
        let candidates = vec![candidate("sa", 0x01, 1)];
        assert!(validate_candidates(&candidates, false).is_err());
    }

    #[test]
    fn validate_candidates_rejects_duplicate_strategy_id() {
        let candidates = vec![candidate("SA", 0x01, 1), candidate("SA", 0x02, 1)];
        assert!(validate_candidates(&candidates, false).is_err());
    }

    #[test]
    fn validate_candidates_rejects_zero_address() {
        let candidates = vec![Candidate {
            strategy_id: "SA".to_string(),
            contract_address: Address::ZERO,
            min_phase: 1,
        }];
        assert!(validate_candidates(&candidates, false).is_err());
    }

    #[test]
    fn validate_candidates_rejects_empty_strategy_id() {
        let candidates = vec![candidate("", 0x01, 1)];
        // allow_unknown=true so this fails on the empty-string check
        // specifically, not on the known-set check.
        assert!(validate_candidates(&candidates, true).is_err());
    }

    #[test]
    fn validate_candidates_rejects_whitespace_only_strategy_id() {
        let candidates = vec![candidate("   ", 0x01, 1)];
        assert!(validate_candidates(&candidates, true).is_err());
    }

    #[test]
    fn validate_candidates_accepts_distinct_known_ids_with_distinct_addresses() {
        // Regression guard: two DIFFERENT known strategy_ids must not be
        // rejected as duplicates just because they're both "known" —
        // dedup is keyed on strategy_id value, not known-ness.
        let candidates = vec![candidate("SA", 0x01, 1), candidate("LA", 0x02, 3)];
        assert!(validate_candidates(&candidates, false).is_ok());
    }

    // ── load_structured ──────────────────────────────────────────────────────

    #[test]
    fn load_structured_rejects_unrecognized_extension() {
        let path = write_temp_file("irrelevant content", "yaml");
        let result: Result<Vec<ManualEntry>> = load_structured(&path);
        assert!(result.is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_structured_rejects_missing_file() {
        let path = PathBuf::from("/this/path/should/not/exist/on/any/machine.json");
        let result: Result<Vec<ManualEntry>> = load_structured(&path);
        assert!(result.is_err());
    }

    #[test]
    fn load_structured_rejects_malformed_json() {
        let path = write_temp_file("{not valid json", "json");
        let result: Result<Vec<ManualEntry>> = load_structured(&path);
        assert!(result.is_err());
        let _ = std::fs::remove_file(&path);
    }

    // ── load_manual_candidates (JSON — see module doc comment re: TOML) ──────

    #[test]
    fn load_manual_candidates_parses_valid_json() {
        let path = write_temp_file(
            r#"[{"strategy_id":"MEV","contract_address":"0x0606060606060606060606060606060606060606","min_phase":4}]"#,
            "json",
        );
        let candidates = load_manual_candidates(&path).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].strategy_id, "MEV");
        assert_eq!(candidates[0].min_phase, 4);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_manual_candidates_parses_multiple_entries() {
        let path = write_temp_file(
            r#"[
                {"strategy_id":"SA","contract_address":"0x0101010101010101010101010101010101010101","min_phase":1},
                {"strategy_id":"LA","contract_address":"0x0202020202020202020202020202020202020202","min_phase":3}
            ]"#,
            "json",
        );
        let candidates = load_manual_candidates(&path).unwrap();
        assert_eq!(candidates.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_manual_candidates_rejects_malformed_address() {
        let path = write_temp_file(
            r#"[{"strategy_id":"SA","contract_address":"not-an-address","min_phase":1}]"#,
            "json",
        );
        let result = load_manual_candidates(&path);
        assert!(result.is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_manual_candidates_rejects_short_address() {
        let path = write_temp_file(
            r#"[{"strategy_id":"SA","contract_address":"0x0101","min_phase":1}]"#,
            "json",
        );
        let result = load_manual_candidates(&path);
        assert!(result.is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_manual_candidates_rejects_missing_min_phase() {
        // min_phase is a required field with no Default — see
        // ManualEntry's own doc comment. A file omitting it must fail to
        // parse, not silently default to phase 0.
        let path = write_temp_file(
            r#"[{"strategy_id":"SA","contract_address":"0x0101010101010101010101010101010101010101"}]"#,
            "json",
        );
        let result = load_manual_candidates(&path);
        assert!(result.is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_manual_candidates_parses_valid_toml_entries_wrapper() {
        let path = write_temp_file(
            r#"
                [[entries]]
                strategy_id = "SA"
                contract_address = "0x0101010101010101010101010101010101010101"
                min_phase = 1

                [[entries]]
                strategy_id = "LA"
                contract_address = "0x0202020202020202020202020202020202020202"
                min_phase = 3
            "#,
            "toml",
        );

        let candidates = load_manual_candidates(&path).unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].strategy_id, "SA");
        assert_eq!(candidates[0].min_phase, 1);
        assert_eq!(candidates[1].strategy_id, "LA");
        assert_eq!(candidates[1].min_phase, 3);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_manual_candidates_rejects_toml_with_unrecognized_wrapper_key() {
        let path = write_temp_file(
            r#"
                [[x]]
                strategy_id = "SA"
                contract_address = "0x0101010101010101010101010101010101010101"
                min_phase = 1
            "#,
            "toml",
        );

        let result = load_manual_candidates(&path);
        assert!(result.is_err());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_manual_candidates_rejects_bare_toml_array_expression() {
        let path = write_temp_file(
            r#"[{ strategy_id = "SA", contract_address = "0x0101010101010101010101010101010101010101", min_phase = 1 }]"#,
            "toml",
        );

        let result = load_manual_candidates(&path);
        assert!(result.is_err());

        let _ = std::fs::remove_file(&path);
    }

    // ── load_forge_candidates ─────────────────────────────────────────────────

    #[test]
    fn load_forge_candidates_filters_to_create_and_applies_map() {
        let broadcast = write_temp_file(
            r#"{"transactions":[
                {"contractName":"SaStrategy","contractAddress":"0x0101010101010101010101010101010101010101","transactionType":"CALL"},
                {"contractName":"SaStrategy","contractAddress":"0x0202020202020202020202020202020202020202","transactionType":"CREATE"},
                {"contractName":"UnmappedContract","contractAddress":"0x0303030303030303030303030303030303030303","transactionType":"CREATE"}
            ]}"#,
            "json",
        );
        let map = write_temp_file(
            r#"{"SaStrategy":{"strategy_id":"SA","min_phase":1}}"#,
            "json",
        );

        let candidates = load_forge_candidates(&broadcast, &map).unwrap();
        assert_eq!(
            candidates.len(),
            1,
            "only the CREATE tx for a mapped contract should produce a candidate"
        );
        assert_eq!(candidates[0].strategy_id, "SA");
        assert_eq!(candidates[0].min_phase, 1);

        let _ = std::fs::remove_file(&broadcast);
        let _ = std::fs::remove_file(&map);
    }

    #[test]
    fn load_forge_candidates_treats_create2_as_deployment() {
        let broadcast = write_temp_file(
            r#"{"transactions":[
                {"contractName":"LaStrategy","contractAddress":"0x0404040404040404040404040404040404040404","transactionType":"CREATE2"}
            ]}"#,
            "json",
        );
        let map = write_temp_file(
            r#"{"LaStrategy":{"strategy_id":"LA","min_phase":3}}"#,
            "json",
        );

        let candidates = load_forge_candidates(&broadcast, &map).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].strategy_id, "LA");

        let _ = std::fs::remove_file(&broadcast);
        let _ = std::fs::remove_file(&map);
    }

    #[test]
    fn load_forge_candidates_transaction_type_is_case_insensitive() {
        let broadcast = write_temp_file(
            r#"{"transactions":[
                {"contractName":"SaStrategy","contractAddress":"0x0101010101010101010101010101010101010101","transactionType":"create"}
            ]}"#,
            "json",
        );
        let map = write_temp_file(
            r#"{"SaStrategy":{"strategy_id":"SA","min_phase":1}}"#,
            "json",
        );

        let candidates = load_forge_candidates(&broadcast, &map).unwrap();
        assert_eq!(candidates.len(), 1);

        let _ = std::fs::remove_file(&broadcast);
        let _ = std::fs::remove_file(&map);
    }

    #[test]
    fn load_forge_candidates_errors_when_nothing_matches() {
        let broadcast = write_temp_file(
            r#"{"transactions":[
                {"contractName":"Unmapped","contractAddress":"0x0505050505050505050505050505050505050505","transactionType":"CREATE"}
            ]}"#,
            "json",
        );
        let map = write_temp_file(r#"{}"#, "json");

        let result = load_forge_candidates(&broadcast, &map);
        assert!(result.is_err());

        let _ = std::fs::remove_file(&broadcast);
        let _ = std::fs::remove_file(&map);
    }

    #[test]
    fn load_forge_candidates_errors_when_all_transactions_are_non_create() {
        let broadcast = write_temp_file(
            r#"{"transactions":[
                {"contractName":"SaStrategy","contractAddress":"0x0101010101010101010101010101010101010101","transactionType":"CALL"}
            ]}"#,
            "json",
        );
        let map = write_temp_file(
            r#"{"SaStrategy":{"strategy_id":"SA","min_phase":1}}"#,
            "json",
        );

        let result = load_forge_candidates(&broadcast, &map);
        assert!(result.is_err());

        let _ = std::fs::remove_file(&broadcast);
        let _ = std::fs::remove_file(&map);
    }

    // ── load_candidates_from_cli (CLI mode selection) ─────────────────────────

    #[test]
    fn load_candidates_from_cli_errors_with_no_mode_selected() {
        let cli = base_cli();
        assert!(load_candidates_from_cli(&cli).is_err());
    }

    #[test]
    fn load_candidates_from_cli_errors_when_forge_broadcast_missing_strategy_map() {
        let mut cli = base_cli();
        cli.forge_broadcast = Some(PathBuf::from("whatever.json"));
        assert!(load_candidates_from_cli(&cli).is_err());
    }

    #[test]
    fn load_candidates_from_cli_errors_when_strategy_map_missing_forge_broadcast() {
        let mut cli = base_cli();
        cli.strategy_map = Some(PathBuf::from("whatever.json"));
        assert!(load_candidates_from_cli(&cli).is_err());
    }

    #[test]
    fn load_candidates_from_cli_errors_when_manual_and_forge_broadcast_both_given() {
        let mut cli = base_cli();
        cli.manual = Some(PathBuf::from("manual.json"));
        cli.forge_broadcast = Some(PathBuf::from("broadcast.json"));
        assert!(load_candidates_from_cli(&cli).is_err());
    }

    #[test]
    fn load_candidates_from_cli_errors_when_manual_and_strategy_map_both_given() {
        let mut cli = base_cli();
        cli.manual = Some(PathBuf::from("manual.json"));
        cli.strategy_map = Some(PathBuf::from("map.json"));
        assert!(load_candidates_from_cli(&cli).is_err());
    }

    #[test]
    fn load_candidates_from_cli_uses_manual_mode_when_only_manual_given() {
        let path = write_temp_file(
            r#"[{"strategy_id":"SA","contract_address":"0x0101010101010101010101010101010101010101","min_phase":1}]"#,
            "json",
        );
        let mut cli = base_cli();
        cli.manual = Some(path.clone());

        let candidates = load_candidates_from_cli(&cli).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].strategy_id, "SA");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_candidates_from_cli_uses_forge_mode_when_both_forge_flags_given() {
        let broadcast = write_temp_file(
            r#"{"transactions":[
                {"contractName":"SaStrategy","contractAddress":"0x0101010101010101010101010101010101010101","transactionType":"CREATE"}
            ]}"#,
            "json",
        );
        let map = write_temp_file(
            r#"{"SaStrategy":{"strategy_id":"SA","min_phase":1}}"#,
            "json",
        );

        let mut cli = base_cli();
        cli.forge_broadcast = Some(broadcast.clone());
        cli.strategy_map = Some(map.clone());

        let candidates = load_candidates_from_cli(&cli).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].strategy_id, "SA");

        let _ = std::fs::remove_file(&broadcast);
        let _ = std::fs::remove_file(&map);
    }

    // ── ManifestFile / ManifestEntry (de)serialization ────────────────────────

    #[test]
    fn manifest_file_round_trips_through_toml() {
        let manifest = ManifestFile {
            strategies: vec![ManifestEntry {
                strategy_id: "SA".to_string(),
                bytecode_hash: format!("0x{}", "ab".repeat(32)),
                contract_address: "0x0101010101010101010101010101010101010101".to_string(),
                min_phase: 1,
            }],
        };
        let toml_out = toml::to_string_pretty(&manifest).unwrap();
        let parsed: ManifestFile = toml::from_str(&toml_out).unwrap();

        assert_eq!(parsed.strategies.len(), 1);
        assert_eq!(parsed.strategies[0].strategy_id, "SA");
        assert_eq!(
            parsed.strategies[0].bytecode_hash,
            manifest.strategies[0].bytecode_hash
        );
        assert_eq!(
            parsed.strategies[0].contract_address,
            manifest.strategies[0].contract_address
        );
        assert_eq!(parsed.strategies[0].min_phase, 1);
    }

    #[test]
    fn manifest_file_round_trips_with_multiple_entries() {
        let manifest = ManifestFile {
            strategies: vec![
                ManifestEntry {
                    strategy_id: "SA".to_string(),
                    bytecode_hash: format!("0x{}", "11".repeat(32)),
                    contract_address: "0x0101010101010101010101010101010101010101".to_string(),
                    min_phase: 1,
                },
                ManifestEntry {
                    strategy_id: "LA".to_string(),
                    bytecode_hash: format!("0x{}", "22".repeat(32)),
                    contract_address: "0x0202020202020202020202020202020202020202".to_string(),
                    min_phase: 3,
                },
            ],
        };
        let toml_out = toml::to_string_pretty(&manifest).unwrap();
        let parsed: ManifestFile = toml::from_str(&toml_out).unwrap();
        assert_eq!(parsed.strategies.len(), 2);
    }

    #[test]
    fn manifest_file_empty_strategies_round_trips() {
        // Mirrors integrity.rs's own
        // empty_manifest_produces_empty_entries_not_an_error test on the
        // real DeploymentManifest side — an empty manifest is valid
        // input, not an error, on this tool's output side too.
        let manifest = ManifestFile { strategies: vec![] };
        let toml_out = toml::to_string_pretty(&manifest).unwrap();
        let parsed: ManifestFile = toml::from_str(&toml_out).unwrap();
        assert!(parsed.strategies.is_empty());
    }
}
