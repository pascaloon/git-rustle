# All captures use the existing helper-owned private desktop, never user input.
param(
    [string]$Candidate = "$PSScriptRoot/../target/release/git-rustle.exe",
    [Parameter(Mandatory)][string]$OutputDirectory,
    [ValidateSet('1440x900','900x600')][string[]]$WindowSizes = @('1440x900','900x600'),
    [string[]]$Scenarios = @(
        'landing','graph','commit-details','commit-diff','pending','branches','settings','settings-project',
        'settings-gitlab','settings-github','diff','pending-diff','stash-panel',
        'operations','history','rebase-plan','tag-dialog','checkout-dialog','graph-menu',
        'external-tools','branches-hierarchy','cleanup-review','request-branches',
        'request-branches-menu','graph-request-compose','graph-new-branch-review',
        'graph-reset-review','graph-delete-review','graph-branch-menu',
        'graph-polish','graph-polish-detached','remote-checkout',
        'ai-settings','ai-commit','ai-summary','ai-conflict','ai-error'
    )
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Retaining existing theme validation evidence' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$results = [Collections.Generic.List[object]]::new()
foreach ($size in $WindowSizes) {
    foreach ($scenario in $Scenarios) {
        $directory = Join-Path $OutputDirectory "$size-$scenario"
        $failure = $null
        try {
            & "$PSScriptRoot/screenshot-app.ps1" -Candidate $Candidate -LightTheme -Scenarios @($scenario) -WindowSize $size -OutputDirectory $directory
            $logs = @(Get-ChildItem -LiteralPath $directory -Recurse -Filter app.log)
            if (!$logs.Count) { throw 'Missing native application log' }
            foreach ($log in $logs) {
                if ([IO.File]::ReadAllText($log.FullName) -notmatch '\bappearance light\b') { throw "Saved light preference was not loaded: $($log.FullName)" }
            }
            $dimensions = $size.Split('x')
            $captures = @(Get-ChildItem -LiteralPath $directory -Recurse -Filter screenshot.json)
            if (!$captures.Count) { throw 'Missing native capture metadata' }
            foreach ($capture in $captures) {
                $metadata = Get-Content -LiteralPath $capture.FullName -Raw | ConvertFrom-Json
                if ([Math]::Abs($metadata.width * 96 / $metadata.dpi - [int]$dimensions[0]) -gt 1 -or [Math]::Abs($metadata.height * 96 / $metadata.dpi - [int]$dimensions[1]) -gt 1) { throw "Capture did not use requested logical window size: $($capture.FullName)" }
            }
        } catch { $failure = $_.ToString(); Write-Warning "$size/$scenario`: $failure" }
        $results.Add([pscustomobject]@{size=$size;scenario=$scenario;success=($null -eq $failure);error=$failure;directory=$directory})
        $results.ToArray() | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'coverage.json')
    }
}
if (@($results | Where-Object { !$_.success }).Count) { throw 'Some theme views failed; see coverage.json and retained diagnostics' }
Write-Output 'All requested native view scenarios passed. Inspect every PNG separately; captures are excluded from timing.'
