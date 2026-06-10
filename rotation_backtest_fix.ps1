# rotation_backtest_fix.ps1
# ─────────────────────────────────────────────────────────────────────────────
# Fix 1: crates\omega-address-rotation\src\rotation.rs
#
# Root cause: LaRelayMetrics::new() already returns Arc<LaRelayMetrics>.
# The tests wrap it in Arc::new() again, producing Arc<Arc<LaRelayMetrics>>.
#
# The compiler pinpointed these four lines:
#   Line 314: AddressRotationManager::new(RotationConfig::default(), metrics)
#             where metrics = Arc::new(LaRelayMetrics::new(...))
#   Line 389: mgr.execute_rotation(..., new_metrics, ...)
#             where new_metrics = Arc::new(LaRelayMetrics::new(...))
#   Line 404: mgr.execute_rotation(..., new, ...)
#             where new = Arc::new(LaRelayMetrics::new(...))
#   Line 414: mgr.execute_rotation(..., new, ...)
#             where new = Arc::new(LaRelayMetrics::new(...))
#
# Run this PowerShell from the repo root to fix all four sites at once:
# ─────────────────────────────────────────────────────────────────────────────

<#
$file = "crates\omega-address-rotation\src\rotation.rs"
$content = Get-Content $file -Raw

# Remove the outer Arc::new( wrapper and its matching closing )
# Pattern: Arc::new(LaRelayMetrics::new( ... ))
# Strategy: replace `Arc::new(LaRelayMetrics::new(` with `LaRelayMetrics::new(`
# Then remove the extra `)` that closes the Arc::new call.
#
# Step 1: replace the opening
$content = $content -replace 'Arc::new\(LaRelayMetrics::new\(', 'LaRelayMetrics::new('

# Step 2: the closing ')' of the outer Arc::new now appears as a standalone
# `)` on what was the last line of each Arc::new(...) expression.
# Since LaRelayMetrics::new() itself ends with `)`, the extra `)` will appear
# immediately after the closing `)` of LaRelayMetrics::new().
# Look for the specific patterns the compiler reported and remove the extra `)`.
#
# The safest approach: open the file in an editor after step 1 and remove
# exactly one `)` from each of the four call sites listed above.
# Each site will look like:
#   LaRelayMetrics::new(...))   ← extra ) at the end
# Change to:
#   LaRelayMetrics::new(...))   ← one ) removed

Set-Content $file $content
Write-Host "Step 1 done. Now manually remove the extra ) at lines 314, 389, 404, 414."
#>

# ─────────────────────────────────────────────────────────────────────────────
# Fix 2: ops\backtest\src\main.rs
#
# Root cause: simulate_la_opportunity() takes 7 arguments.
# Two call sites (lines ~628-636 and ~649-657) pass an 8th argument:
# chrono::Utc::now() which is unexpected.
#
# Run this PowerShell from the repo root:
# ─────────────────────────────────────────────────────────────────────────────

<#
$file = "ops\backtest\src\main.rs"

# Read all lines, filter out any line that contains only chrono::Utc::now(),
# preceded by optional whitespace (the extra argument line).
$lines = Get-Content $file
$fixed = $lines | Where-Object { $_ -notmatch '^\s*chrono::Utc::now\(\),\s*$' }
Set-Content $file $fixed

Write-Host "Removed chrono::Utc::now(), lines from backtest/main.rs"
Write-Host "Verify: cargo check -p omega-backtest"
#>

# ─────────────────────────────────────────────────────────────────────────────
# Manual alternative (if PowerShell regex is tricky):
#
# rotation.rs — open in VS Code, Ctrl+H:
#   Find:    Arc::new(LaRelayMetrics::new(
#   Replace: LaRelayMetrics::new(
#   Then remove the orphaned ) at the end of each of the 4 affected expressions.
#
# backtest/main.rs — open in VS Code, Ctrl+H:
#   Find (regex mode): ^\s*chrono::Utc::now\(\),\s*\n
#   Replace: (empty)
#   This removes the two lines.
# ─────────────────────────────────────────────────────────────────────────────