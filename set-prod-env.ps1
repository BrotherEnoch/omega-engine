# set-prod-env.ps1
# Production Arbitrum One — run:  . .\set-prod-env.ps1

Set-Location C:\Users\silve\Documents\omega-engine

# ── Production flags ───────────────────────────────────────────────
$env:OMEGA_PRODUCTION_ACK = "1"
$env:OMEGA_ACTIVE_PHASE   = "4"
$env:OMEGA_CHAIN_ID       = "42161"

# ── Optional: load .env.production.local if present ────────────────
function Load-Env($path) {
  if (-not (Test-Path $path)) { return }
  Get-Content $path | ForEach-Object {
    if ($_ -match '^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$' -and $_ -notmatch '^\s*#') {
      [Environment]::SetEnvironmentVariable($matches[1], $matches[2].Trim('"').Trim("'"), 'Process')
    }
  }
}
Load-Env .env.production.local

# ── Required addresses (production) ────────────────────────────────
$env:VAULT_ADDRESS           = "0x8b8a1DdC0A2E7467D05f1672D867056847F3142E"
$env:ORCHESTRATOR_ADDRESS    = "0x29bED5E5c073Af9D693396cC2CE7DAfb529dd521"
$env:ORCHESTRATOR            = $env:ORCHESTRATOR_ADDRESS
$env:OMEGA_EXECUTION_ADDRESS = "0x57d421ca99bc069f471e29f5c2a8d766853b58a2"

# ── RPCs (replace YOUR_INFURA_KEY) ─────────────────────────────────
$env:ARBITRUM_RPC_URL      = "wss://arbitrum-one-rpc.publicnode.com"
$env:ARBITRUM_HTTP_RPC_URL = "https://rpc.ankr.com/arbitrum/93e7f523195586a78a3929720b841fccf6d863812e2d4c6f190c817f8f2a73a1"

# ── Signing keys (replace with real values — never commit this file) ─
$env:OMEGA_TX_SIGNING_KEY        = "49d5062238ea990c4ed43789182f48630562f3cc88aa266c1d031b37539bd91c"
$env:OMEGA_BLUEPRINT_SIGNING_KEY = "49d5062238ea990c4ed43789182f48630562f3cc88aa266c1d031b37539bd91c"

# ── Relay auth ─────────────────────────────────────────────────────
$env:TITAN_AUTH_KEY       = "0x4a1493639a63a77e13d79a932c92cc199e19446eff95fd4f6f473f320251648b"
$env:BLOXROUTE_AUTH_TOKEN = "b465daad-9937-4830-8403-313653d89f77"
$env:EDEN_AUTH_TOKEN      = "0x0faabd6e1f28d2d326e757471777fbc7fa59e1d61e994c99a0c25446fd7a0303"
$env:FLASHBOTS_AUTH_KEY   = "0x535b0f699c16e15cb61392ebc5dbb2c005090dfdd189e00db435c82863e0b57f"

# ── Real relay endpoints (NOT localhost) ───────────────────────────
$env:OMEGA_RELAY_ENDPOINT_TITAN     = "https://rpc.titanbuilder.xyz"
$env:OMEGA_RELAY_ENDPOINT_FLASHBOTS = "https://relay.flashbots.net"
$env:OMEGA_RELAY_ENDPOINT_BLOXROUTE = "https://api.blxrbdn.com"
$env:OMEGA_RELAY_ENDPOINT_EDEN      = "https://api.edennetwork.io/v1/bundle"

Write-Host "Production env loaded (Arbitrum One, phase 4)." -ForegroundColor Green