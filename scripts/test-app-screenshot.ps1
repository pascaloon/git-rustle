# Integration checks use only helper-owned private desktops and disposable fixtures.
param(
    [string]$Candidate = (Join-Path $PSScriptRoot '../target/release/git-rustle.exe'),
    [string]$OutputDirectory = (Join-Path $PSScriptRoot ('../target/bench/screenshot-test-' + [guid]::NewGuid().ToString('N')))
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Assert-Check([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
foreach ($name in @('benchmark-native-process.ps1', 'benchmark-screenshot.ps1', 'capture-native-app-window.ps1', 'screenshot-app.ps1', 'test-app-screenshot.ps1')) {
    $parseTokens = $null
    $parseErrors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot $name), [ref]$parseTokens, [ref]$parseErrors)
    Assert-Check ($parseErrors.Count -eq 0) "PowerShell parsing failed: $name $parseErrors"
}
$runDirectory = [IO.Path]::GetFullPath($OutputDirectory)
Assert-Check (-not (Test-Path -LiteralPath $runDirectory)) "Test output must be new: $runDirectory"
& (Join-Path $PSScriptRoot 'screenshot-app.ps1') -Candidate $Candidate -Scenarios @('graph', 'settings') -OutputDirectory $runDirectory -TimeoutSeconds 30
Assert-Check (Test-Path -LiteralPath (Join-Path $runDirectory 'report.json')) 'Missing run report'
$report = Get-Content -Raw -LiteralPath (Join-Path $runDirectory 'report.json') | ConvertFrom-Json
Assert-Check ($report.success -eq $true -and $report.scenarios.Count -eq 2) 'Run report did not confirm both scenarios'
foreach ($scenario in $report.scenarios) {
    Assert-Check ($scenario.exitCode -eq 0 -and $scenario.cleanupVerified -eq $true -and $scenario.fixtureUnchanged -eq $true) "Scenario did not exit cleanly or preserve its fixture: $($scenario.scenario)"
}
$screenshots = @(Get-ChildItem -LiteralPath $runDirectory -Filter screenshot.png -Recurse)
Assert-Check ($screenshots.Count -eq 2) 'Expected exactly two scenario screenshots'
Add-Type -AssemblyName System.Drawing
$hashes = @()
foreach ($screenshot in $screenshots) {
    $bitmap = [System.Drawing.Bitmap]::new($screenshot.FullName)
    try {
        Assert-Check ($bitmap.Width -ge 640 -and $bitmap.Height -ge 480) "Screenshot dimensions too small: $($screenshot.FullName)"
    } finally { $bitmap.Dispose() }
    foreach ($artifact in @('screenshot.json', 'app.log', 'readiness.json')) {
        Assert-Check (Test-Path -LiteralPath (Join-Path $screenshot.DirectoryName $artifact)) "Missing $artifact"
    }
    $metadata = Get-Content -Raw -LiteralPath (Join-Path $screenshot.DirectoryName 'screenshot.json') | ConvertFrom-Json
    Assert-Check ($metadata.pixelValidation.passed -eq $true -and $metadata.pixelValidation.sentinelSamples -eq 0) 'Screenshot pixel validation failed'
    Assert-Check ($metadata.desktop -cmatch '^git-rustle-bench-[0-9a-f]{32}$' -and $metadata.desktop -cne $metadata.inputDesktop) 'Capture desktop was not private and non-current'
    Assert-Check ($metadata.closeRequested -eq $true) 'Capture did not request graceful application close'
    $hash = (Get-FileHash -LiteralPath $screenshot.FullName -Algorithm SHA256).Hash
    Assert-Check ($metadata.sha256 -eq $hash) 'Screenshot SHA256 does not match metadata'
    $live = Get-Process -Id $metadata.processId -ErrorAction SilentlyContinue
    Assert-Check ($null -eq $live -or $live.StartTime.ToUniversalTime().Ticks.ToString() -cne $metadata.processStartTimeUtcTicks) 'Captured application process remains alive'
    $hashes += $hash
}
Assert-Check ($hashes[0] -ne $hashes[1]) 'Distinct application scenarios produced identical PNGs'
. (Join-Path $PSScriptRoot 'benchmark-native-process.ps1')
. (Join-Path $PSScriptRoot 'benchmark-screenshot.ps1')
$stale = Start-BenchmarkNativeProcess -FilePath (Join-Path $PSHOME 'pwsh.exe') -ArgumentList @('-NoProfile', '-Command', '"Start-Sleep -Milliseconds 500"') -RedirectStandardError (Join-Path $runDirectory 'stale.log')
try {
    Assert-Check ($stale.WaitForExit(10000)) 'Stale-process fixture did not exit'
    $staleOutput = Join-Path $runDirectory 'stale.png'
    $rejected = $false
    try { Save-BenchmarkNativeScreenshot -Process $stale -OutputPath $staleOutput -TimeoutSeconds 5 | Out-Null }
    catch { $rejected = $_.Exception.Message -like '*Cannot capture an exited private-desktop process*' }
    Assert-Check $rejected 'Exited process screenshot must be rejected'
    Assert-Check (-not (Test-Path -LiteralPath $staleOutput)) 'Exited process produced a screenshot'
    $existingOutput = Join-Path $runDirectory 'existing.png'
    [IO.File]::WriteAllText($existingOutput, 'preserve screenshot sentinel')
    $rejected = $false
    try { Save-BenchmarkNativeScreenshot -Process $stale -OutputPath $existingOutput -TimeoutSeconds 5 | Out-Null }
    catch { $rejected = $_.Exception.Message -like '*Retaining existing screenshot evidence*' }
    Assert-Check $rejected 'Existing screenshot output must be rejected'
    Assert-Check ([IO.File]::ReadAllText($existingOutput) -eq 'preserve screenshot sentinel') 'Existing output was modified'
} finally { $stale.Dispose() }
$invalidRequest = Join-Path $runDirectory 'invalid-desktop-request.json'
$invalidOutput = Join-Path $runDirectory 'invalid-desktop.png'
# Rejected by name validation before desktop lookup, HWND lookup, or pixel capture.
@{ desktop = 'Default'; processId = $PID; outputPath = $invalidOutput } |
    ConvertTo-Json | Set-Content -LiteralPath $invalidRequest
$negativeLog = Join-Path $runDirectory 'invalid-desktop.log'
& (Join-Path $PSHOME 'pwsh.exe') -NoProfile -NonInteractive -File (Join-Path $PSScriptRoot 'capture-native-app-window.ps1') -RequestPath $invalidRequest 2> $negativeLog
Assert-Check ($LASTEXITCODE -ne 0 -and (Get-Content -Raw -LiteralPath $negativeLog) -like '*Not a private benchmark desktop*') 'Non-private desktop request was not rejected'
Assert-Check (-not (Test-Path -LiteralPath $invalidOutput)) 'Invalid desktop request produced screenshot output'
$identityRequest = Join-Path $runDirectory 'invalid-identity-request.json'
$identityOutput = Join-Path $runDirectory 'invalid-identity.png'
# A deliberately incorrect creation timestamp rejects this PID before any window lookup.
@{ desktop = 'git-rustle-bench-' + [guid]::NewGuid().ToString('N'); processId = $PID; processStartTimeUtcTicks = '0'; outputPath = $identityOutput } |
    ConvertTo-Json | Set-Content -LiteralPath $identityRequest
$identityLog = Join-Path $runDirectory 'invalid-identity.log'
& (Join-Path $PSHOME 'pwsh.exe') -NoProfile -NonInteractive -File (Join-Path $PSScriptRoot 'capture-native-app-window.ps1') -RequestPath $identityRequest 2> $identityLog
Assert-Check ($LASTEXITCODE -ne 0 -and (Get-Content -Raw -LiteralPath $identityLog) -like '*Capture process identity changed*') 'Stale PID identity was not rejected'
Assert-Check (-not (Test-Path -LiteralPath $identityOutput)) 'Invalid process identity produced screenshot output'
Write-Output "Screenshot integration checks passed: $runDirectory"
