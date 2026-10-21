# Opt-in screenshots of processes launched by our private-desktop helper.
# Keep capture runs separate from performance runs: PrintWindow synchronizes paint.
. "$PSScriptRoot/benchmark-native-process.ps1"

function Save-BenchmarkNativeScreenshot {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][GitRustle.Bench.NativeDesktopProcess]$Process,
        [Parameter(Mandatory)][string]$OutputPath,
        [ValidateRange(1,60)][int]$TimeoutSeconds = 15,
        [switch]$CloseAfterCapture
    )
    $ErrorActionPreference = 'Stop'
    $OutputPath = [IO.Path]::GetFullPath($OutputPath)
    if ([IO.Path]::GetExtension($OutputPath) -ine '.png') { throw 'Screenshot output must be a PNG path' }
    $metadataPath = [IO.Path]::ChangeExtension($OutputPath, '.json')
    if ((Test-Path -LiteralPath $OutputPath) -or (Test-Path -LiteralPath $metadataPath)) {
        throw "Retaining existing screenshot evidence: $OutputPath"
    }
    if ($Process.WaitForExit(0)) { throw 'Cannot capture an exited private-desktop process' }
    $live = [Diagnostics.Process]::GetProcessById($Process.Id)
    try { $startTicks = $live.StartTime.ToUniversalTime().Ticks.ToString() } finally { $live.Dispose() }
    $directory = [IO.Path]::GetDirectoryName($OutputPath)
    if (!(Test-Path -LiteralPath $directory -PathType Container)) { throw 'Screenshot output directory must exist' }
    $requestPath = Join-Path $directory ('capture-' + [Guid]::NewGuid().ToString('N') + '.json')
    @{
        desktop = $Process.DesktopName; processId = $Process.Id
        processStartTimeUtcTicks = $startTicks; outputPath = $OutputPath
        closeAfterCapture = [bool]$CloseAfterCapture
    } | ConvertTo-Json | Set-Content -LiteralPath $requestPath -Encoding utf8
    $stdout = "$requestPath.stdout.log"
    $stderr = "$requestPath.stderr.log"
    $worker = $null
    try {
        # A separate process bounds PrintWindow, which is a synchronous OS call.
        $worker = Start-Process -FilePath (Join-Path $PSHOME 'pwsh.exe') -WindowStyle Hidden `
            -ArgumentList @('-NoProfile','-NonInteractive','-File',('"' + "$PSScriptRoot/capture-native-app-window.ps1" + '"'),'-RequestPath',('"' + $requestPath + '"')) `
            -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru
        if (!$worker.WaitForExit($TimeoutSeconds * 1000)) {
            throw "Isolated screenshot worker timed out after ${TimeoutSeconds}s; diagnostics: $requestPath"
        }
        if ($worker.ExitCode -ne 0) {
            throw "Isolated screenshot failed: $(Get-Content -LiteralPath $stderr -Raw)"
        }
        if (!(Test-Path -LiteralPath $OutputPath) -or !(Test-Path -LiteralPath $metadataPath)) {
            throw 'Screenshot worker did not produce a PNG and metadata'
        }
        Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
    } finally {
        if ($null -ne $worker) {
            if (!$worker.HasExited) { $worker.Kill($true); $worker.WaitForExit() }
            $worker.Dispose()
        }
        # The caller owns the application; its surrounding finally must Dispose it.
        # Retain request and logs, including failures, beside the screenshot.
    }
}
