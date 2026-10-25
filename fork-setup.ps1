# fork-setup.ps1 - LOCAL ANVIL FORK ONLY. (ASCII-only on purpose: Windows PowerShell 5.1 mangles UTF-8.)
#
# Run from the project root AFTER anvil is up on 127.0.0.1:8545:
#   cd C:\Users\silve\Documents\omega-engine
#   .\fork-setup.ps1
#
# Does, in order:
#   1. Deploys core contracts (EXECUTION_KEY = Anvil account 1, the engine's blueprint signer)
#   2. Deploys the five strategies
#   3. Writes the new addresses/hashes into the four config/script files
#   4. Registers the strategies on the new orchestrator
#   5. Verifies the orchestrator and turns on 1s interval mining
#
# Keys below are Anvil's PUBLISHED test keys - worthless on any real chain.

$rpc  = "http://127.0.0.1:8545"
$root = $PSScriptRoot
$utf8 = New-Object System.Text.UTF8Encoding($false)

function Fail([string]$msg) {
    Write-Host ""
    Write-Host "ERROR: $msg" -ForegroundColor Red
    exit 1
}

function Grab([string]$text, [string]$pattern) {
    return [regex]::Match($text, $pattern).Groups[1].Value
}

# Replace the quoted 0x value on the Nth matching line, in order, for each value given.
function Update-Seq([string]$path, [string]$key, [string[]]$vals) {
    $lines = [IO.File]::ReadAllLines($path, $utf8)
    $i = 0
    for ($n = 0; $n -lt $lines.Count; $n++) {
        if ($lines[$n] -match ('^\s*' + $key + '\s*=\s*"0x[0-9a-fA-F]+"')) {
            if ($i -ge $vals.Count) { Fail "$path has more '$key' lines than expected" }
            $lines[$n] = $lines[$n] -replace '"0x[0-9a-fA-F]+"', ('"' + $vals[$i] + '"')
            $i++
        }
    }
    if ($i -ne $vals.Count) { Fail "$path : expected $($vals.Count) '$key' lines, found $i" }
    [IO.File]::WriteAllLines($path, $lines, $utf8)
}

# --- Preconditions ---
$cid = "$(cast chain-id --rpc-url $rpc 2>$null)".Trim()
if ($cid -ne "42161") { Fail "Anvil not reachable on $rpc or chain id is '$cid' (expected 42161). Start anvil first." }

if (Get-Process omega-engine -ErrorAction SilentlyContinue) {
    Write-Host "Stopping running omega-engine..."
    Get-Process omega-engine | Stop-Process -Force
}

$foundry = Join-Path $root "contracts\foundry.toml"
if (-not (Select-String -Path $foundry -Pattern "fs_permissions" -Quiet)) {
    Fail "contracts\foundry.toml has no fs_permissions line (needed so RegisterStrategies can read ../config)."
}

Set-Location (Join-Path $root "contracts")

# --- Keys / addresses ---
$pk    = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"   # Anvil #0 (deployer, pays gas)
$admin = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"                           # Anvil #0
$exec1 = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"                           # Anvil #1 (blueprint signer)

$env:PROFIT_TOKEN             = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"
$env:DAO_FEE_ADDRESS          = $admin
$env:ADMIN                    = $admin
$env:EXECUTION_KEY            = $exec1
$env:DEPTH_UPDATER            = $admin
$env:BALANCER_VAULT           = "0xBA12222222228d8Ba445958a75a0704d566BF2C8"
$env:AAVE_POOL                = "0x794a61358D6845594F94dc1DB02A252b5b4814aD"
$env:PER_TRANSFER_CAP         = "50000000000000000000"
$env:DAILY_CAP                = "500000000000000000000"
$env:SP1_VERIFIER             = "0x0000000000000000000000000000000000000001"
$env:PROGRAM_VKEY             = "0x0000000000000000000000000000000000000000000000000000000000000001"
$env:COMPOUND_COMPTROLLER     = "0x9c4ec768c28520B50860ea7a15bd7213a9fF58bf"
$env:MORPHO_BLUE              = "0x6c247b1F6182318877311737BaC0844bAa518F5e"
$env:ROUTER                   = "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45"
$env:MEV_OFA_MIN_PRICE_IMPACT = "50"

$A = '(0x[0-9a-fA-F]{40})'
$H = '(0x[0-9a-fA-F]{64})'

# --- 1. Core ---
Write-Host "== 1/5 DeployCore =="
$core = forge script script/DeployCore.s.sol:DeployCore --rpc-url $rpc --private-key $pk --broadcast 2>&1 | Out-String
$vault = Grab $core "OmegaVault:\s+$A"
$orch  = Grab $core "OmegaOrchestrator:\s+$A"
if (-not $vault -or -not $orch) { Write-Host $core; Fail "DeployCore failed - see output above." }
$env:ORCHESTRATOR = $orch
Write-Host "   vault        $vault"
Write-Host "   orchestrator $orch"

# --- 2. Strategies ---
Write-Host "== 2/5 DeployStrategies =="
$st = forge script script/DeployStrategies.s.sol:DeployStrategies --rpc-url $rpc --private-key $pk --broadcast 2>&1 | Out-String

$addr = [ordered]@{
    SA   = Grab $st "SimpleArb deployed:\s+$A"
    LA   = Grab $st "LiquidationArb deployed:\s+$A"
    MSA  = Grab $st "MultiStepArb deployed:\s+$A"
    MEV  = Grab $st "MevOfa deployed:\s+$A"
    CNRY = Grab $st "CanaryArb deployed:\s+$A"
}
$hash = [ordered]@{
    SA   = Grab $st "(?m)^\s*SA\s+bytecode_hash:\s+$H"
    LA   = Grab $st "(?m)^\s*LA\s+bytecode_hash:\s+$H"
    MSA  = Grab $st "(?m)^\s*MSA\s+bytecode_hash:\s+$H"
    MEV  = Grab $st "(?m)^\s*MEV\s+bytecode_hash:\s+$H"
    CNRY = Grab $st "(?m)^\s*CNRY\s+bytecode_hash:\s+$H"
}
foreach ($k in $addr.Keys) {
    if (-not $addr[$k] -or -not $hash[$k]) { Write-Host $st; Fail "DeployStrategies output missing values for $k - see output above." }
    Write-Host ("   {0,-5} {1}" -f $k, $addr[$k])
}

# --- 3. Update config/script files ---
Write-Host "== 3/5 Updating config files =="
$tomlDeploy   = Join-Path $root "config\deployment\arbitrum.toml"
$tomlManifest = Join-Path $root "config\deployment_manifest.toml"
$loadEnv      = Join-Path $root "load-engine-env.ps1"
$setupLocal   = Join-Path $root "setup-local.ps1"

# config\deployment\arbitrum.toml
Update-Seq $tomlDeploy "address"        @($orch)
Update-Seq $tomlDeploy "implementation" @($addr.SA, $addr.LA, $addr.MSA, $addr.MEV, $addr.CNRY)
Update-Seq $tomlDeploy "bytecode_hash"  @($hash.SA, $hash.LA, $hash.MSA, $hash.MEV, $hash.CNRY)

# config\deployment_manifest.toml (file order CNRY, SA, MSA, LA, MEV)
Update-Seq $tomlManifest "contract_address" @($addr.CNRY, $addr.SA, $addr.MSA, $addr.LA, $addr.MEV)
Update-Seq $tomlManifest "bytecode_hash"    @($hash.CNRY, $hash.SA, $hash.MSA, $hash.LA, $hash.MEV)

# load-engine-env.ps1 — this is what start-engine.ps1 actually sources
$loadContent = @"
# load-engine-env.ps1 - written by fork-setup.ps1. Do not edit by hand.
`$env:VAULT_ADDRESS        = '$vault'
`$env:ORCHESTRATOR_ADDRESS = '$orch'
`$env:ORCHESTRATOR         = `$env:ORCHESTRATOR_ADDRESS
"@
[IO.File]::WriteAllText($loadEnv, $loadContent, $utf8)
Write-Host "   wrote load-engine-env.ps1"

# setup-local.ps1 (legacy helper)
if (Test-Path $setupLocal) {
    $sl = [IO.File]::ReadAllText($setupLocal, $utf8)
    $sl = $sl -replace "\`$env:VAULT_ADDRESS\s*=\s*`"0x[0-9a-fA-F]+`"", "`$env:VAULT_ADDRESS        = `"$vault`""
    $sl = $sl -replace "\`$env:ORCHESTRATOR_ADDRESS\s*=\s*`"0x[0-9a-fA-F]+`"", "`$env:ORCHESTRATOR_ADDRESS = `"$orch`""
    [IO.File]::WriteAllText($setupLocal, $sl, $utf8)
}

Write-Host "   done"

# --- 4. Register ---
Write-Host "== 4/5 RegisterStrategies =="
$env:MANIFEST_PATH = "../config/deployment/arbitrum.toml"
$reg = forge script script/RegisterStrategies.s.sol:RegisterStrategies --rpc-url $rpc --private-key $pk --broadcast 2>&1 | Out-String
if ($reg -notmatch "ONCHAIN EXECUTION COMPLETE") { Write-Host $reg; Fail "RegisterStrategies failed - see output above." }
Write-Host "   registered 5 strategies"

# --- 5. Verify + mining + housekeeping ---
Write-Host "== 5/5 Verifying =="
$ek = "$(cast call $orch 'execution_key()(address)' --rpc-url $rpc 2>$null)".Trim()
Write-Host "   execution_key = $ek"
if ($ek.ToLower() -ne $exec1.ToLower()) { Fail "execution_key is not Anvil #1 ($exec1)." }

$vcode = "$(cast code $vault --rpc-url $rpc 2>$null)".Trim()
if ($vcode -eq "0x" -or $vcode -eq "") { Fail "No code at vault $vault." }

$null = cast rpc anvil_setIntervalMining 1 --rpc-url $rpc 2>$null
Write-Host "   interval mining: 1s"

# A journal left over from an earlier anvil session would make the engine chase transactions that no longer exist.
$journal = Join-Path $root "data\inflight_journal.jsonl"
if (Test-Path $journal) {
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    Move-Item $journal "$journal.$stamp.old"
    Write-Host "   moved old inflight journal aside"
}

Set-Location $root
Write-Host ""
Write-Host "Fork is ready. Make sure .\mock-relay.ps1 is running, then start the engine:  .\start-engine.ps1" -ForegroundColor Green