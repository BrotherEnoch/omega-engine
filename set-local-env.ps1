# set-prod-env.ps1
Set-Location C:\Users\silve\Documents\omega-engine
function Load-Env($path) {
  Get-Content $path | ForEach-Object {
    if ($_ -match '^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$' -and $_ -notmatch '^\s*#') {
      [Environment]::SetEnvironmentVariable($matches[1], $matches[2].Trim('"').Trim("'"), 'Process')
    }
  }
}
Load-Env .env.production.local
$env:BALANCER_VAULT       = "0x09635F643e140090A9A8Dcd712eD6285858ceBef"
$env:AAVE_POOL            = "0x794a61358D6843594F235190De1CcD5644eC68bB"
$env:PROFIT_TOKEN         = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"
$env:ADMIN                = "0xC3B002f428a34267bA7C6d49e9Ed5FaC7BdE0F98"
$env:VAULT_ADDRESS        = "0x8b8a1DdC0A2E7467D05f1672D867056847F3142E"
$env:ORCHESTRATOR_ADDRESS = "0x29bED5E5c073Af9D693396cC2CE7DAfb529dd521"
$env:ORCHESTRATOR         = $env:ORCHESTRATOR_ADDRESS
foreach ($n in 'FLASHBOTS','BLOXROUTE','TITAN','EDEN') {
  [Environment]::SetEnvironmentVariable("OMEGA_RELAY_ENDPOINT_$n", "http://127.0.0.1:9", 'Process')
}
$pk = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"   # Anvil account 0, local only
