param([Parameter(Mandatory)][string]$Path)
$ErrorActionPreference = 'Stop'
./scripts/new-scroll-fixture.ps1 -Path $Path | Out-Null
$Path = (Resolve-Path -LiteralPath $Path).Path
git -C $Path config commit.gpgSign false
git -C $Path config core.hooksPath disabled-benchmark-hooks
$source = Join-Path $Path 'src'
New-Item -ItemType Directory -Path $source | Out-Null
[IO.File]::WriteAllText((Join-Path $source 'a.rs'), "original a`n")
[IO.File]::WriteAllText((Join-Path $source 'b.rs'), "original b`n")
git -C $Path add src
git -C $Path commit -qm 'Add folder fixture'
if ($LASTEXITCODE -ne 0) { throw 'Fixture commit failed' }
[IO.File]::WriteAllText((Join-Path $source 'a.rs'), "pending a`n")
[IO.File]::WriteAllText((Join-Path $source 'b.rs'), "pending b`n")
1..300 | ForEach-Object {
    [IO.File]::WriteAllText((Join-Path $Path ('file-{0:D4}.txt' -f $_)), "pending`n")
}
$lines = 1..1200 | ForEach-Object {
    'Line {0:D4}: pending text | {1} END {0:D4}' -f $_, (('PENDING 0123456789 ')*12)
}
[IO.File]::WriteAllLines((Join-Path $Path '000-wide.txt'), $lines)
Write-Output $Path
