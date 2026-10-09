# mock-relay.ps1 — LOCAL FORK TESTING ONLY.
#
# A stand-in for a Flashbots-style relay. Accepts JSON-RPC POSTs; for eth_sendBundle it
# forwards each signed raw transaction to Anvil via eth_sendRawTransaction (Anvil automines,
# so they land immediately — target block / bundle atomicity are ignored).
#
# Usage:   .\mock-relay.ps1                      (listens on http://127.0.0.1:8600/)
#          .\mock-relay.ps1 -Port 8600 -Anvil http://127.0.0.1:8545
#
# Never point this at a real chain: it has no auth, no simulation, and no atomicity.
param(
    [int]$Port = 8600,
    [string]$Anvil = "http://127.0.0.1:8545"
)

$ErrorActionPreference = "Stop"

function Send-Anvil([string]$method, $params) {
    $body = @{ jsonrpc = "2.0"; id = 1; method = $method; params = $params } | ConvertTo-Json -Depth 10 -Compress
    Invoke-RestMethod -Uri $Anvil -Method Post -ContentType "application/json" -Body $body
}

$listener = [System.Net.HttpListener]::new()
$listener.Prefixes.Add("http://127.0.0.1:$Port/")
$listener.Start()
Write-Host "mock relay listening on http://127.0.0.1:$Port/  ->  forwarding to $Anvil"

try {
    while ($listener.IsListening) {
        $ctx = $listener.GetContext()
        $reader = [System.IO.StreamReader]::new($ctx.Request.InputStream)
        $raw = $reader.ReadToEnd()
        $reader.Close()

        $stamp = (Get-Date).ToString("HH:mm:ss.fff")
        $preview = if ($raw.Length -gt 300) { $raw.Substring(0, 300) + "..." } else { $raw }
        Write-Host "[$stamp] $($ctx.Request.HttpMethod) $($ctx.Request.Url.AbsolutePath)  $preview"

        $resp = $null
        try {
            $req = $raw | ConvertFrom-Json
            $id = if ($null -ne $req.id) { $req.id } else { 1 }

            switch ($req.method) {
                "eth_sendBundle" {
                    $txs = @($req.params[0].txs)
                    $hashes = @()
                    foreach ($tx in $txs) {
                        $r = Send-Anvil "eth_sendRawTransaction" @($tx)
                        if ($r.error) { throw "anvil rejected tx: $($r.error.message)" }
                        $hashes += $r.result
                        Write-Host "    -> anvil accepted tx $($r.result)"
                    }
                    $bundleHash = if ($hashes.Count -gt 0) { $hashes[0] } else { "0x" + ("00" * 32) }
                    $resp = @{ jsonrpc = "2.0"; id = $id; result = @{ bundleHash = $bundleHash } }
                }
                "eth_sendPrivateTransaction" {
                    $r = Send-Anvil "eth_sendRawTransaction" @($req.params[0].tx)
                    if ($r.error) { throw "anvil rejected tx: $($r.error.message)" }
                    $resp = @{ jsonrpc = "2.0"; id = $id; result = $r.result }
                }
                default {
                    # eth_callBundle, eth_cancelBundle, flashbots_* etc: acknowledge, do nothing.
                    Write-Host "    (unhandled method '$($req.method)' acknowledged with empty result)"
                    $resp = @{ jsonrpc = "2.0"; id = $id; result = @{} }
                }
            }
        }
        catch {
            Write-Host "    ERROR: $($_.Exception.Message)"
            $resp = @{ jsonrpc = "2.0"; id = 1; error = @{ code = -32000; message = "$($_.Exception.Message)" } }
        }

        $out = [System.Text.Encoding]::UTF8.GetBytes(($resp | ConvertTo-Json -Depth 10 -Compress))
        $ctx.Response.ContentType = "application/json"
        $ctx.Response.ContentLength64 = $out.Length
        $ctx.Response.OutputStream.Write($out, 0, $out.Length)
        $ctx.Response.OutputStream.Close()
    }
}
finally {
    $listener.Stop()
}