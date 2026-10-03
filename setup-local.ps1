Set-Location C:\Users\silve\Documents\omega-engine
function Load-Env($path) {
  Get-Content $path | ForEach-Object {
    if ($_ -match '^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$' -and $_ -notmatch '^\s*#') {
      [Environment]::SetEnvironmentVariable($matches[1], $matches[2].Trim('"').Trim("'"), 'Process')
    }
  }
}
Load-Env .env.production.local
$env:BALANCER_VAULT       = "0xBA12222222228d8Ba445958a75a0704d566BF2C8"
$env:AAVE_POOL            = "0x794a61358D6845594F94dc1DB02A252b5b4814aD"
$env:PROFIT_TOKEN         = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"
$env:ADMIN                = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
$env:VAULT_ADDRESS        = "0x22bF1A63C12E2d238DAccAaB146652DbB2265009"
$env:ORCHESTRATOR_ADDRESS = "0x333b303F9D41B51aC37c05711e20f64b51C58D98"
$env:ORCHESTRATOR         = $env:ORCHESTRATOR_ADDRESS
foreach ($n in 'FLASHBOTS','BLOXROUTE','TITAN','EDEN') {
  [Environment]::SetEnvironmentVariable("OMEGA_RELAY_ENDPOINT_$n", "http://127.0.0.1:9", 'Process')
}
$pk = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"   # Anvil account 0, local only
