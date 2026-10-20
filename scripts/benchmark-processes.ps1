# Strict preflight shared by focused benchmark runners. A live `cargo run`
# waiting on this workspace's existing app is permitted; builders never are.
$script:BenchmarkIdleState = $null
function Get-BenchmarkProcessIdentity([int]$ProcessId) {
    $process = Get-Process -Id $ProcessId -ErrorAction Stop
    [pscustomobject]@{id=$process.Id;name=$process.ProcessName;startTicks=$process.StartTime.ToUniversalTime().Ticks}
}
function Resolve-BenchmarkWaitingCargo($Metadata,$CargoIdentities,$AppIdentities,[string]$WorkspaceRoot) {
    $target = [IO.Path]::GetFullPath((Join-Path $WorkspaceRoot 'target')).TrimEnd('\','/')+[IO.Path]::DirectorySeparatorChar
    $nodes = @{}
    foreach($node in @($Metadata)){$nodes[[int]$node.ProcessId]=$node}
    $cargo = @{}
    foreach($identity in @($CargoIdentities)){
        $node=$nodes[[int]$identity.id]
        if($null-eq$node-or$node.Name-ine'cargo.exe'-or![string]$node.ExecutablePath-or
           [IO.Path]::GetFileName([string]$node.ExecutablePath)-ine'cargo.exe'-or
           [string]$node.CommandLine-notmatch'^\s*(?:"[^"\r\n]*cargo\.exe"|[^\s"\r\n]*cargo(?:\.exe)?)\s+(?:\+[^\s"\r\n]+\s+)?run(?:\s|$)'){
            throw "Timing rejected: cargo PID $($identity.id) is not a verified cargo run launcher"
        }
        if($null-eq$node.CreationDate-or[Math]::Abs(([datetime]$node.CreationDate).ToUniversalTime().Ticks-[long]$identity.startTicks)-gt20000){throw 'Timing rejected: cargo identity changed during process inspection'}
        $cargo[[int]$identity.id]=[pscustomobject]@{id=[int]$identity.id;parentId=[int]$node.ParentProcessId;startTicks=[long]$identity.startTicks;executable=[string]$node.ExecutablePath;subcommand='run'}
    }
    $proofs=[Collections.Generic.List[object]]::new()
    foreach($identity in @($AppIdentities)){
        $node=$nodes[[int]$identity.id]
        if($null-eq$node-or$node.Name-ine'git-rustle.exe'-or![string]$node.ExecutablePath){continue}
        $path=[IO.Path]::GetFullPath([string]$node.ExecutablePath)
        if(!$path.StartsWith($target,[StringComparison]::OrdinalIgnoreCase)-or[IO.Path]::GetFileName($path)-ine'git-rustle.exe'){continue}
        if($null-eq$node.CreationDate-or[Math]::Abs(([datetime]$node.CreationDate).ToUniversalTime().Ticks-[long]$identity.startTicks)-gt20000){throw 'Timing rejected: application identity changed during process inspection'}
        $ancestors=[Collections.Generic.List[int]]::new();$visited=[Collections.Generic.HashSet[int]]::new()
        $parent=[int]$node.ParentProcessId
        while($cargo.ContainsKey($parent)-and$visited.Add($parent)-and$ancestors.Count-lt16){$ancestors.Add($parent);$parent=[int]$cargo[$parent].parentId}
        if($ancestors.Count){$proofs.Add([pscustomobject]@{app=[pscustomobject]@{id=[int]$identity.id;parentId=[int]$node.ParentProcessId;startTicks=[long]$identity.startTicks;executable=$path};cargoChain=@($ancestors|ForEach-Object{$cargo[$_]})})}
    }
    $proven=@($proofs|ForEach-Object{$_.cargoChain.id}|Sort-Object -Unique)
    foreach($id in $cargo.Keys){if($id-notin$proven){throw "Timing rejected: cargo PID $id has no verified live workspace app child"}}
    return ,@($proofs)
}
function Assert-BenchmarkIdle([Parameter(Mandatory)][string]$WorkspaceRoot,[string]$AuditPath) {
    $builders=@(Get-Process rustc,cl,link -ErrorAction SilentlyContinue)
    if($builders.Count){throw 'Timing rejected: compiler/linker process is running'}
    $cargoProcesses=@(Get-Process cargo -ErrorAction SilentlyContinue)
    $identities=@(foreach($process in $cargoProcesses){Get-BenchmarkProcessIdentity $process.Id})
    $key=(@($identities|Sort-Object id|ForEach-Object{"$($_.id):$($_.startTicks)"})-join',')
    $root=[IO.Path]::GetFullPath($WorkspaceRoot)
    $cached=$null-ne$script:BenchmarkIdleState-and$script:BenchmarkIdleState.key-ceq$key-and$script:BenchmarkIdleState.root-ceq$root
    if($cached){
        try{foreach($proof in $script:BenchmarkIdleState.proofs){$live=Get-BenchmarkProcessIdentity $proof.app.id;if($live.startTicks-ne$proof.app.startTicks){$cached=$false;break}}}catch{$cached=$false}
    }
    if(!$cached){
        $proofs=@()
        if($identities.Count){
            try{
                # Query only when cargo exists. Full arguments are inspected but
                # never persisted, avoiding disclosure of application arguments.
                $metadata=@(Get-CimInstance -ClassName Win32_Process -Filter "Name = 'cargo.exe' OR Name = 'git-rustle.exe'" -ErrorAction Stop)
                $appIdentities=@(foreach($node in $metadata|Where-Object Name -IEQ 'git-rustle.exe'){Get-BenchmarkProcessIdentity ([int]$node.ProcessId)})
                $proofs=Resolve-BenchmarkWaitingCargo $metadata $identities $appIdentities $root
            }catch{throw "Timing rejected: waiting cargo/app ancestry could not be verified. $($_.Exception.Message)"}
        }
        $script:BenchmarkIdleState=[pscustomobject]@{root=$root;key=$key;proofs=@($proofs);auditedPaths=[Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)}
    }
    # Confirm identities remain live after CIM inspection (including PID reuse).
    foreach($identity in $identities){if((Get-BenchmarkProcessIdentity $identity.id).startTicks-ne$identity.startTicks){throw 'Timing rejected: cargo identity changed'}}
    foreach($proof in $script:BenchmarkIdleState.proofs){if((Get-BenchmarkProcessIdentity $proof.app.id).startTicks-ne$proof.app.startTicks){throw 'Timing rejected: workspace application exited/restarted'}}
    if(@(Get-Process rustc,cl,link -ErrorAction SilentlyContinue).Count){throw 'Timing rejected: compiler/linker started during verification'}
    $finalCargo=@(Get-Process cargo -ErrorAction SilentlyContinue|Sort-Object Id|ForEach-Object{$_.Id})
    if(($finalCargo-join',')-cne(@($identities|Sort-Object id|ForEach-Object{$_.id})-join',')){throw 'Timing rejected: cargo process set changed'}
    if($AuditPath-and$script:BenchmarkIdleState.auditedPaths.Add([IO.Path]::GetFullPath($AuditPath))){
        [pscustomobject]@{observedUtc=[DateTime]::UtcNow.ToString('o');workspace=$root;permittedWaitingCargo=@($script:BenchmarkIdleState.proofs)
            policy='Only verified cargo run ancestors of a live git-rustle.exe under this workspace target are permitted. All compilers/linkers and other cargo are rejected.'
            limitation='Existing user application remains running and may consume CPU/GPU/I/O; no claim of zero background load. Process command arguments are not recorded.'}|ConvertTo-Json -Depth 8|Set-Content -LiteralPath $AuditPath
    }
}
