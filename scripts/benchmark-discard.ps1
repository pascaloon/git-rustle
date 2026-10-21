param(
    [string]$Candidate = "$PSScriptRoot/../target/release/git-rustle.exe",
    [ValidateRange(1,100)][int]$Samples = 15,
    [switch]$Batch,
    [switch]$Screenshots,
    [Parameter(Mandatory)][string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
$sourceRoot = (Resolve-Path -LiteralPath "$PSScriptRoot/..").Path
. (Join-Path $PSScriptRoot 'benchmark-processes.ps1')
. (Join-Path $PSScriptRoot 'benchmark-native-process.ps1')
. (Join-Path $PSScriptRoot 'benchmark-screenshot.ps1')
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$allowed = Join-Path $sourceRoot 'target/bench'
if (!$OutputDirectory.StartsWith($allowed + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Discard results must stay under workspace target/bench/<run>' }
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Choose a new output directory; existing samples are retained.' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$saved = @{}
$names = @('LOCALAPPDATA','TEMP','TMP','GIT_RUSTLE_TIMING','GIT_RUSTLE_TRACE','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_DUMP','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_FRAME_SAMPLES','GIT_DIR','GIT_WORK_TREE','GIT_INDEX_FILE','GIT_OBJECT_DIRECTORY','GIT_ALTERNATE_OBJECT_DIRECTORIES','GIT_CONFIG_COUNT','GIT_CONFIG_GLOBAL','GIT_CONFIG_SYSTEM')
foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
function Fixture-Git([string]$Path, [string[]]$Arguments) {
    $lines = @(& git -c "safe.directory=$($Path.Replace('\','/'))" -c core.hooksPath= -c core.autocrlf=false -c commit.gpgSign=false -C $Path @Arguments 2>&1)
    if ($LASTEXITCODE -ne 0) { throw "Fixture Git failed: $Arguments $($lines -join ' ')" }
    return ($lines -join "`n").TrimEnd()
}
$rows = @()
try {
    foreach ($name in @('GIT_DIR','GIT_WORK_TREE','GIT_INDEX_FILE','GIT_OBJECT_DIRECTORY','GIT_ALTERNATE_OBJECT_DIRECTORIES')) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
    $env:GIT_CONFIG_COUNT = '0'
    $env:GIT_CONFIG_GLOBAL = Join-Path $OutputDirectory 'empty-config'
    $env:GIT_CONFIG_SYSTEM = $env:GIT_CONFIG_GLOBAL
    [IO.File]::WriteAllText($env:GIT_CONFIG_GLOBAL, '')
    $null = Fixture-Git $sourceRoot @('config','--file',$env:GIT_CONFIG_GLOBAL,'--add','safe.directory',($OutputDirectory.Replace('\','/') + '/*'))
    $hashBefore = (Get-FileHash -LiteralPath $Candidate).Hash
    [pscustomobject]@{candidate=$Candidate;sha256=$hashBefore;sourceHead=(Fixture-Git $sourceRoot @('rev-parse','HEAD'));samples=$Samples;batch=[bool]$Batch;screenshots=[bool]$Screenshots} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory 'provenance.json')
    $lastSample = if($Screenshots){0}else{$Samples}
    for ($sample = 0; $sample -le $lastSample; $sample++) {
        Assert-BenchmarkIdle -WorkspaceRoot $sourceRoot -AuditPath (Join-Path $OutputDirectory 'background-processes.json')
        $launch = Join-Path $OutputDirectory "sample-$sample"
        $fixture = Join-Path $launch 'repository'
        New-Item -ItemType Directory -Path $fixture -Force | Out-Null
        $null = Fixture-Git $fixture @('init','-q','-b','main')
        foreach ($pair in @(@('user.name','Discard Benchmark'),@('user.email','discard@example.invalid'),@('commit.gpgSign','false'),@('core.autocrlf','false'),@('core.hooksPath','disabled-hooks'))) { $null = Fixture-Git $fixture @('config',$pair[0],$pair[1]) }
        [IO.File]::WriteAllText((Join-Path $fixture 'a.txt'), "base a`n")
        [IO.File]::WriteAllText((Join-Path $fixture 'b.txt'), "base b`n")
        $null = Fixture-Git $fixture @('add','.')
        $null = Fixture-Git $fixture @('commit','-qm','Initial')
        [IO.File]::WriteAllText((Join-Path $fixture '.git/git-rustle-discard-fixture'), 'disposable discard benchmark fixture')
        [IO.File]::WriteAllText((Join-Path $fixture 'a.txt'), "staged a`n")
        [IO.File]::WriteAllText((Join-Path $fixture 'b.txt'), "preserved staged b`n")
        $null = Fixture-Git $fixture @('add','.')
        [IO.File]::WriteAllText((Join-Path $fixture 'a.txt'), "unstaged a`n")
        [IO.File]::WriteAllText((Join-Path $fixture 'untracked.txt'), "preserved untracked`n")
        $head = Fixture-Git $fixture @('rev-parse','HEAD')
        $indexB = Fixture-Git $fixture @('show',':b.txt')
        $untrackedHash = (Get-FileHash -LiteralPath (Join-Path $fixture 'untracked.txt')).Hash
        $beforeStatus = Fixture-Git $fixture @('status','--porcelain=v1')
        $beforeIndex = Fixture-Git $fixture @('write-tree')
        $env:LOCALAPPDATA = Join-Path $launch 'appdata'
        $env:TEMP = Join-Path $launch 'temp'
        $env:TMP = $env:TEMP
        New-Item -ItemType Directory -Path $env:LOCALAPPDATA,$env:TEMP | Out-Null
        $env:GIT_RUSTLE_TIMING = '1'
        $env:GIT_RUSTLE_TRACE = if($Screenshots){'1'}else{'0'}
        $env:GIT_RUSTLE_AUTOQUIT = if($Screenshots){'600000'}else{'100'}
        $env:GIT_RUSTLE_SCENARIO = if($Batch){'pending-discard-selected'}else{'pending-discard'}
        $env:GIT_RUSTLE_DUMP = Join-Path $launch 'state.txt'
        $env:GIT_RUSTLE_FRAME_SAMPLES = Join-Path $launch 'frames.json'
        $log = Join-Path $launch 'timing.txt'
        # Automated actions verify GPUI paint without interactive user input.
        $process = Start-BenchmarkNativeProcess -FilePath $Candidate -ArgumentList @('"' + $fixture + '"') -RedirectStandardError $log
        try {
            if($Screenshots){
                $readyMark=if($Batch){'desktop discard undo verified frame'}else{'desktop discard verified frame'}
                $deadline=[DateTime]::UtcNow.AddSeconds(60);$ready=$false
                while([DateTime]::UtcNow-lt$deadline){
                    if($process.WaitForExit(0)){throw "Discard exited before capture: $log"}
                    if((Get-Content -LiteralPath $log -Raw)-match$readyMark){$ready=$true;break}
                    Start-Sleep -Milliseconds 50
                }
                if(!$ready){throw "Discard paint timeout: $log"}
                Save-BenchmarkNativeScreenshot -Process $process -OutputPath (Join-Path $launch 'screenshot.png') -CloseAfterCapture -TimeoutSeconds 15|Out-Null
            }
            if (!$process.WaitForExit(60000)) { $process.Kill(); throw "Timeout: $log" }
            $process.Refresh()
            if ($process.ExitCode -ne 0) { throw "Native discard failed: $log" }
        } finally { $process.Dispose() }
        $marks = @{}
        foreach ($match in [regex]::Matches((Get-Content -LiteralPath $log -Raw),'([\d.]+) ms  ([^\r\n]+)')) { if (!$marks.ContainsKey($match.Groups[2].Value)) { $marks[$match.Groups[2].Value] = [double]::Parse($match.Groups[1].Value,[cultureinfo]::InvariantCulture) } }
        foreach ($mark in @('desktop discard menu requested','desktop discard menu painted','desktop discard armed red painted','desktop discard requested','desktop discard verified frame','desktop scenario frame')) { if (!$marks.ContainsKey($mark)) { throw "Missing $mark in $log" } }
        # Startup inspection also emits operation marks; take only the pair after
        # this scenario's confirmed discard, retaining all original timing logs.
        $timingText = Get-Content -LiteralPath $log -Raw
        $discardText = $timingText.Substring($timingText.IndexOf('desktop discard requested'))
        $workerStart = [regex]::Match($discardText, '([\d.]+) ms  git operation requested')
        $workerEnd = [regex]::Match($discardText, '([\d.]+) ms  git operation done')
        if (!$workerStart.Success -or !$workerEnd.Success -or $workerEnd.Index -le $workerStart.Index) { throw "Missing confirmed-discard worker pair in $log" }
        $workerMs = [double]::Parse($workerEnd.Groups[1].Value,[cultureinfo]::InvariantCulture) - [double]::Parse($workerStart.Groups[1].Value,[cultureinfo]::InvariantCulture)
        if ($marks['desktop discard armed red painted'] -lt $marks['desktop discard menu painted']) { throw 'Confirmation did not follow initial menu paint.' }
        if ((Fixture-Git $fixture @('rev-parse','HEAD')) -ne $head -or (Fixture-Git $fixture @('show',':b.txt')) -ne $indexB -or (Get-FileHash -LiteralPath (Join-Path $fixture 'untracked.txt')).Hash -ne $untrackedHash) { throw 'Discard modified unrelated fixture state.' }
        if($Batch){
            if(!$marks.ContainsKey('desktop discard undo verified frame')){throw 'Missing batch Undo paint'}
            if([IO.File]::ReadAllText((Join-Path $fixture 'a.txt'))-cne"unstaged a`n"-or(Fixture-Git $fixture @('write-tree'))-cne$beforeIndex-or(Fixture-Git $fixture @('status','--porcelain=v1'))-cne$beforeStatus){throw 'Batch Undo did not restore exact fixture state'}
        }elseif ([IO.File]::ReadAllText((Join-Path $fixture 'a.txt')) -cne "base a`n" -or (Fixture-Git $fixture @('diff','--cached','--name-only')) -ne 'b.txt') { throw 'Discard did not restore only a.txt.' }
        $rows += [pscustomobject]@{sample=$sample;warmup=($sample -eq 0);menuMs=$marks['desktop discard menu painted']-$marks['desktop discard menu requested'];armedMs=$marks['desktop discard armed red painted']-$marks['desktop discard menu painted'];discardWorkerMs=$workerMs;discardFrameMs=$marks['desktop discard verified frame']-$marks['desktop discard requested'];log=$log}
        $rows | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'samples.json')
    }
    if ((Get-FileHash -LiteralPath $Candidate).Hash -ne $hashBefore) { throw 'Candidate executable changed during benchmark.' }
    Assert-BenchmarkIdle -WorkspaceRoot $sourceRoot -AuditPath (Join-Path $OutputDirectory 'background-processes.json')
    if($Screenshots){
        'Separate screenshot capture after verified native actions. Inspect PNG for visual review; capture intervals are not performance samples.'|Set-Content -LiteralPath (Join-Path $OutputDirectory 'report.md')
        return
    }
    $retained = @($rows | Where-Object { !$_.warmup })
    $report = @('# Pending-file discard benchmark', '', "Retained samples: $Samples (one additional warm-up).", '', '| Metric | Median ms | p95 ms |', '| --- | ---: | ---: |')
    foreach ($metric in @('menuMs','armedMs','discardWorkerMs','discardFrameMs')) {
        $values = @($retained | ForEach-Object { $_.$metric } | Sort-Object)
        $median = if ($values.Count % 2) { $values[[int][Math]::Floor($values.Count / 2)] } else { ($values[$values.Count / 2 - 1] + $values[$values.Count / 2]) / 2 }
        $report += '| {0} | {1:F3} | {2:F3} |' -f $metric,$median,$values[[int][Math]::Ceiling(.95 * $values.Count) - 1]
    }
    $report += @('', 'Candidate-only feature costs; the baseline does not implement file discard. Menu and confirmation intervals include actual paint. discardWorkerMs isolates confirmed-discard worker dispatch through result delivery, including recovery checkpoints. discardFrameMs bundles that operation, pending-status refresh, painted presentation and post-result verification reads/Git subprocesses; it is not a pure worker or paint cost. Fixture preparation and runner postcondition checks are outside measured intervals. Tiny local fixtures do not establish large-worktree latency, physical pointer input, or display scanout. Startup and shared controls require a separate baseline comparison.')
    $report | Set-Content -LiteralPath (Join-Path $OutputDirectory 'report.md')
} finally {
    foreach ($name in $names) {
        if ($null -eq $saved[$name]) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
    }
}
