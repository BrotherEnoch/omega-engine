# start-engine.ps1 - LOCAL ANVIL FORK ONLY. (ASCII-only on purpose: Windows PowerShell 5.1 mangles UTF-8.)
#
# Sets every environment variable the engine needs, then launches it. Refuses to run unless the
# RPC endpoints are on 127.0.0.1, so it can never be pointed at a real chain by accident.
# Keys below are Anvil's PUBLISHED test keys (accounts #0 and #1) - worthless on any real chain.
#
# Prerequisites (separate windows, already running):
#   1. anvil (forked, chain id 42161) on 127.0.0.1:8545
#   2. .\mock-relay.ps1
#   3. Contracts deployed on THIS anvil session (.\setup-local.ps1). If you restarted anvil,
#      re-run setup-local.ps1 and update the three addresses below.
#
# Usage:  cd C:\Users\silve\Documents\omega-engine ; .\start-engine.ps1
# Do NOT run omega-engine.exe directly - it needs the variables set here.

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

# Make piped engine output decode as UTF-8 (fixes mangled characters in the log)
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8

# --- Local-only guard ---
$ws   = "ws://127.0.0.1:8545"
$http = "http://127.0.0.1:8545"
foreach ($u in @($ws, $http)) {
    if ($u -notmatch '^(ws|http)://127\.0\.0\.1[:/]') { throw "refusing to run: non-local RPC $u" }
}

# --- Binary must exist ---
if (-not (Test-Path .\target\release\omega-engine.exe)) {
    throw "target\release\omega-engine.exe not found. Run: cargo build --release"
}

# --- Anvil must be up, on the expected chain ---
$cid = "$(cast chain-id --rpc-url $http 2>$null)".Trim()
if ($cid -ne "42161") { throw "Anvil not reachable on $http or chain id is '$cid' (expected 42161)" }

# --- Mock relay must be up ---
try {
    $null = Invoke-WebRequest -Uri "http://127.0.0.1:8600" -Method Get -UseBasicParsing -TimeoutSec 3
} catch {
    # Any HTTP response (even 404/405) means something is listening; only a connection failure is fatal.
    if (-not $_.Exception.Response) { throw "mock relay not reachable on http://127.0.0.1:8600 - start .\mock-relay.ps1 first" }
}

# --- No other engine instance may be running (it would hold the log and the nonce) ---
if (Get-Process omega-engine -ErrorAction SilentlyContinue) {
    throw "omega-engine is already running. Stop it first: Stop-Process -Name omega-engine -Force"
}

# --- Endpoints / chain ---
$env:ARBITRUM_RPC_URL      = $ws
$env:ARBITRUM_HTTP_RPC_URL = $http
$env:OMEGA_CHAIN_ID        = "42161"

# --- Deployed contracts (from setup-local.ps1) ---
$env:VAULT_ADDRESS         = "0x22bF1A63C12E2d238DAccAaB146652DbB2265009"
$env:PROFIT_TOKEN          = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"
$env:ORCHESTRATOR_ADDRESS  = "0x333b303F9D41B51aC37c05711e20f64b51C58D98"

# --- Contracts must actually exist on this anvil session ---
foreach ($pair in @(@("VAULT", $env:VAULT_ADDRESS), @("ORCHESTRATOR", $env:ORCHESTRATOR_ADDRESS))) {
    $code = "$(cast code $pair[1] --rpc-url $http)".Trim()
    if ($code -eq "0x" -or $code -eq "") {
        throw "No contract code at $($pair[0]) $($pair[1]). Anvil was restarted? Re-run .\setup-local.ps1 and update the addresses."
    }
}

# --- Phase 4 + limits ---
$env:OMEGA_ACTIVE_PHASE                  = "4"
$env:OMEGA_PRODUCTION_ACK                = "1"
$env:OMEGA_MAX_ACCOUNT_EXPOSURE_WEI      = "5000000000000000000"
$env:OMEGA_KILL_MAX_CUMULATIVE_LOSS_WEI  = "5000000000000000000"
$env:OMEGA_KILL_MAX_LOSS_PER_WINDOW_WEI  = "1000000000000000000"
$env:OMEGA_KILL_LOSS_WINDOW_SECS         = "3600"
$env:OMEGA_KILL_MAX_CONSECUTIVE_FAILURES = "5"

# Fork testing only: live mainnet MEV-Share traffic otherwise pushes the competition estimate over the cap.
$env:OMEGA_MAX_COMPETITION_PROBABILITY   = "1.0"

# --- Mock relay (mock-relay.ps1 must be running) ---
$env:OMEGA_RELAY_ENDPOINT_FLASHBOTS = "http://127.0.0.1:8600"
$env:FLASHBOTS_AUTH_KEY             = "0x" + ("11" * 32)

# --- Keys: Anvil #1 signs blueprints (== Orchestrator.execution_key); Anvil #0 pays gas ---
$env:OMEGA_TX_SIGNER             = "local"
$env:OMEGA_BLUEPRINT_SIGNING_KEY = "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d"
$env:OMEGA_TX_SIGNING_KEY        = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"

# --- Sanity checks before launch ---
$want = "$(cast call $env:ORCHESTRATOR_ADDRESS 'execution_key()(address)' --rpc-url $http)".Trim()
$have = "$(cast wallet address --private-key $env:OMEGA_BLUEPRINT_SIGNING_KEY)".Trim()
Write-Host "Orchestrator.execution_key = $want"
Write-Host "blueprint signer           = $have"
if ($want.ToLower() -ne $have.ToLower()) {
    throw "blueprint key does not match Orchestrator.execution_key; every execute() would revert InvalidSignature"
}

$enc = "$(cast abi-encode 'f(bytes32,uint64)' 0xc4bb1c851b1c74593f61f8d1f99ec07e2960d847a94d4a736e321ba387d4d2d7 42161)".Trim()
$k   = "$(cast keccak $enc)".Trim()
$n   = "$(cast call $env:ORCHESTRATOR_ADDRESS 'next_nonce(bytes32)(uint64)' $k --rpc-url $http)".Trim()
Write-Host "Orchestrator next_nonce[SA] = $n   (SA blueprints start counting at 1)"

# --- Layer-2 local ops (Anvil fork has no working ArbGasInfo precompile) ---
if (-not $env:OMEGA_SEED_L1_ON_POLL_FAIL) { $env:OMEGA_SEED_L1_ON_POLL_FAIL = "1" }
if (-not $env:OMEGA_LOCAL_L1_DATA_FEE_GWEI) { $env:OMEGA_LOCAL_L1_DATA_FEE_GWEI = "15" }
if (-not $env:OMEGA_MAX_RISK_SCORE) { $env:OMEGA_MAX_RISK_SCORE = "0.85" }
if (-not $env:OMEGA_MAX_PRICE_IMPACT_BPS) { $env:OMEGA_MAX_PRICE_IMPACT_BPS = "100" }
if (-not $env:OMEGA_ORACLE_DIVERGE_THRESHOLD) { $env:OMEGA_ORACLE_DIVERGE_THRESHOLD = "0.5" }
if (-not $env:OMEGA_PYTH_API_KEY -and -not $env:PYTH_API_KEY) { $env:OMEGA_PYTH_LOCAL_SEED = "1" }
if (-not $env:OMEGA_EXECUTION_ADDRESS) { $env:OMEGA_EXECUTION_ADDRESS = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8" }

# --- Logging (WARN+ by default to keep the log small; set OMEGA_LOG_LEVEL=info for more) ---
if (-not $env:OMEGA_LOG_LEVEL) { $env:OMEGA_LOG_LEVEL = "warn" }
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$log = Join-Path $env:USERPROFILE ("omega-engine-" + $stamp + ".log")
Write-Host "logging to $log"

# --- Launch ---
& .\target\release\omega-engine.exe 2>&1 | Tee-Object -FilePath $log