param(
    [string]$Repository = (Join-Path $env:TEMP 'gr-repos/git'),
    [string]$Candidate = (Join-Path $PSScriptRoot '../target/release/git-rustle.exe'),
    [int]$Samples = 15,
    [Parameter(Mandatory=$true)][string]$OutputDirectory,
    [string[]]$Operations = @('author','sha','message','default','files','files-common')
)
$ErrorActionPreference = 'Stop'
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
$Repository = (Resolve-Path -LiteralPath $Repository).Path
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$saved = @{}
foreach($name in @('GIT_RUSTLE_TIMING','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_KEYS','GIT_RUSTLE_DUMP')) { $saved[$name] = [Environment]::GetEnvironmentVariable($name) }
$queries = @{
    'author' = 'a: Junio'; 'sha' = 's: 6de20f6'; 'message' = 'm: fix typo';
    'default' = 'gitk'; 'files' = 'f: .sharpmake.cs'; 'files-common' = 'f: readme'
    'words' = 'mold contracts'
}
try {
    $env:GIT_RUSTLE_TIMING='1'
    $env:GIT_RUSTLE_AUTOQUIT='1600'
    $results = @()
    foreach($operation in $Operations) {
        $query = $queries[$operation]
        if(-not $query) { throw "Unknown operation: $operation" }
        for($sample=-1; $sample -lt $Samples; $sample++) {
            $log = Join-Path $OutputDirectory "$operation-$sample.txt"
            $dump = Join-Path $OutputDirectory "$operation-$sample-screen.txt"
            $env:GIT_RUSTLE_DUMP=$dump
            $env:GIT_RUSTLE_KEYS="/$query<enter>"
            $process = Start-Process -FilePath $Candidate -ArgumentList @('"'+$Repository+'"') -WindowStyle Hidden -RedirectStandardError $log -PassThru
            if(-not $process.WaitForExit(300000)) { Stop-Process -Id $process.Id; throw "Search timed out: $log" }
            $process.Refresh()
            if($process.ExitCode -ne 0) { throw "Search failed: $log" }
            $text = Get-Content -LiteralPath $log -Raw
            $inputs = [regex]::Matches($text,'([\d.]+) ms  search input \((\d+)\)')
            if($inputs.Count -eq 0) { throw "Missing input marks: $log" }
            $last = $inputs[$inputs.Count-1]
            # Ref/preview updates can legitimately re-run the same query after
            # its final keystroke. Include that time and inspect the final frame.
            $frames = [regex]::Matches($text,'([\d.]+) ms  search frame \((\d+); (\d+) matches\)')
            if($frames.Count -eq 0) { throw "Missing final search frame: $log" }
            $frame = $frames[$frames.Count-1]
            $generation = $frame.Groups[2].Value
            $requests=[regex]::Matches($text,'([\d.]+) ms  search requested \((\d+)\)')
            if($requests.Count -eq 0 -or $generation -ne $requests[$requests.Count-1].Groups[2].Value) { throw "Final query was not drawn: $log" }
            if([double]$frame.Groups[1].Value -lt [double]$last.Groups[1].Value) { throw "Stale search frame: $log" }
            $resultsFrame = [regex]::Match($text,"([\d.]+) ms  search results frame \($generation; (\d+) matches\)")
            if(-not $resultsFrame.Success) { throw "Missing interactive results frame: $log" }
            $screen = Get-Content -LiteralPath $dump -Raw -Encoding UTF8
            if(-not $screen.Contains($query) -or $screen.Contains('Search incomplete:')) { throw "Invalid search screen: $dump" }
            $number = [cultureinfo]::InvariantCulture
            $results += [pscustomobject]@{
                operation=$operation; query=$query; sample=$sample
                inputToFrameMs=([double]::Parse($frame.Groups[1].Value,$number)-[double]::Parse($last.Groups[1].Value,$number))
                inputToResultsFrameMs=([double]::Parse($resultsFrame.Groups[1].Value,$number)-[double]::Parse($last.Groups[1].Value,$number))
                firstInputToFinalFrameMs=([double]::Parse($frame.Groups[1].Value,$number)-[double]::Parse($inputs[0].Groups[1].Value,$number))
                matches=[int]$frame.Groups[3].Value
            }
            $results | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $OutputDirectory 'samples.json')
        }
    }
    $results | Format-Table -AutoSize
} finally {
    foreach($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name]) }
}
