# start-console.ps1
# Phase 4 production launcher for omega-engine control console.
# No mocks. Points the UI at the real control-plane (default :8080).
cd C:\Users\silve\Documents\omega-engine\apps\control-console

@'
# start-console.ps1
# Phase 4 production launcher for omega-engine control console.
# No mocks. Points the UI at the real control-plane (default :8080).

[CmdletBinding()]
param(
    [string]$ApiBase      = "http://127.0.0.1:8080",
    [int]$Port           = 5173,
    [string]$EnvFile     = "",
    [switch]$SkipHealthCheck,
    [switch]$OpenBrowser
)

$ErrorActionPreference = "Stop"

$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$ConsoleRoot = $ScriptDir
if (-not (Test-Path (Join-Path $ConsoleRoot "index.html"))) {
    $ConsoleRoot = Join-Path (Split-Path -Parent $ScriptDir) "apps\control-console"
}
$RepoRoot = (Resolve-Path (Join-Path $ConsoleRoot "..\..")).Path

if (-not $EnvFile) {
    $EnvFile = Join-Path $RepoRoot ".env.controlplane"
}

function Load-DotEnv {
    param([string]$Path)
    if (-not (Test-Path $Path)) {
        Write-Host "[warn] Env file not found: $Path" -ForegroundColor Yellow
        return
    }
    Get-Content $Path | ForEach-Object {
        $line = $_.Trim()
        if ($line -eq "" -or $line.StartsWith("#")) { return }
        if ($line -match '^\s*([^=]+)=(.*)$') {
            $name  = $matches[1].Trim()
            $value = $matches[2].Trim().Trim('"').Trim("'")
            [Environment]::SetEnvironmentVariable($name, $value, "Process")
            Set-Item -Path "Env:$name" -Value $value
        }
    }
    Write-Host "[ok] Loaded $Path" -ForegroundColor Green
}

function Test-ControlPlane {
    param([string]$Base)
    $healthUrl = "$Base/api/v1/health"
    try {
        $r = Invoke-WebRequest -Uri $healthUrl -Method GET -TimeoutSec 3 -UseBasicParsing
        if ($r.StatusCode -ge 200 -and $r.StatusCode -lt 300) {
            Write-Host "[ok] Control-plane healthy: $healthUrl" -ForegroundColor Green
            return $true
        }
        Write-Host "[warn] Unexpected status $($r.StatusCode) from $healthUrl" -ForegroundColor Yellow
        return $false
    } catch {
        Write-Host "[fail] Control-plane not reachable at $Base" -ForegroundColor Red
        Write-Host "       $($_.Exception.Message)" -ForegroundColor DarkRed
        return $false
    }
}

Write-Host ""
Write-Host "=== Omega Control Console (Phase 4 production) ===" -ForegroundColor Cyan
Write-Host "Console root : $ConsoleRoot"
Write-Host "API base     : $ApiBase"
Write-Host ""

Load-DotEnv -Path $EnvFile

$token = $env:OMEGA_API_TOKEN
if (-not $token) {
    Write-Host "[warn] OMEGA_API_TOKEN is not set." -ForegroundColor Yellow
    Write-Host "       Add it to .env.controlplane or set `$env:OMEGA_API_TOKEN before running." -ForegroundColor Yellow
} else {
    $preview = if ($token.Length -gt 12) {
        $token.Substring(0, 8) + "..." + $token.Substring($token.Length - 4)
    } else { "***" }
    Write-Host "[ok] OMEGA_API_TOKEN present ($preview)" -ForegroundColor Green
}

if (-not $SkipHealthCheck) {
    $ok = Test-ControlPlane -Base $ApiBase
    if (-not $ok) {
        Write-Host ""
        Write-Host "Start the control-plane first, then re-run this script." -ForegroundColor Yellow
        Write-Host "Or pass -SkipHealthCheck if you know the plane is up." -ForegroundColor Yellow
        exit 1
    }
}

Set-Location $ConsoleRoot

$url = "http://127.0.0.1:$Port"
Write-Host ""
Write-Host "In the browser console UI set:" -ForegroundColor Cyan
Write-Host "  API base : $ApiBase" -ForegroundColor White
Write-Host "  Token    : (same value as OMEGA_API_TOKEN)" -ForegroundColor White
Write-Host ""

if ($OpenBrowser) { Start-Process $url }

if (Get-Command npx -ErrorAction SilentlyContinue) {
    Write-Host "Starting Vite on $url ..." -ForegroundColor Cyan
    npx --yes vite --host 127.0.0.1 --port $Port --strictPort
} else {
    Write-Host "npx not found; using Python http.server on $url" -ForegroundColor Yellow
    python -m http.server $Port --bind 127.0.0.1
}
'@ | Set-Content -Path .\start-console.ps1 -Encoding UTF8

Write-Host "Wrote start-console.ps1"