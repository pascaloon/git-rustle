param(
    [string]$Repository = (Join-Path $env:TEMP 'gr-repos/git'),
    [Parameter(Mandatory=$true)][string]$OutputDirectory
)
$ErrorActionPreference='Stop'
$Repository=(Resolve-Path -LiteralPath $Repository).Path
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$OutputDirectory=(Resolve-Path -LiteralPath $OutputDirectory).Path
$fixture=Join-Path $OutputDirectory 'repository'
if(Test-Path -LiteralPath $fixture) { throw "Fixture already exists: $fixture" }
New-Item -ItemType Directory -Path $fixture | Out-Null
git clone --mirror --shared $Repository (Join-Path $fixture '.git')
if($LASTEXITCODE -ne 0) { throw 'Shared mirror failed' }
git -C $fixture config core.bare false
git -C $fixture config core.hooksPath no-hooks
git -C $fixture config commit.gpgsign false
$sourceRefs=@(git -C $Repository for-each-ref --format='%(objectname) %(refname) %(symref)')
# Clone preserves targets but flattens non-HEAD symbolic refs. Restore them.
foreach($line in $sourceRefs) {
    $parts=$line.Split(' ',3)
    if($parts.Count -eq 3 -and $parts[2]) {
        git -C $fixture symbolic-ref $parts[1] $parts[2]
        if($LASTEXITCODE -ne 0) { throw 'Symbolic reference copy failed' }
    }
}
git -C $fixture reset --hard HEAD
if($LASTEXITCODE -ne 0) { throw 'Fixture checkout failed' }
$fixtureRefs=@(git -C $fixture for-each-ref --format='%(objectname) %(refname) %(symref)')
if(Compare-Object $sourceRefs $fixtureRefs) { throw 'Fixture references differ' }
$status=@(git -C $fixture status --porcelain)
if($status.Count -ne 0) { throw 'Fixture must be clean' }
[ordered]@{
    path=$fixture; source=$Repository
    head=(git -C $fixture rev-parse HEAD)
    commits=(git -C $fixture rev-list --all --count)
    refs=$fixtureRefs.Count; status=$status; sourceRefs=$sourceRefs; fixtureRefs=$fixtureRefs
} | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $OutputDirectory 'fixture.json')
Write-Output $fixture
