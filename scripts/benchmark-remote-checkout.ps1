param(
    [string]$Candidate="$PSScriptRoot/../target/release/git-rustle.exe",
    [string]$Baseline,
    [int]$Samples=15,
    [switch]$Screenshots,
    [switch]$Functional,
    [Parameter(Mandatory)][string]$OutputDirectory
)
$ErrorActionPreference='Stop'
. "$PSScriptRoot/benchmark-screenshot.ps1"
. "$PSScriptRoot/benchmark-processes.ps1"
$Candidate=(Resolve-Path $Candidate).Path
if($Baseline){$Baseline=(Resolve-Path $Baseline).Path}
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(Test-Path $OutputDirectory){throw 'Use a fresh output directory'}
New-Item -ItemType Directory $OutputDirectory|Out-Null
$names=@('LOCALAPPDATA','APPDATA','GIT_CONFIG_GLOBAL','GIT_CONFIG_NOSYSTEM','GIT_TERMINAL_PROMPT','GIT_RUSTLE_TIMING','GIT_RUSTLE_TRACE','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_DUMP','GIT_RUSTLE_GHOST_CHECKOUT_SOURCE','GIT_RUSTLE_GHOST_CHECKOUT_BRANCH','GIT_RUSTLE_GHOST_CHECKOUT_SHA')
$saved=@{};foreach($name in $names){$saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')}
$results=[Collections.Generic.List[object]]::new()
function FixtureGit([string]$Root,[string[]]$Arguments){
    $out=@(& git -C $Root @Arguments 2>&1)
    if($LASTEXITCODE-ne0){throw "Fixture Git failed: $Arguments : $out"}
    return ($out-join"`n").Trim()
}
function ReadLiveLog([string]$Path){
    $stream=[IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::ReadWrite)
    $reader=[IO.StreamReader]::new($stream)
    try{return $reader.ReadToEnd()}finally{$reader.Dispose()}
}
try{
    $env:GIT_CONFIG_GLOBAL='NUL';$env:GIT_CONFIG_NOSYSTEM='1';$env:GIT_TERMINAL_PROMPT='0'
    $env:GIT_RUSTLE_TIMING='1';$env:GIT_RUSTLE_TRACE='1'
    $cases=if($Screenshots){@('menu')}else{@('existing','new','detached')}
    foreach($case in $cases){
        $iterations=if($Screenshots){@(0)}else{@(-1..($Samples-1))}
        foreach($sample in $iterations){
            $labels=if($Baseline-and$case-ne'detached'-and!$Screenshots){if($sample%2-eq0){@('baseline','candidate')}else{@('candidate','baseline')}}else{@('candidate')}
            foreach($label in $labels){
                if(!$Screenshots -and !$Functional){Assert-BenchmarkIdle -WorkspaceRoot (Resolve-Path "$PSScriptRoot/..").Path -AuditPath (Join-Path $OutputDirectory 'processes.json')}
                $dir=Join-Path $OutputDirectory "$case-$sample-$label"
                $work=Join-Path $dir 'work';$source=Join-Path $dir 'source.git';$repo=Join-Path $dir 'client'
                New-Item -ItemType Directory $work -Force|Out-Null
                FixtureGit $work @('init','-q','-b','main')|Out-Null
                foreach($pair in @(@('user.name','Checkout Fixture'),@('user.email','fixture@example.invalid'),@('commit.gpgsign','false'),@('core.hooksPath','disabled-fixture-hooks'),@('core.autocrlf','false'))){FixtureGit $work (@('config')+$pair)|Out-Null}
                Set-Content (Join-Path $work 'file.txt') 'base'
                FixtureGit $work @('add','.')|Out-Null;FixtureGit $work @('commit','-qm','Base')|Out-Null
                $base=FixtureGit $work @('rev-parse','HEAD')
                FixtureGit $work @('branch','topic')|Out-Null
                FixtureGit $work @('clone','-q','--bare',$work,$source)|Out-Null
                FixtureGit $work @('clone','-q','--single-branch','--branch','main',$source,$repo)|Out-Null
                FixtureGit $repo @('config','core.hooksPath','disabled-fixture-hooks')|Out-Null
                FixtureGit $repo @('config','core.autocrlf','false')|Out-Null
                if($case-eq'existing'){
                    FixtureGit $repo @('branch','topic')|Out-Null
                    FixtureGit $repo @('config','--add','remote.origin.fetch','+refs/heads/topic:refs/remotes/origin/topic')|Out-Null
                    FixtureGit $repo @('update-ref','refs/remotes/origin/topic',$base)|Out-Null
                    FixtureGit $repo @('config','branch.topic.remote','origin')|Out-Null
                    FixtureGit $repo @('config','branch.topic.merge','refs/heads/topic')|Out-Null
                }
                FixtureGit $work @('switch','-q','topic')|Out-Null
                Set-Content (Join-Path $work 'file.txt') 'remote branch content'
                FixtureGit $work @('commit','-qam','Remote branch advance')|Out-Null
                FixtureGit $work @('push','-q',$source,'topic')|Out-Null
                $tip=FixtureGit $work @('rev-parse','HEAD')
                [IO.File]::WriteAllText((Join-Path $source 'git-rustle-ghost-checkout-fixture'),'disposable ghost checkout fixture')
                [IO.File]::WriteAllText((Join-Path $repo '.git/git-rustle-ghost-checkout-fixture'),'disposable ghost checkout fixture')
                $env:LOCALAPPDATA=Join-Path $dir 'appdata';$env:APPDATA=Join-Path $dir 'roaming'
                New-Item -ItemType Directory $env:LOCALAPPDATA,$env:APPDATA|Out-Null
                $env:GIT_RUSTLE_GHOST_CHECKOUT_SOURCE=$source;$env:GIT_RUSTLE_GHOST_CHECKOUT_BRANCH='topic';$env:GIT_RUSTLE_GHOST_CHECKOUT_SHA=$tip
                $env:GIT_RUSTLE_SCENARIO="ghost-checkout-$case";$env:GIT_RUSTLE_AUTOQUIT=if($Screenshots){'600000'}else{'100'}
                $env:GIT_RUSTLE_DUMP=Join-Path $dir 'state.txt'
                $exe=if($label-eq'baseline'){$Baseline}else{$Candidate};$log=Join-Path $dir 'app.log'
                $process=Start-BenchmarkNativeProcess -FilePath $exe -ArgumentList @(('"'+$repo+'"'),'--no-cache') -RedirectStandardError $log
                try{
                    if($Screenshots){
                        $deadline=[DateTime]::UtcNow.AddSeconds(45)
                        do{
                            if($process.WaitForExit(0)){throw "App exited before capture: $log"}
                            $text=ReadLiveLog $log
                            if($text-match'ghost checkout final frame'){break}
                            if([DateTime]::UtcNow-gt$deadline){throw "Paint timeout: $log"}
                            Start-Sleep -Milliseconds 50
                        }while($true)
                        Save-BenchmarkNativeScreenshot -Process $process -OutputPath (Join-Path $dir 'screenshot.png') -CloseAfterCapture|Out-Null
                    }
                    if(!$process.WaitForExit(60000)){throw "Checkout timeout: $log"}
                    if($process.ExitCode-ne0){throw "Checkout failed: $log"}
                }finally{$process.Dispose()}
                $text=[IO.File]::ReadAllText($log)
                if($text-notmatch'ghost checkout final frame'){throw "Missing verified paint: $log"}
                if((FixtureGit $repo @('rev-parse','main'))-cne$base){throw 'Unrelated main moved'}
                if($case-ne'menu'){
                    if((FixtureGit $repo @('rev-parse','HEAD'))-cne$tip){throw 'Wrong checked out commit'}
                    $branch=FixtureGit $repo @('branch','--show-current')
                    if(($case-eq'detached'-and$branch)-or($case-ne'detached'-and$branch-cne'topic')){throw 'Wrong checkout mode'}
                    if((Get-Content (Join-Path $repo 'file.txt') -Raw).Trim()-cne'remote branch content'){throw 'Wrong worktree content'}
                    if((FixtureGit $repo @('write-tree'))-cne(FixtureGit $repo @('rev-parse','HEAD^{tree}'))){throw 'Wrong index'}
                    if(FixtureGit $repo @('status','--porcelain')){throw 'Unexpected pending changes'}
                }
                $start=[regex]::Match($text,'([0-9.]+) ms\s+ghost checkout requested');$end=[regex]::Match($text,'([0-9.]+) ms\s+ghost checkout final frame')
                $results.Add([pscustomobject]@{case=$case;sample=$sample;binary=$label;ms=([double]$end.Groups[1].Value-[double]$start.Groups[1].Value);screenshot=[bool]$Screenshots;timingValid=(!$Screenshots -and !$Functional)})
                $results.ToArray()|ConvertTo-Json|Set-Content (Join-Path $OutputDirectory 'samples.json')
            }
        }
    }
}finally{
    foreach($name in $names){[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}
    @{candidate=$Candidate;baseline=$Baseline;screenshot=[bool]$Screenshots;functional=[bool]$Functional;limitations='Private desktop action/worker/paint validation. Functional and screenshot runs are excluded from performance results. No physical input, desktop composition or display scanout. Local bare remote and loopback API only.'}|ConvertTo-Json|Set-Content (Join-Path $OutputDirectory 'report.json')
}
