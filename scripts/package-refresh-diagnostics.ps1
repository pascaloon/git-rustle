[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Executable,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$Version = '0.1.14-refresh-diag.1'
)
$ErrorActionPreference = 'Stop'
$Executable = (Resolve-Path -LiteralPath $Executable).Path
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $OutputDirectory) { throw "Output already exists: $OutputDirectory" }
if ($Version -notmatch '^[A-Za-z0-9.-]+$') { throw 'Version contains unsafe filename characters' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$package = Join-Path $OutputDirectory "git-rustle-$Version-windows-x64"
New-Item -ItemType Directory -Path $package | Out-Null
Copy-Item -LiteralPath $Executable -Destination (Join-Path $package 'git-rustle.exe')
@'
@echo off
setlocal
if not exist "%~dp0Logs" mkdir "%~dp0Logs"
set "GIT_RUSTLE_REFRESH_LOG=%~dp0Logs\refresh-%RANDOM%-%RANDOM%-%RANDOM%.jsonl"
if "%~1"=="" (
  start "" "%~dp0git-rustle.exe"
) else (
  start "" "%~dp0git-rustle.exe" "%~1"
)
endlocal
'@ | Set-Content -LiteralPath (Join-Path $package 'Start-Diagnostics.cmd') -Encoding ascii
@"
Git Rustle $Version - portable refresh diagnostics build

Run Start-Diagnostics.cmd, optionally passing a quoted repository folder:
  Start-Diagnostics.cmd "C:\Work\My Repository"
The launcher writes a fresh JSONL session beneath the package's Logs folder.
It preserves APPDATA, LOCALAPPDATA, Git configuration and existing credentials.
The packaging script does not launch the application.

To capture the slowdown:
1. Open the work repository and wait for initial loading to finish.
2. Click the Graph's circular-arrow Refresh button two or three times,
   waiting for each refresh to finish. Use the button, not F5.
3. Open Branches once and wait for its listing to finish.
4. Close the app normally and share the new .jsonl file from Logs.
   Include which step felt slow. No repository upload is needed.

Running git-rustle.exe directly records diagnostics beneath
%LOCALAPPDATA%\git-rustle\diagnostics\refresh-<epoch>-<pid>.jsonl.
Set GIT_RUSTLE_REFRESH_LOG to an exact filename to override the location,
or to off to disable logging. Logs contain static operation/status names and
numeric measurements, not paths, URLs, branches, object IDs, file contents,
tokens or raw error strings. Review files before sharing.

The asynchronous bounded queue counts dropped events. Files stop at 64 MiB;
a truncated session cannot establish complete operation pairing. The writer
flushes every 250 ms and joins on orderly shutdown. Forced termination can
lose buffered records. Close the app before collecting Logs.
"@ | Set-Content -LiteralPath (Join-Path $package 'README.txt') -Encoding utf8
$archive = "$package.zip"
Compress-Archive -LiteralPath $package -DestinationPath $archive -CompressionLevel Optimal
$sourceHash = (Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash
$packagedHash = (Get-FileHash -LiteralPath (Join-Path $package 'git-rustle.exe') -Algorithm SHA256).Hash
if ($sourceHash -ne $packagedHash) { throw 'Packaged executable differs from supplied binary' }
[ordered]@{version=$Version;source=$Executable;sha256=$sourceHash;archive=$archive;package=$package} |
    ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory 'package-manifest.json') -Encoding utf8
Write-Output $archive
