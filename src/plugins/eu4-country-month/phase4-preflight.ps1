<#
.SYNOPSIS
Checks the staged country-month package for a later manual A/B session.

.DESCRIPTION
Builds or inspects the production plugin directory, optionally hashes an
installed eu4.exe, and prints the arm ladder. This script never starts
eu4.exe, Steam, or a scheduled task.
#>
[CmdletBinding()]
param(
	[switch]$Build,
	[string]$OutDir,
	[string]$ExePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$expectedExeHash = "c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e"
$pluginDir = $PSScriptRoot
$repoRoot = (Resolve-Path (Join-Path $pluginDir "..\..\..")).Path
if ([string]::IsNullOrWhiteSpace($OutDir)) {
	$OutDir = Join-Path $repoRoot "target\plugins\eu4-country-month"
}

if ($Build) {
	& (Join-Path $pluginDir "build.ps1") -OutDir $OutDir
	if ($LASTEXITCODE -ne 0) {
		throw "build.ps1 failed with exit code $LASTEXITCODE"
	}
}

$dll = Join-Path $OutDir "eu4_country_month.dll"
$manifest = Join-Path $OutDir "foch-plugin.toml"
foreach ($required in @($dll, $manifest)) {
	if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
		throw "Staged package is missing $required. Re-run with -Build."
	}
}

$dllItem = Get-Item -LiteralPath $dll
$manifestText = Get-Content -LiteralPath $manifest -Raw
if ($manifestText -notmatch 'id = "dev\.foch\.eu4-country-month"') {
	throw "Staged manifest is not dev.foch.eu4-country-month."
}
if ($manifestText -notmatch 'path = "eu4_country_month\.dll"') {
	throw "Staged manifest does not name eu4_country_month.dll."
}

Write-Host "Staged package:"
Write-Host "  DLL:      $dll ($($dllItem.Length) bytes)"
Write-Host "  Manifest: $manifest"

if (-not [string]::IsNullOrWhiteSpace($ExePath)) {
	if (-not (Test-Path -LiteralPath $ExePath -PathType Leaf)) {
		throw "ExePath does not name a file: $ExePath"
	}
	$hash = (Get-FileHash -LiteralPath $ExePath -Algorithm SHA256).Hash.ToLowerInvariant()
	$match = $hash -eq $expectedExeHash
	Write-Host "  EXE hash: $hash"
	Write-Host "  Pinned 1.37.5 hash matches: $match"
	if (-not $match) {
		throw "ExePath is not the pinned eu4.exe. This preflight does not start it."
	}
}

Write-Host ""
Write-Host "Manual A/B ladder (operator runs the game; this script does not):"
Write-Host "  1. foch plugin import `"$OutDir`""
Write-Host "  2. Baseline: plugin loaded, FOCH_DAILY_A unset, FOCH_DAILY_B unset."
Write-Host "  3. Treatment: FOCH_DAILY_A=1 only. Leave FOCH_DAILY_B unset."
Write-Host "     The installed 0xa89360 body stays native; the gate hooks the fixture stand-in only."
Write-Host "  4. Stop the arm if daily-a-real.txt has arena-live other than 0, or if two saves from the same start differ."
Write-Host "  5. Do not start a second eu4.exe while an arm is running."
