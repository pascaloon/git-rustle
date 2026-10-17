param([Parameter(Mandatory)][string]$Path)
$ErrorActionPreference = 'Stop'
if (Test-Path -LiteralPath $Path) { throw 'Choose a new fixture directory' }
New-Item -ItemType Directory -Path $Path | Out-Null
$Path = (Resolve-Path -LiteralPath $Path).Path
git -C $Path init -q
git -C $Path config user.name Benchmark
git -C $Path config user.email benchmark@example.invalid
git -C $Path config core.autocrlf false
foreach ($version in @('original','modified')) {
    $alphabet = if ($version -eq 'original') { 'abcdefghij' } else { 'ABCDEFGHIJ' }
    $lines = 1..1200 | ForEach-Object {
        'Line {0:D4}: {1} text | {2} END {0:D4}' -f $_, $version, (("$alphabet 0123456789 ") * 12)
    }
    [IO.File]::WriteAllLines((Join-Path $Path '000-wide.txt'), $lines)
    1..300 | ForEach-Object {
        [IO.File]::WriteAllText((Join-Path $Path ('file-{0:D4}.txt' -f $_)), "$version`n")
    }
    git -C $Path add .
    git -C $Path commit -qm $version
    if ($LASTEXITCODE -ne 0) { throw 'Fixture commit failed' }
}
Write-Output $Path
