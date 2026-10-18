param(
    [Parameter(Mandatory)][string]$Path,
    [int]$Lines = 1200,
    [int]$Width = 12,
    [ValidateSet('Rust','Batch','PowerShell')][string]$Language = 'Rust'
)
$ErrorActionPreference = 'Stop'
if (Test-Path -LiteralPath $Path) { throw 'Choose a new fixture directory' }
New-Item -ItemType Directory -Path $Path | Out-Null
$Path = (Resolve-Path -LiteralPath $Path).Path
git -C $Path init -q
git -C $Path config user.name Benchmark
git -C $Path config user.email benchmark@example.invalid
git -C $Path config core.autocrlf false
$extension = switch ($Language) { 'Rust' {'rs'} 'Batch' {'bat'} 'PowerShell' {'ps1'} }
foreach ($version in 1,2) {
    $source = [Text.StringBuilder]::new()
    $header = switch ($Language) {
        'Rust' { "/* Complete-source multiline comment`n   Unicode is intentional: café 世界. */`n" }
        'Batch' { "@echo off`nREM Unicode is intentional: café 世界.`nsetlocal EnableDelayedExpansion`n" }
        'PowerShell' { "<# Complete-source multiline comment`n   Unicode is intentional: café 世界. #>`n" }
    }
    [void]$source.Append($header)
    foreach ($i in 1..$Lines) {
        $line = switch ($Language) {
            'Rust' { 'pub fn item_{0}(value: usize) -> usize {{ let label = "{1}"; value + {2} }}' -f $i, ('wide source text ' * $Width), $version }
            'Batch' { 'if %ERRORLEVEL% equ 0 echo "item_{0}: {1}" {2}' -f $i, ('wide source text ' * $Width), $version }
            'PowerShell' { 'function Get-Item{0} {{ param([string]$Name) $Count = {2}; Write-Output "{1} $Name" }}' -f $i, ('wide source text ' * $Width), $version }
        }
        [void]$source.Append($line + "`n")
    }
    # These are inert source fixtures, never executed.
    [IO.File]::WriteAllText((Join-Path $Path "000-wide.$extension"), $source.ToString(), [Text.UTF8Encoding]::new($false))
    foreach ($i in 1..300) {
        [IO.File]::WriteAllText((Join-Path $Path ('file-{0:D4}.txt' -f $i)), "version $version`n")
    }
    git -C $Path add .
    git -C $Path commit -qm "source version $version"
    if ($LASTEXITCODE -ne 0) { throw 'Fixture commit failed' }
}
Write-Output $Path
