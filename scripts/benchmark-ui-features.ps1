param(
    [string]$Candidate = "$PSScriptRoot/../target/release/git-rustle.exe",
    [string[]]$Scenarios = @('diff-search','filter-escape','project-settings'),
    [ValidateRange(1,100)][int]$Samples = 15,
    [switch]$Screenshots,
    [Parameter(Mandatory)][string]$OutputDirectory
)
$ErrorActionPreference='Stop'
. "$PSScriptRoot/benchmark-screenshot.ps1"
. "$PSScriptRoot/benchmark-processes.ps1"
$Candidate=(Resolve-Path -LiteralPath $Candidate).Path
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(Test-Path -LiteralPath $OutputDirectory){throw 'Retaining existing UI-feature run'}
New-Item -ItemType Directory -Path $OutputDirectory|Out-Null
$saved=@{};$names=@(@(Get-ChildItem Env: | Where-Object {$_.Name -match '^(GIT_|GIT_RUSTLE_)'} | ForEach-Object Name) + @('LOCALAPPDATA','APPDATA','GIT_CONFIG_GLOBAL','GIT_CONFIG_NOSYSTEM','GIT_TERMINAL_PROMPT','GIT_AUTHOR_DATE','GIT_COMMITTER_DATE','GIT_RUSTLE_TIMING','GIT_RUSTLE_TRACE','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_DUMP') | Select-Object -Unique)
foreach($name in $names){$saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')}
$results=[Collections.Generic.List[object]]::new()
function UI-Git([string]$Path,[string[]]$Arguments){$value=@(& git -C $Path @Arguments 2>&1);if($LASTEXITCODE-ne0){throw "Fixture Git failed: $($value-join' ')"};return ($value-join"`n")}
function UI-State([string]$Path){return @{head=UI-Git $Path @('rev-parse','HEAD');refs=UI-Git $Path @('show-ref');status=UI-Git $Path @('status','--porcelain=v1');index=UI-Git $Path @('write-tree');files=@(Get-ChildItem -LiteralPath $Path -File | Sort-Object Name | ForEach-Object{@{name=$_.Name;sha=(Get-FileHash -LiteralPath $_.FullName).Hash}})}|ConvertTo-Json -Depth 5 -Compress}
function UI-Log([string]$Path){
    if(!(Test-Path -LiteralPath $Path)){return ''}
    $stream=[IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,([IO.FileShare]::ReadWrite-bor[IO.FileShare]::Delete))
    $reader=[IO.StreamReader]::new($stream);try{return $reader.ReadToEnd()}finally{$reader.Dispose()}
}
function Interval([string]$Text,[string]$First,[string]$Last){
    $a=[regex]::Match($Text,'([0-9.]+) ms\s+'+[regex]::Escape($First));$b=[regex]::Match($Text,'([0-9.]+) ms\s+'+[regex]::Escape($Last))
    if(!$a.Success -or !$b.Success){throw "Missing interval $First -> $Last"}
    return [double]$b.Groups[1].Value-[double]$a.Groups[1].Value
}
try{
    foreach($name in $names){Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue}
    $env:GIT_CONFIG_GLOBAL='NUL';$env:GIT_CONFIG_NOSYSTEM='1';$env:GIT_TERMINAL_PROMPT='0';$env:GIT_RUSTLE_TIMING='1';$env:GIT_RUSTLE_TRACE='1'
    $env:GIT_AUTHOR_DATE='2026-01-01T12:00:00+0000';$env:GIT_COMMITTER_DATE=$env:GIT_AUTHOR_DATE
    foreach($scenario in $Scenarios){
        if($scenario -notin @('diff-search','filter-escape','project-settings')){throw "Unknown UI feature $scenario"}
        $launches=if($Screenshots){@(0)}else{@(-1..($Samples-1))}
        foreach($sample in $launches){
            Assert-BenchmarkIdle -WorkspaceRoot (Resolve-Path "$PSScriptRoot/..").Path -AuditPath (Join-Path $OutputDirectory 'background-processes.json')
            $directory=Join-Path $OutputDirectory "$scenario-$sample";New-Item -ItemType Directory -Path $directory|Out-Null
            $fixture=Join-Path $directory 'UI Feature Fixture';New-Item -ItemType Directory -Path $fixture|Out-Null
            $env:LOCALAPPDATA=Join-Path $directory 'app-data';$env:APPDATA=$env:LOCALAPPDATA;New-Item -ItemType Directory -Path $env:LOCALAPPDATA|Out-Null
            UI-Git $fixture @('init','-b','main')|Out-Null
            foreach($config in @(@('user.name','UI Fixture'),@('user.email','ui@example.test'),@('commit.gpgSign','false'),@('core.hooksPath','disabled-fixture-hooks'),@('core.autocrlf','false'))){UI-Git $fixture @('config',$config[0],$config[1])|Out-Null}
            Set-Content -LiteralPath (Join-Path $fixture '.git/git-rustle-ui-fixture') -Value 'disposable UI feature fixture'
            # Sixteen changed files, ~800 KiB before+after, including unchanged context.
            foreach($revision in 1..2){
                foreach($file in 0..15){
                    $lines=[Text.StringBuilder]::new()
                    foreach($line in 1..500){$value=if($line-eq250){"revision $revision MAGIC_NEEDLE item $file"}elseif($line-eq258){"second changed hunk revision $revision file $file"}elseif($line-eq430){("long source prefix "*80)+"LONG_SEGMENT_NEEDLE "+("more long source "*80)+"LONG_SEGMENT_NEEDLE file $file"}else{"source line $line ordinary content in file $file"};[void]$lines.AppendLine($value)}
                    [IO.File]::WriteAllText((Join-Path $fixture "needle-file-$file.txt"),$lines.ToString())
                }
                UI-Git $fixture @('add','.')|Out-Null;UI-Git $fixture @('commit','-m',"needle revision $revision")|Out-Null
            }
            UI-Git $fixture @('branch','feature/search')|Out-Null
            $before=UI-State $fixture;$before|Set-Content -LiteralPath (Join-Path $directory 'before.json')
            $env:GIT_RUSTLE_SCENARIO=$scenario;$env:GIT_RUSTLE_AUTOQUIT=if($Screenshots){'600000'}else{'100'};$env:GIT_RUSTLE_DUMP=Join-Path $directory 'state.txt'
            $log=Join-Path $directory 'app.log';$process=$null
            try{
                $process=Start-BenchmarkNativeProcess -FilePath $Candidate -ArgumentList @(('"'+$fixture+'"'),'--no-cache') -RedirectStandardError $log
                if($Screenshots){
                    $deadline=[DateTime]::UtcNow.AddSeconds(45);$ready=$false
                    while([DateTime]::UtcNow-lt$deadline){if($process.WaitForExit(0)){throw "Scenario exited before screenshot: $log"};if((UI-Log $log)-match'ui feature verified frame'){$ready=$true;break};Start-Sleep -Milliseconds 50}
                    if(!$ready){throw "UI scenario paint timeout: $log"}
                    Save-BenchmarkNativeScreenshot -Process $process -OutputPath (Join-Path $directory 'screenshot.png') -CloseAfterCapture -TimeoutSeconds 15|Out-Null
                    $capture=Get-Content -LiteralPath (Join-Path $directory 'screenshot.json') -Raw|ConvertFrom-Json
                    if($capture.windowTitle -cne 'UI Feature Fixture — Git Rustle'){throw "Wrong native window title: $($capture.windowTitle)"}
                }
                if(!$process.WaitForExit(60000)){throw "UI scenario timeout: $log"};if($process.ExitCode-ne0){throw "UI scenario failed: $log"}
            }finally{if($null-ne$process){$process.Dispose()}}
            $text=Get-Content -LiteralPath $log -Raw
            $after=UI-State $fixture;$after|Set-Content -LiteralPath (Join-Path $directory 'after.json')
            $a=$after|ConvertFrom-Json;$b=$before|ConvertFrom-Json
            foreach($key in @('head','refs','status','index')){if($a.$key-cne$b.$key){throw "Read-only UI feature changed $key"}}
            if(($a.files|ConvertTo-Json -Compress)-cne($b.files|ConvertTo-Json -Compress)){throw 'Read-only UI feature changed files'}
            $result=[ordered]@{scenario=$scenario;sample=$sample;warmup=($sample-eq-1);ms=(Interval $text 'ui feature requested' 'ui feature verified frame');screenshot=[bool]$Screenshots}
            if($scenario-eq'diff-search'){$result.coldIndexMs=Interval $text 'diff filename requested' 'diff filename completed';$result.cachedQueryMs=Interval $text 'diff cached content requested' 'diff cached content completed'}
            $results.Add([pscustomobject]$result)
            $results.ToArray()|ConvertTo-Json -Depth 5|Set-Content -LiteralPath (Join-Path $OutputDirectory 'samples.json')
        }
    }
}finally{
    foreach($name in $names){[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}
    @{candidate=$Candidate;sha256=(Get-FileHash -LiteralPath $Candidate).Hash;screenshots=[bool]$Screenshots;limitations='Candidate-only real handlers and GPUI keystroke dispatch on owned private desktops, no OS input. Query timing includes 65ms debounce and frame scheduling. Screenshot runs excluded from timing. Does not establish human UX or display scanout.'}|ConvertTo-Json|Set-Content -LiteralPath (Join-Path $OutputDirectory 'provenance.json')
}
