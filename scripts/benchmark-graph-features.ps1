param(
    [string]$Candidate="$PSScriptRoot/../target/release/git-rustle.exe",
    [string[]]$Scenarios=@('graph-multiselect','multi-details','pending-graph','stash-preview','stash-file','stash-pop','dense-labels','checkout-simple','diff-base','diff-base-choose','graph-commit-menu','graph-branch-menu','cherry-pick-one','cherry-pick-many','cherry-pick-review'),
    [ValidateRange(1,100)][int]$Samples=15,
    [switch]$Screenshots,
    [Parameter(Mandatory)][string]$OutputDirectory
)
$ErrorActionPreference='Stop'
. "$PSScriptRoot/benchmark-screenshot.ps1"
. "$PSScriptRoot/benchmark-processes.ps1"
$Candidate=(Resolve-Path -LiteralPath $Candidate).Path
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(Test-Path -LiteralPath $OutputDirectory){throw 'Retaining existing graph-feature run'}
New-Item -ItemType Directory -Path $OutputDirectory|Out-Null
$saved=@{};$names=@('LOCALAPPDATA','APPDATA','GIT_CONFIG_GLOBAL','GIT_CONFIG_NOSYSTEM','GIT_TERMINAL_PROMPT','GIT_AUTHOR_DATE','GIT_COMMITTER_DATE','GIT_RUSTLE_TIMING','GIT_RUSTLE_TRACE','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_DUMP','GIT_RUSTLE_BASE_CHOOSER_CAPTURE')
foreach($name in $names){$saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')}
$results=[Collections.Generic.List[object]]::new()
function Graph-Git([string]$Path,[string[]]$Arguments){$value=@(& git -C $Path @Arguments 2>&1);if($LASTEXITCODE-ne0){throw "Fixture Git failed: $($value-join' ')"};return ($value-join"`n")}
function Read-GraphLiveLog([string]$Path){
    $stream=$null;$reader=$null
    try{
        $stream=[IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
        $reader=[IO.StreamReader]::new($stream,[Text.Encoding]::UTF8,$true)
        return $reader.ReadToEnd()
    }catch [IO.IOException]{return ''}
    finally{if($null-ne$reader){$reader.Dispose()}elseif($null-ne$stream){$stream.Dispose()}}
}
function Graph-State([string]$Path){return @{head=Graph-Git $Path @('rev-parse','HEAD');branch=Graph-Git $Path @('branch','--show-current');refs=Graph-Git $Path @('for-each-ref','--format=%(objectname) %(refname)','refs/heads','refs/remotes','refs/tags');stashes=Graph-Git $Path @('stash','list','--format=%H %s');status=Graph-Git $Path @('status','--porcelain=v1');index=Graph-Git $Path @('write-tree');files=@(Get-ChildItem -LiteralPath $Path -File|ForEach-Object{@{name=$_.Name;sha=(Get-FileHash -LiteralPath $_.FullName).Hash}})}|ConvertTo-Json -Depth 5 -Compress}
try{
    $env:GIT_CONFIG_GLOBAL='NUL';$env:GIT_CONFIG_NOSYSTEM='1';$env:GIT_TERMINAL_PROMPT='0';$env:GIT_RUSTLE_TIMING='1';$env:GIT_RUSTLE_TRACE='1'
    $env:GIT_AUTHOR_DATE='2026-01-01T12:00:00+0000';$env:GIT_COMMITTER_DATE=$env:GIT_AUTHOR_DATE
    foreach($scenario in $Scenarios){
        if($scenario-notin@('graph-multiselect','multi-details','pending-graph','stash-preview','stash-file','stash-pop','dense-labels','checkout-simple','diff-base','diff-base-choose','graph-commit-menu','graph-branch-menu','cherry-pick-one','cherry-pick-many','cherry-pick-review')){throw "Unknown graph feature $scenario"}
        $samplesForScenario=if($Screenshots){@(0)}else{@(-1..($Samples-1))}
        foreach($sample in $samplesForScenario){
            Assert-BenchmarkIdle -WorkspaceRoot (Resolve-Path "$PSScriptRoot/..").Path -AuditPath (Join-Path $OutputDirectory 'background-processes.json')
            $directory=Join-Path $OutputDirectory "$scenario-$sample";New-Item -ItemType Directory -Path $directory|Out-Null
            $fixture=Join-Path $directory 'repository';New-Item -ItemType Directory -Path $fixture|Out-Null
            $env:LOCALAPPDATA=Join-Path $directory 'app-data';$env:APPDATA=$env:LOCALAPPDATA;New-Item -ItemType Directory -Path $env:LOCALAPPDATA|Out-Null
            Graph-Git $fixture @('init','-b','main')|Out-Null
            foreach($config in @(@('user.name','Graph Fixture'),@('user.email','graph@example.test'),@('commit.gpgSign','false'),@('core.hooksPath','disabled-fixture-hooks'),@('core.autocrlf','false'))){Graph-Git $fixture @('config',$config[0],$config[1])|Out-Null}
            Set-Content -LiteralPath (Join-Path $fixture '.git/git-rustle-graph-fixture') -Value 'disposable graph feature fixture'
            foreach($n in 0..3){Set-Content -LiteralPath (Join-Path $fixture 'file.txt') -Value "base $n";Graph-Git $fixture @('add','.')|Out-Null;Graph-Git $fixture @('commit','-m',"Base commit $n with a useful long subject for graph layout validation")|Out-Null}
            Graph-Git $fixture @('checkout','-b','topic')|Out-Null
            foreach($n in 0..1){Set-Content -LiteralPath (Join-Path $fixture 'file.txt') -Value "topic $n";Graph-Git $fixture @('commit','-am',"Topic commit $n")|Out-Null}
            Graph-Git $fixture @('checkout','main')|Out-Null;Set-Content -LiteralPath (Join-Path $fixture 'main.txt') -Value 'main advanced';Graph-Git $fixture @('add','.')|Out-Null;Graph-Git $fixture @('commit','-m','Main advanced after topic diverged')|Out-Null
            if($scenario-eq'pending-graph'){
                Graph-Git $fixture @('update-ref','refs/remotes/origin/main','HEAD')|Out-Null
                Graph-Git $fixture @('reset','--hard','HEAD~1')|Out-Null
                Set-Content -LiteralPath (Join-Path $fixture 'pending.txt') -Value 'Pending beside incoming remote history'
            }
            if($scenario-like'stash-*'){
                foreach($stashNumber in 1..3){
                    Set-Content -LiteralPath (Join-Path $fixture 'file.txt') -Value "saved tracked change $stashNumber"
                    Set-Content -LiteralPath (Join-Path $fixture 'saved-untracked.txt') -Value "saved untracked change $stashNumber"
                    Graph-Git $fixture @('stash','push','-u','-m',"Saved work $stashNumber")|Out-Null
                }
            }
            $before=Graph-State $fixture;$before|Set-Content -LiteralPath (Join-Path $directory 'before.json')
            $env:GIT_RUSTLE_BASE_CHOOSER_CAPTURE=if($Screenshots -and $scenario -eq 'diff-base-choose'){'1'}else{'0'}
            $env:GIT_RUSTLE_SCENARIO=$scenario;$env:GIT_RUSTLE_AUTOQUIT=if($Screenshots){'600000'}else{'100'};$env:GIT_RUSTLE_DUMP=Join-Path $directory 'state.txt'
            $log=Join-Path $directory 'app.log';$process=$null
            try{
                $process=Start-BenchmarkNativeProcess -FilePath $Candidate -ArgumentList @(('"'+$fixture+'"'),'--no-cache') -RedirectStandardError $log
                if($Screenshots){
                    $deadline=[DateTime]::UtcNow.AddSeconds(45)
                    $ready=$false
                    while([DateTime]::UtcNow-lt$deadline){
                        if($process.WaitForExit(0)){throw "Scenario exited before screenshot: $log"}
                        $observed=Read-GraphLiveLog $log
                        if($observed-match'(graph feature|graph context) verified frame'){$ready=$true;break}
                        Start-Sleep -Milliseconds 50
                    }
                    if(!$ready){throw "Graph scenario paint timeout: $log"}
                    Save-BenchmarkNativeScreenshot -Process $process -OutputPath (Join-Path $directory 'screenshot.png') -CloseAfterCapture -TimeoutSeconds 15|Out-Null
                }
                if(!$process.WaitForExit(60000)){throw "Graph scenario timeout: $log"};if($process.ExitCode-ne0){throw "Graph scenario failed: $log"}
            }finally{if($null-ne$process){$process.Dispose()}}
            $text=Read-GraphLiveLog $log
            if($text-notmatch'(graph feature|graph context) verified frame'){throw "Missing verified paint: $log"}
            $start=[regex]::Match($text,'([0-9.]+) ms\s+(?:graph feature|graph context) requested');$end=[regex]::Match($text,'([0-9.]+) ms\s+(?:graph feature|graph context) verified frame')
            $after=Graph-State $fixture;$after|Set-Content -LiteralPath (Join-Path $directory 'after.json')
            $a=$after|ConvertFrom-Json;$b=$before|ConvertFrom-Json
            foreach($key in @('refs')){if($a.$key-cne$b.$key){throw "Graph feature changed unrelated $key"}}
            if($scenario-ne'stash-pop'-and($a.status-cne$b.status-or$a.stashes-cne$b.stashes)){throw 'Graph feature changed unrelated status or stashes'}
            if($scenario-eq'checkout-simple'){
                if($a.branch-cne'topic'){throw 'Checkout failed to attach topic'}
                if($a.index-cne(Graph-Git $fixture @('rev-parse','HEAD^{tree}'))){throw 'Checkout index differs from target tree'}
            }elseif($scenario-eq'stash-pop'){
                if($a.index-cne$b.index-or$a.head-cne$b.head-or$a.branch-cne$b.branch){throw 'Stash Pop changed HEAD or index'}
                if((Graph-Git $fixture @('stash','list','--format=%s'))-match'Saved work 3'){throw 'Successful Pop did not drop selected stash'}
                if((Get-Content -LiteralPath (Join-Path $fixture 'file.txt') -Raw).Trim()-cne'saved tracked change 3'){throw 'Stash Pop tracked content differs'}
                if((Get-Content -LiteralPath (Join-Path $fixture 'saved-untracked.txt') -Raw).Trim()-cne'saved untracked change 3'){throw 'Stash Pop untracked content differs'}
                if(@(& git -C $fixture stash list).Count-ne2){throw 'Stash Pop removed an unrelated stash'}
            }else{
                if($a.index-cne$b.index-or($a.files|ConvertTo-Json -Compress)-cne($b.files|ConvertTo-Json -Compress)){throw 'Read-only graph feature changed index or worktree'}
                if($a.head-cne$b.head-or$a.branch-cne$b.branch){throw 'Read-only graph scenario changed HEAD'}
            }
            $result=[ordered]@{scenario=$scenario;sample=$sample;warmup=($sample-eq-1);ms=([double]$end.Groups[1].Value-[double]$start.Groups[1].Value);screenshot=[bool]$Screenshots}
            if($scenario -in @('cherry-pick-one','cherry-pick-many')){
                $confirmed=[regex]::Match($text,'([0-9.]+) ms\s+cherry pick confirmed')
                $applied=[regex]::Match($text,'([0-9.]+) ms\s+cherry pick applied frame')
                if(!$confirmed.Success -or !$applied.Success){throw 'Missing cherry-pick action paint interval'}
                $result.applyFrameMs=[double]$applied.Groups[1].Value-[double]$confirmed.Groups[1].Value
                $result.undoFrameMs=[double]$end.Groups[1].Value-[double]$applied.Groups[1].Value
            }
            $results.Add([pscustomobject]$result)
            $results.ToArray()|ConvertTo-Json -Depth 5|Set-Content -LiteralPath (Join-Path $OutputDirectory 'samples.json')
        }
    }
}finally{
    foreach($name in $names){[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}
    @{candidate=$Candidate;sha256=(Get-FileHash -LiteralPath $Candidate).Hash;screenshots=$Screenshots;limitations='Candidate-only real actions through verified GPUI paint on private desktops. Capture runs are separate from timing. Does not measure physical input, scanout or employer network.'}|ConvertTo-Json|Set-Content -LiteralPath (Join-Path $OutputDirectory 'provenance.json')
}
