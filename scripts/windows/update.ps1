<#
.SYNOPSIS
    Fetch the latest Lanroam Windows dev build.

.DESCRIPTION
    Downloads lanroam-cli.exe from the rolling `dev` pre-release (rebuilt by
    CI on every push) into %LOCALAPPDATA%\Lanroam\dev and prints its version.
    A running copy is stopped first, since Windows cannot overwrite an
    executable in use.

    Run once with -Firewall from an elevated PowerShell to allow inbound
    traffic. The rule is bound to the program path, so it survives updates
    and covers every port the CLI uses (QUIC, discovery, mDNS).

.EXAMPLE
    # From anywhere, no checkout needed:
    irm https://raw.githubusercontent.com/zlx2019/lanroam/main/scripts/windows/update.ps1 | iex

.EXAMPLE
    # First time, elevated, with the firewall rule:
    & ([scriptblock]::Create((irm https://raw.githubusercontent.com/zlx2019/lanroam/main/scripts/windows/update.ps1))) -Firewall
#>
param(
    # Also create (or refresh) the inbound firewall rule; needs elevation
    [switch]$Firewall
)

$ErrorActionPreference = 'Stop'

$Url = 'https://github.com/zlx2019/lanroam/releases/download/dev/lanroam-cli.exe'
$Dir = Join-Path $env:LOCALAPPDATA 'Lanroam\dev'
$Exe = Join-Path $Dir 'lanroam-cli.exe'
$RuleName = 'Lanroam dev (lanroam-cli)'

New-Item -ItemType Directory -Force -Path $Dir | Out-Null
Get-Process -Name 'lanroam-cli' -ErrorAction SilentlyContinue | Stop-Process -Force

Write-Host "Downloading $Url"
Invoke-WebRequest -Uri $Url -OutFile $Exe -UseBasicParsing
Write-Host "Installed $(& $Exe --version) at $Exe"

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
