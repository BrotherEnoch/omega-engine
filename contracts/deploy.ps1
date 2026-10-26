# contracts\deploy.ps1
# Usage: cd contracts && .\deploy.ps1

# ── Load .env ────────────────────────────────────────────────────────────────
if (-Not (Test-Path .\.env)) {
    Write-Error ".env file not found. Create contracts\.env first."
    exit 1
}
Get-Content .\.env | ForEach-Object {
    if ($_ -match '^\s*([^#][^=]+)=(.+)$') {
        [System.Environment]::SetEnvironmentVariable($matches[1].Trim(), $matches[2].Trim(), 'Process')
    }
}

# ── Menu ─────────────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "  Omega Engine — Deploy"
Write-Host "  ─────────────────────────────────────────"
Write-Host "  [1] DeployCore         (Vault, Orchestrator, Treasury, Token)"
Write-Host "  [2] DeployStrategies   (SimpleArb, CanaryArb, LiquidationArb, etc.)"
Write-Host "  [3] RegisterStrategies (reads config/deployment/arbitrum.toml)"
Write-Host ""
$choice = Read-Host "  Select"

switch ($choice) {

    # ── DeployCore ────────────────────────────────────────────────────────────
    "1" {
        $required = @(
            "PRIVATE_KEY","RPC_URL",
            "PROFIT_TOKEN","DAO_FEE_ADDRESS","ADMIN",
            "SP1_VERIFIER","PROGRAM_VKEY",
            "PER_TRANSFER_CAP","DAILY_CAP",
            "BALANCER_VAULT","AAVE_POOL","EXECUTION_KEY","DEPTH_UPDATER"
        )
        foreach ($v in $required) {
            if (-Not [System.Environment]::GetEnvironmentVariable($v, 'Process')) {
                Write-Error "Missing env var: $v"
                exit 1
            }
        }

        forge script script/DeployCore.s.sol `
            --rpc-url  $env:RPC_URL `
            --private-key $env:PRIVATE_KEY `
            --broadcast `
            --verify
    }

    # ── DeployStrategies ──────────────────────────────────────────────────────
    "2" {
        $required = @(
            "PRIVATE_KEY","RPC_URL",
            "ORCHESTRATOR",
            "AAVE_POOL","COMPOUND_COMPTROLLER","MORPHO_BLUE","ROUTER",
            "MEV_OFA_MIN_PRICE_IMPACT"
        )
        foreach ($v in $required) {
            if (-Not [System.Environment]::GetEnvironmentVariable($v, 'Process')) {
                Write-Error "Missing env var: $v"
                exit 1
            }
        }
        # EULER_V2 is optional (defaults to address(0) inside the Solidity script)
        if (-Not $env:EULER_V2) {
            $env:EULER_V2 = "0x0000000000000000000000000000000000000000"
            Write-Host "  EULER_V2 not set — using address(0) (Phase 3.0, expected)"
        }

        forge script script/DeployStrategies.s.sol `
            --rpc-url  $env:RPC_URL `
            --private-key $env:PRIVATE_KEY `
            --broadcast `
            --verify
    }

    # ── RegisterStrategies ────────────────────────────────────────────────────
    "3" {
        $required = @("PRIVATE_KEY","RPC_URL")
        foreach ($v in $required) {
            if (-Not [System.Environment]::GetEnvironmentVariable($v, 'Process')) {
                Write-Error "Missing env var: $v"
                exit 1
            }
        }
        if (-Not $env:MANIFEST_PATH) {
            $env:MANIFEST_PATH = "config/deployment/arbitrum.toml"
            Write-Host "  MANIFEST_PATH not set — defaulting to $env:MANIFEST_PATH"
        }
        if (-Not (Test-Path $env:MANIFEST_PATH)) {
            Write-Error "Manifest not found: $env:MANIFEST_PATH"
            exit 1
        }

        forge script script/RegisterStrategies.s.sol `
            --rpc-url  $env:RPC_URL `
            --private-key $env:PRIVATE_KEY `
            --broadcast
    }

    default {
        Write-Error "Invalid selection."
        exit 1
    }
}