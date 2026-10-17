param(
    [Parameter(Mandatory)][string]$Path,
    [int]$Lines = 1200,
    [int]$Width = 12
)
$ErrorActionPreference = 'Stop'
if (Test-Path -LiteralPath $Path) { throw 'Choose a new fixture directory' }
New-Item -ItemType Directory -Path $Path | Out-Null
$Path = (Resolve-Path -LiteralPath $Path).Path
git -C $Path init -q
git -C $Path config user.name Benchmark
git -C $Path config user.email benchmark@example.invalid
git -C $Path config core.autocrlf false
foreach ($version in 1,2) {
    $source = [Text.StringBuilder]::new()
    [void]$source.Append("/* Complete-source multiline comment`n   Unicode and tabs are intentional: café 世界. */`n")
    foreach ($i in 1..$Lines) {
        [void]$source.Append(('pub fn item_{0}(value: usize) -> usize {{ let label = "{1}"; value + {2} }}' -f $i, ('wide source text ' * $Width), $version) + "`n")
    }
    [IO.File]::WriteAllText((Join-Path $Path '000-wide.rs'), $source.ToString(), [Text.UTF8Encoding]::new($false))
    foreach ($i in 1..300) {
        [IO.File]::WriteAllText((Join-Path $Path ('file-{0:D4}.txt' -f $i)), "version $version`n")
    }
    git -C $Path add .
    git -C $Path commit -qm "source version $version"
    if ($LASTEXITCODE -ne 0) { throw 'Fixture commit failed' }
}
Write-Output $Path
