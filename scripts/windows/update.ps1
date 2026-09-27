<#
.SYNOPSIS
    Fetch the latest Lanroam Windows dev build.

.DESCRIPTION
    Downloads the app (Lanroam.exe) and the CLI (lanroam-cli.exe) from the
    rolling `dev` pre-release (rebuilt by CI on every push) into
    %LOCALAPPDATA%\Lanroam\dev and prints the build. Running copies are
    stopped first, since Windows cannot overwrite an executable in use.

    The file keeps its name from build to build, so a caching proxy or a
    GitHub download accelerator may keep serving an old copy of the plain
    download link. The script asks the GitHub API for the current file
    instead, whose address changes with every upload, and checks that the
    installed build is the one the release announces.

    Run once with -Firewall from an elevated PowerShell to allow inbound
    traffic. The rules are bound to the program paths, so they survive
    updates and cover every port Lanroam uses (QUIC, discovery, mDNS).

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
# Executable name -> process name; the CLI reports the build it is
$Programs = [ordered]@{ 'Lanroam.exe' = 'Lanroam'; 'lanroam-cli.exe' = 'lanroam-cli' }
$App = Join-Path $Dir 'Lanroam.exe'
$Cli = Join-Path $Dir 'lanroam-cli.exe'

Write-Host 'Looking up the latest dev build'
$release = Invoke-RestMethod -Uri $Api -Headers @{ 'Cache-Control' = 'no-cache' } -UseBasicParsing
$expected = if ($release.body -match 'commit: ([0-9a-f]{7})') { $Matches[1] } else { $null }

New-Item -ItemType Directory -Force -Path $Dir | Out-Null
Get-Process -Name @($Programs.Values) -ErrorAction SilentlyContinue |
    Stop-Process -Force -PassThru |
    Wait-Process -Timeout 5 -ErrorAction SilentlyContinue

Write-Host "Downloading build $expected"
foreach ($name in $Programs.Keys) {
    $asset = $release.assets | Where-Object { $_.name -eq $name } | Select-Object -First 1
    if (-not $asset) {
        throw "The dev pre-release has no $name yet"
    }
    Invoke-WebRequest -Uri $asset.url -Headers @{ Accept = 'application/octet-stream' } `
        -OutFile (Join-Path $Dir $name) -UseBasicParsing
}
$version = & $Cli --version
Write-Host "Installed $version in $Dir"
if ($expected -and $version -notlike "*@$expected") {
    Write-Warning "Expected build ${expected}, but something between this machine and GitHub served an old copy"
}

if ($Firewall) {
    foreach ($name in $Programs.Keys) {
        $rule = "Lanroam dev ($($Programs[$name]))"
        Remove-NetFirewallRule -DisplayName $rule -ErrorAction SilentlyContinue
        New-NetFirewallRule -DisplayName $rule -Direction Inbound -Program (Join-Path $Dir $name) `
            -Action Allow -Profile Domain, Private | Out-Null
        Write-Host "Firewall rule '$rule' allows inbound traffic on Domain and Private networks"
    }
}

# The rule deliberately leaves Public networks closed; say so when that is
# what this machine is on
$public = Get-NetConnectionProfile | Where-Object NetworkCategory -eq 'Public'
foreach ($net in $public) {
    Write-Warning ("'$($net.Name)' is a Public network, so inbound traffic stays blocked. " +
        "If it is your LAN, mark it private (elevated): " +
        "Set-NetConnectionProfile -InterfaceIndex $($net.InterfaceIndex) -NetworkCategory Private")
}

Write-Host "Start the app with: & '$App'"
Write-Host "Or the CLI with:   & '$Cli' run"
