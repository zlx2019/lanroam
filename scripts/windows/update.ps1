<#
.SYNOPSIS
    Fetch the latest Lanroam Windows dev build.

.DESCRIPTION
    Downloads lanroam-cli.exe from the rolling `dev` pre-release (rebuilt by
    CI on every push) into %LOCALAPPDATA%\Lanroam\dev and prints its version.
    A running copy is stopped first, since Windows cannot overwrite an
    executable in use.

    The file keeps its name from build to build, so a caching proxy or a
    GitHub download accelerator may keep serving an old copy of the plain
    download link. The script asks the GitHub API for the current file
    instead, whose address changes with every upload, and checks that the
    installed build is the one the release announces.

    Run once with -Firewall from an elevated PowerShell to allow inbound
    traffic. The rule is bound to the program path, so it survives updates
    and covers every port the CLI uses (QUIC, discovery, mDNS).

.EXAMPLE
    # From anywhere, no checkout needed (the random query skips cached
    # copies of the script itself):
    irm "https://raw.githubusercontent.com/zlx2019/lanroam/main/scripts/windows/update.ps1?$(Get-Random)" | iex

.EXAMPLE
    # First time, elevated, with the firewall rule:
    & ([scriptblock]::Create((irm "https://raw.githubusercontent.com/zlx2019/lanroam/main/scripts/windows/update.ps1?$(Get-Random)"))) -Firewall
#>
param(
    # Also create (or refresh) the inbound firewall rule; needs elevation
    [switch]$Firewall
)

$ErrorActionPreference = 'Stop'

$Api = 'https://api.github.com/repos/zlx2019/lanroam/releases/tags/dev'
$Dir = Join-Path $env:LOCALAPPDATA 'Lanroam\dev'
$Exe = Join-Path $Dir 'lanroam-cli.exe'
$RuleName = 'Lanroam dev (lanroam-cli)'

Write-Host 'Looking up the latest dev build'
$release = Invoke-RestMethod -Uri $Api -Headers @{ 'Cache-Control' = 'no-cache' } -UseBasicParsing
$asset = $release.assets | Where-Object { $_.name -eq 'lanroam-cli.exe' } | Select-Object -First 1
if (-not $asset) {
    throw 'The dev pre-release has no lanroam-cli.exe yet'
}
$expected = if ($release.body -match 'commit: ([0-9a-f]{7})') { $Matches[1] } else { $null }

New-Item -ItemType Directory -Force -Path $Dir | Out-Null
Get-Process -Name 'lanroam-cli' -ErrorAction SilentlyContinue |
    Stop-Process -Force -PassThru |
    Wait-Process -Timeout 5 -ErrorAction SilentlyContinue

Write-Host "Downloading build $expected"
Invoke-WebRequest -Uri $asset.url -Headers @{ Accept = 'application/octet-stream' } -OutFile $Exe -UseBasicParsing
$version = & $Exe --version
Write-Host "Installed $version at $Exe"
if ($expected -and $version -notlike "*@$expected") {
    Write-Warning "Expected build ${expected}, but something between this machine and GitHub served an old copy"
}

if ($Firewall) {
    Remove-NetFirewallRule -DisplayName $RuleName -ErrorAction SilentlyContinue
    New-NetFirewallRule -DisplayName $RuleName -Direction Inbound -Program $Exe `
        -Action Allow -Profile Domain, Private | Out-Null
    Write-Host "Firewall rule '$RuleName' allows inbound traffic on Domain and Private networks"
}

# The rule deliberately leaves Public networks closed; say so when that is
# what this machine is on
$public = Get-NetConnectionProfile | Where-Object NetworkCategory -eq 'Public'
foreach ($net in $public) {
    Write-Warning ("'$($net.Name)' is a Public network, so inbound traffic stays blocked. " +
        "If it is your LAN, mark it private (elevated): " +
        "Set-NetConnectionProfile -InterfaceIndex $($net.InterfaceIndex) -NetworkCategory Private")
}

Write-Host "Run it with: & '$Exe' listen"
