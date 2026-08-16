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
//                               triples — see `ManualEntry` below.
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
//     `stratAddr.codehash != expectedHash` (see that contract's own
//     "## 8. Bytecode integrity" check) and the same value
//     `IntegrityRegistry`/`build_check_context`'s `strategy_bytecode_hash`
//     field is meant to hold off-chain — so a manifest built by this tool
//     should make the on-chain and off-chain checks agree BY
//     CONSTRUCTION, not by two independently-maintained values happening
//     to match.
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
// ## UNVERIFIED ASSUMPTION — flagged loudly, check before trusting this tool
//
// This tool was written WITHOUT access to
// `crates/omega-security/src/integrity.rs`'s real source. Every field
// name and type below (`strategies`, `strategy_id`, `bytecode_hash`,
// `contract_address`, `min_phase`, and the assumption that
// `bytecode_hash`/`contract_address` are TOML STRINGS in `0x...` hex form
// rather than raw byte arrays) is inferred from doc-comment prose in
// src/main.rs (search that file for `StrategyDeployment` and
// `strategy_entries_from_manifest`), not from the real struct
// definitions or their `#[serde(...)]` attributes. This tool deliberately
// does NOT import `omega_security::DeploymentManifest` and construct it
// directly — it builds and serializes an independent LOCAL mirror struct
// (`ManifestFile` below) instead, specifically so a wrong guess here is a
// bad TOML file you can inspect and fix, not a compile error inside a
// crate whose internals this tool can't see. BEFORE trusting output from
// this tool:
//   1. Open `crates/omega-security/src/integrity.rs` and confirm
//      `DeploymentManifest`/`StrategyDeployment`'s real field names,
//      types, and any `#[serde(rename = "...")]` attributes.
//   2. Confirm the exact string `StrategyId::to_string()` produces for
//      each strategy (SA/MSA/LA/MEV) — `resolve_strategy_bytecode_hash`
//      in src/main.rs compares `e.strategy_id == id_str` where `id_str =
//      strategy_id.to_string()`, so this tool's `strategy_id` strings
//      must match that EXACTLY (case included) or every lookup silently
//      misses and falls back to the `[0u8; 32]` placeholder — the same
//      failure mode as having no manifest at all, just harder to notice.
//   3. Run `main.rs`'s own `load_deployment_manifest` +
//      `strategy_entries_from_manifest` against a real generated file
//      (e.g. a small integration test, or just running the engine
//      pointed at it in a non-production environment) before relying on
//      this in production.

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
    /// of {strategy_id, contract_address, min_phase}. Mutually exclusive
    /// with --forge-broadcast/--strategy-map.
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
    /// — case-sensitive, see the module-level "UNVERIFIED ASSUMPTION"
    /// note on why case matters). Without this flag, an unrecognized
    /// strategy_id aborts the run rather than silently writing a
    /// manifest entry that will never match anything
    /// `resolve_strategy_bytecode_hash` looks up.
    #[arg(long, default_value_t = false)]
    allow_unknown_strategy_id: bool,
}

/// Strategy IDs this tool recognizes without --allow-unknown-strategy-id.
/// CNRY is deliberately excluded — cnry.rs's own module doc comment
/// states CNRY's `build_blueprint` always returns `Err` before
/// constructing a real blueprint and uses a fixed `B256::ZERO` bytecode
/// hash sentinel; it is not part of the IntegrityRegistry-checked
/// strategy set this manifest exists to authorize.
///
/// CASE: written exactly as guessed from src/main.rs's own
/// `StrategyId::Sa | ::Msa | ::La | ::Mev` enum-variant spelling. This is
/// almost certainly NOT the same as whatever `Display`/`to_string()`
/// actually produces for that enum (Rust enum variants are typically
/// `PascalCase` in source but a custom `Display` impl could render
/// anything) — see this file's module-level "UNVERIFIED ASSUMPTION"
/// item 2. Treat this list as a starting point to edit, not a verified
/// answer.
const KNOWN_STRATEGY_IDS: [&str; 4] = ["Sa", "Msa", "La", "Mev"];

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
// Output types (local mirror — see module-level "UNVERIFIED ASSUMPTION")
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
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    match path.extension().and_then(|e| e.to_str()) {
        Some("json") => {
            serde_json::from_str(&raw).with_context(|| format!("parsing {} as JSON", path.display()))
        }
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
    let entries: Vec<ManualEntry> = load_structured(path)?;
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

fn load_forge_candidates(
    broadcast_path: &PathBuf,
    map_path: &PathBuf,
) -> Result<Vec<Candidate>> {
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
            bail!("empty strategy_id for contract_address {}", c.contract_address);
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
            bail!("strategy_id {:?}: contract_address is the zero address", c.strategy_id);
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

    // NOT VERIFIED against a real compile: `Provider::get_code_at`'s
    // exact signature (does it take a block tag argument? is it
    // `get_code_at(address)` or `get_code_at(address, BlockId::latest())`
    // in this workspace's pinned alloy version?) was not confirmed
    // against real `cargo check` output the way every alloy-facing call
    // in this codebase's other files explicitly was (see e.g.
    // chainlink_agg.rs's and arb_gas_info.rs's own module comments,
    // which document real compiler-confirmed corrections). If this line
    // fails to compile, check `cargo doc -p alloy --open` for the real
    // `Provider::get_code_at` signature in the pinned version rather
    // than guessing again.
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

    // ── Load candidates from whichever input mode was given ──────────────────
    let candidates = match (&cli.manual, &cli.forge_broadcast, &cli.strategy_map) {
        (Some(manual), None, None) => load_manual_candidates(manual)?,
        (None, Some(broadcast), Some(map)) => load_forge_candidates(broadcast, map)?,
        (None, Some(_), None) => bail!("--forge-broadcast requires --strategy-map"),
        (None, None, Some(_)) => bail!("--strategy-map requires --forge-broadcast"),
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            bail!("--manual is mutually exclusive with --forge-broadcast/--strategy-map")
        }
        (None, None, None) => {
            bail!("provide either --manual <path> or --forge-broadcast <path> --strategy-map <path>")
        }
    };

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
    // SAME local mirror struct. This does NOT validate against the real
    // omega_security::DeploymentManifest shape (see module-level
    // "UNVERIFIED ASSUMPTION" note) — it only catches this tool
    // producing syntactically-broken TOML, which would otherwise fail
    // silently until someone tried to load it with the real engine.
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
        "Before trusting this file in production: verify it against the REAL \
         omega_security::DeploymentManifest/StrategyDeployment field names AND \
         confirm each strategy_id string matches StrategyId::to_string() exactly \
         — see this tool's own module-level 'UNVERIFIED ASSUMPTION' doc comment."
    );

    Ok(())
}