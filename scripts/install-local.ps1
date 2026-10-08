param([Parameter(Mandatory=$true)][string]$Binary,
      [string]$Destination = (Join-Path $env:LOCALAPPDATA 'shardrop\bin'))
$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) { throw "Executable not found: $Binary" }
New-Item -ItemType Directory -Force -Path $Destination | Out-Null
Copy-Item -LiteralPath $Binary -Destination (Join-Path $Destination 'shardrop.exe') -Force
$userPath = [string][Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $Destination) {
    [Environment]::SetEnvironmentVariable('Path', (($userPath.TrimEnd(';') + ';' + $Destination).TrimStart(';')), 'User')
}
Write-Output "Installed to $Destination. Open a new terminal to refresh PATH."
