<#
.SYNOPSIS
Builds and packages the production eu4_country_month.dll plugin alongside foch-plugin.toml.

.DESCRIPTION
Compiles country_month.c and country_detour.asm with MSVC x64 (no fixture defines or test exports)
and stages only eu4_country_month.dll and foch-plugin.toml into OutDir, ready for
`foch plugin import <OutDir>` and `foch test --with-plugins`.
#>
[CmdletBinding()]
param(
	[string]$OutDir,
	[switch]$DebugBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$pluginDir = $PSScriptRoot
$repoRoot = (Resolve-Path (Join-Path $pluginDir "..\..\..")).Path
$sdkInclude = (Resolve-Path (Join-Path $pluginDir "..\..\packages\foch-plugin-sdk\include")).Path
$manifestSrc = Join-Path $pluginDir "foch-plugin.toml"
$cSrc = Join-Path $pluginDir "country_month.c"
$asmSrc = Join-Path $pluginDir "country_detour.asm"

foreach ($required in @($manifestSrc, $cSrc, $asmSrc, (Join-Path $sdkInclude "foch_plugin.h"))) {
	if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
		throw "Required input not found: $required"
	}
}

if ([string]::IsNullOrWhiteSpace($OutDir)) {
	$OutDir = Join-Path $repoRoot "target\plugins\eu4-country-month"
}

$null = New-Item -ItemType Directory -Path $OutDir -Force
$resolvedOutDir = (Resolve-Path -LiteralPath $OutDir).Path
$dllOut = Join-Path $resolvedOutDir "eu4_country_month.dll"
$manifestOut = Join-Path $resolvedOutDir "foch-plugin.toml"

function Find-VcVars64 {
	$programFilesX86 = ${env:ProgramFiles(x86)}
	if ($programFilesX86) {
		$vswhere = Join-Path $programFilesX86 "Microsoft Visual Studio\Installer\vswhere.exe"
		if (Test-Path -LiteralPath $vswhere -PathType Leaf) {
			$installPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
			if ($LASTEXITCODE -eq 0 -and -not [string]::IsNullOrWhiteSpace($installPath)) {
				$vcvars = Join-Path ($installPath.Trim()) "VC\Auxiliary\Build\vcvars64.bat"
				if (Test-Path -LiteralPath $vcvars -PathType Leaf) {
					return $vcvars
				}
			}
		}
	}
	$roots = @($env:ProgramFiles, ${env:ProgramFiles(x86)}) | Where-Object { $_ } | ForEach-Object {
		Join-Path $_ "Microsoft Visual Studio"
	} | Where-Object { Test-Path -LiteralPath $_ -PathType Container }
	foreach ($root in $roots) {
		$found = Get-ChildItem -LiteralPath $root -Filter "vcvars64.bat" -Recurse -ErrorAction SilentlyContinue |
			Sort-Object -Property FullName -Descending |
			Select-Object -First 1
		if ($found) {
			return $found.FullName
		}
	}
	return $null
}

$buildDir = Join-Path ([System.IO.Path]::GetTempPath()) ("foch-eu4-country-month-" + [System.Guid]::NewGuid().ToString("N"))
$null = New-Item -ItemType Directory -Path $buildDir -Force

try {
	$asmObj = Join-Path $buildDir "country_detour.obj"
	$cObj = Join-Path $buildDir "country_month.obj"
	$implib = Join-Path $buildDir "eu4_country_month.lib"
	$optFlags = if ($DebugBuild) {
		"/Od /Zi /Fd`"$buildDir\eu4_country_month.pdb`""
	} else {
		"/O2"
	}

	$hasCl = [bool](Get-Command cl.exe -ErrorAction SilentlyContinue)
	$hasMl64 = [bool](Get-Command ml64.exe -ErrorAction SilentlyContinue)

	if ($hasCl -and $hasMl64) {
		& ml64.exe /nologo /c "/Fo$asmObj" $asmSrc
		if ($LASTEXITCODE -ne 0) {
			throw "ml64.exe failed with exit code $LASTEXITCODE"
		}
		$clArgs = @("/nologo", "/LD", "/W4", "/utf-8", "/I$sdkInclude")
		if ($DebugBuild) {
			$clArgs += @("/Od", "/Zi", "/Fd$buildDir\eu4_country_month.pdb")
		} else {
			$clArgs += "/O2"
		}
		$clArgs += @(
			$cSrc,
			"/Fo$cObj",
			"/Fe$dllOut",
			$asmObj,
			"/link",
			"/IMPLIB:$implib"
		)
		& cl.exe @clArgs
		if ($LASTEXITCODE -ne 0) {
			throw "cl.exe failed with exit code $LASTEXITCODE"
		}
	} else {
		$vcvars = Find-VcVars64
		if (-not $vcvars) {
			throw "Could not find cl.exe/ml64.exe on PATH or locate Visual Studio vcvars64.bat via vswhere.exe."
		}
		$batchPath = Join-Path $buildDir "build.cmd"
		$batch = @(
			"@echo off"
			"chcp 65001 >nul"
			"call `"$vcvars`" >nul"
			"if errorlevel 1 exit /b %errorlevel%"
			"ml64.exe /nologo /c /Fo`"$asmObj`" `"$asmSrc`""
			"if errorlevel 1 exit /b %errorlevel%"
			"cl.exe /nologo /LD $optFlags /W4 /utf-8 /I`"$sdkInclude`" `"$cSrc`" /Fo`"$cObj`" /Fe`"$dllOut`" `"$asmObj`" /link /IMPLIB:`"$implib`""
			"if errorlevel 1 exit /b %errorlevel%"
		) -join "`r`n"
		[System.IO.File]::WriteAllText($batchPath, $batch, (New-Object System.Text.UTF8Encoding($false)))
		& cmd.exe /d /c "`"$batchPath`""
		if ($LASTEXITCODE -ne 0) {
			throw "MSVC build failed with exit code $LASTEXITCODE"
		}
	}

	Copy-Item -LiteralPath $manifestSrc -Destination $manifestOut -Force

	if (-not (Test-Path -LiteralPath $dllOut -PathType Leaf)) {
		throw "Expected output DLL was not created: $dllOut"
	}
	if (-not (Test-Path -LiteralPath $manifestOut -PathType Leaf)) {
		throw "Expected manifest was not staged: $manifestOut"
	}

	Write-Host "Staged production plugin package:"
	Write-Host "  Directory: $resolvedOutDir"
	Write-Host "  DLL:       $dllOut"
	Write-Host "  Manifest:  $manifestOut"
	Write-Host ""
	Write-Host "Next steps (Phase-4 real-game A/B):"
	Write-Host "  foch plugin import `"$resolvedOutDir`""
	Write-Host "  foch test --with-plugins dev.foch.eu4-country-month"
} finally {
	if (Test-Path -LiteralPath $buildDir) {
		Remove-Item -LiteralPath $buildDir -Recurse -Force -ErrorAction SilentlyContinue
	}
}
