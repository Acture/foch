#Requires -Version 7.2
<#
.SYNOPSIS
Install, upgrade and uninstall the foch CLI through WinGet.

.DESCRIPTION
Exercises WinGet's real portable install path for Acture.Foch:

- local mode renders manifests for a win32-x64 release archive with
  `foch_dev winget render`: the release set, checked as a release, and two
  sets served from http://127.0.0.1. It installs a lower `<Version>-smoke`
  package, upgrades to `-Version` and uninstalls;
- release mode installs `-Version` from the manifest set a release attaches
  (`-ManifestArchive`), so WinGet downloads the published archive and checks it
  against the hash the manifests name, and uninstalls;
- source mode installs `-Version` of Acture.Foch from the community `winget`
  source, after its winget-pkgs pull request merged.

Every manifest set passes `foch_dev winget check` (`--release` for a release
set) and `winget validate` before it is used. Each installed state must expose
%LOCALAPPDATA%\Microsoft\WinGet\Links\foch.exe as a symbolic link into
Packages\Acture.Foch__DefaultSource and register the ARP uninstall key with
the manifest's DisplayVersion, and `foch_dev dist check-binary` must pass on
the link: the exact `--version` identity, `--help`, a rejected unknown
subcommand and `input inspect` of a fixture project. Source mode checks only
the link and the program, because WinGet names a community install's package
directory and key after that source. Uninstall must remove the link and every
Acture.Foch_* package directory and key. On failure, WinGet's DiagOutputDir
logs are copied to <WorkDirectory>\logs.

WinGet is bootstrapped from the pinned winget-cli release below, verified by
SHA-256, unless the runner already has that version or newer. The script needs
an elevated session, because `winget settings --enable LocalManifestFiles` is
an administrator setting; GitHub-hosted Windows runners are elevated.
#>
[CmdletBinding()]
param(
	# Local mode: the win32-x64 release archive.
	[string] $ZipPath = '',

	# Release mode: the release's foch-<Version>-winget-manifests.zip.
	[string] $ManifestArchive = '',

	[Parameter(Mandatory)]
	[ValidateNotNullOrEmpty()]
	[string] $Version,

	[Parameter(Mandatory)]
	[ValidatePattern('^[0-9a-f]{64}$')]
	[string] $ExpectedCwtSchemaId,

	# Command prefix that runs foch-dev, split on whitespace, for example
	# "uv run --locked --project src/tools/foch-dev python -m foch_dev".
	[Parameter(Mandatory)]
	[ValidateNotNullOrEmpty()]
	[string] $FochDev,

	[Parameter(Mandatory)]
	[ValidateSet('local', 'release', 'source')]
	[string] $Mode,

	# Local mode: the ReleaseDate the rendered manifests name.
	[ValidatePattern('^[0-9]{4}-[0-9]{2}-[0-9]{2}$')]
	[string] $ReleaseDate = [DateTime]::UtcNow.ToString('yyyy-MM-dd'),

	# Must be empty or absent (default: $env:RUNNER_TEMP\foch-winget-smoke).
	[string] $WorkDirectory = '',

	# Python that serves the archive in local mode.
	[string] $Python = 'python',

	[ValidateRange(1, 300)]
	[int] $ServerTimeoutSeconds = 30
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
# Every native command is checked explicitly by Invoke-Native.
$PSNativeCommandUseErrorActionPreference = $false

$PackageIdentifier = 'Acture.Foch'
# winget names a --manifest portable install <id>_<source>, with '*' replaced.
$ProductCode = "${PackageIdentifier}__DefaultSource"
$PinnedWingetVersion = [version] '1.29.380'
$WingetReleaseUrl = "https://github.com/microsoft/winget-cli/releases/download/v$PinnedWingetVersion"
$WingetBundle = 'Microsoft.DesktopAppInstaller_8wekyb3d8bbwe.msixbundle'
$WingetDependencies = 'DesktopAppInstaller_Dependencies.zip'
$WingetAssetSha256 = @{
	$WingetBundle = '65DEA9C01CE08EE7B763366B27C0E651F97DB857C11CA9B9C301826C10092F2E'
	$WingetDependencies = 'BA875AFE9D190F61218985AC0292A99D1DB710BF93E13C68944CA9D89F0D82D1'
}
# Microsoft.VCLibs.140.00, Microsoft.VCLibs.140.00.UWPDesktop and
# Microsoft.WindowsAppRuntime.1.8 under x64\ in the dependencies archive.
$WingetX64DependencyCount = 3
$WingetExe = Join-Path $env:LOCALAPPDATA 'Microsoft\WindowsApps\winget.exe'
$WingetDiagnostics = Join-Path $env:LOCALAPPDATA 'Packages\Microsoft.DesktopAppInstaller_8wekyb3d8bbwe\LocalState\DiagOutputDir'
$LinkPath = Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Links\foch.exe'
$PackageDirectory = Join-Path $env:LOCALAPPDATA "Microsoft\WinGet\Packages\$ProductCode"
$PackagedExecutable = Join-Path $PackageDirectory 'foch.exe'
$PackagesRoot = Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Packages'
$UninstallRoot = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall'
$UninstallKey = "$UninstallRoot\$ProductCode"
$PackageAgreements = @(
	'--accept-package-agreements',
	'--accept-source-agreements',
	'--disable-interactivity',
	'--verbose-logs'
)

function Invoke-Native {
	[CmdletBinding()]
	[OutputType([string[]])]
	param(
		[Parameter(Mandatory)]
		[string] $FilePath,

		[string[]] $ArgumentList = @()
	)

	Write-Host "> $FilePath $($ArgumentList -join ' ')"
	[string[]] $output = @(& $FilePath @ArgumentList | ForEach-Object { "$_" })
	$exitCode = $LASTEXITCODE
	foreach ($line in $output) {
		Write-Host $line
	}
	if ($exitCode -ne 0) {
		throw ('{0} {1} exited with code {2} (0x{2:X8})' -f $FilePath, ($ArgumentList -join ' '), $exitCode)
	}
	return $output
}

function Invoke-Prefixed {
	[CmdletBinding()]
	[OutputType([string[]])]
	param(
		[Parameter(Mandatory)]
		[string] $Prefix,

		[Parameter(Mandatory)]
		[string[]] $ArgumentList
	)

	$parts = @($Prefix -split '\s+' | Where-Object { $_ -ne '' })
	if ($parts.Count -eq 0) {
		throw 'Command prefix is empty'
	}
	$leading = @($parts | Select-Object -Skip 1)
	return Invoke-Native -FilePath $parts[0] -ArgumentList ($leading + $ArgumentList)
}

function Get-WingetVersion {
	if (-not (Test-Path -LiteralPath $WingetExe)) {
		return $null
	}
	$reported = @(Invoke-Native -FilePath $WingetExe -ArgumentList @('--version'))
	$text = ($reported -join "`n").Trim()
	if ($text -notmatch '^v([0-9]+\.[0-9]+\.[0-9]+)') {
		throw "Unexpected winget --version output '$text'"
	}
	return [version] $Matches[1]
}

function Install-PinnedWinget {
	[CmdletBinding()]
	param(
		[Parameter(Mandatory)]
		[string] $Directory
	)

	New-Item -ItemType Directory -Force -Path $Directory | Out-Null
	foreach ($name in @($WingetBundle, $WingetDependencies)) {
		$path = Join-Path $Directory $name
		Write-Host "Downloading $WingetReleaseUrl/$name"
		Invoke-WebRequest -Uri "$WingetReleaseUrl/$name" -OutFile $path
		$actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
		if ($actual -ne $WingetAssetSha256[$name]) {
			throw "$name has SHA-256 $actual, expected $($WingetAssetSha256[$name])"
		}
	}

	$dependencyRoot = Join-Path $Directory 'dependencies'
	Expand-Archive -LiteralPath (Join-Path $Directory $WingetDependencies) -DestinationPath $dependencyRoot
	$dependencies = @(
		Get-ChildItem -LiteralPath (Join-Path $dependencyRoot 'x64') -Filter '*.appx' -File |
			Sort-Object -Property Name
	)
	if ($dependencies.Count -ne $WingetX64DependencyCount) {
		$found = ($dependencies | ForEach-Object { $_.Name }) -join ', '
		throw "Expected $WingetX64DependencyCount x64 packages in $WingetDependencies; found '$found'"
	}

	# Appx is a Windows PowerShell module; run it there with its own exit code.
	$quoted = { param([string] $Value) "'" + ($Value -replace "'", "''") + "'" }
	$dependencyList = ($dependencies | ForEach-Object { & $quoted $_.FullName }) -join ','
	$command = "`$ErrorActionPreference = 'Stop'; Add-AppxPackage -Path $(& $quoted (Join-Path $Directory $WingetBundle)) -DependencyPath $dependencyList -ForceApplicationShutdown"
	$encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
	Invoke-Native -FilePath 'powershell.exe' -ArgumentList @(
		'-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', $encoded
	) | Out-Null
}

function Initialize-Winget {
	[CmdletBinding()]
	param(
		[Parameter(Mandatory)]
		[string] $Directory
	)

	$installed = Get-WingetVersion
	Write-Host "Preinstalled winget: $(if ($null -eq $installed) { 'none' } else { "v$installed" })"
	if ($null -eq $installed -or $installed -lt $PinnedWingetVersion) {
		Install-PinnedWinget -Directory $Directory
		$installed = Get-WingetVersion
		if ($null -eq $installed -or $installed -lt $PinnedWingetVersion) {
			throw "Installing winget v$PinnedWingetVersion left '$installed'"
		}
	}
	Write-Host "Using winget v$installed at '$WingetExe'"
	Invoke-Native -FilePath $WingetExe -ArgumentList @('--info') | Out-Null
	Invoke-Native -FilePath $WingetExe -ArgumentList @('settings', '--enable', 'LocalManifestFiles') | Out-Null
}

function Copy-Asset {
	[CmdletBinding()]
	param(
		[Parameter(Mandatory)]
		[string] $Source,

		[Parameter(Mandatory)]
		[string] $Directory,

		[Parameter(Mandatory)]
		[string] $PackageVersion
	)

	$destination = Join-Path $Directory "foch-$PackageVersion-win32-x64.zip"
	Copy-Item -LiteralPath $Source -Destination $destination
	return $destination
}

function Test-ManifestSet {
	[CmdletBinding()]
	param(
		[Parameter(Mandatory)]
		[string] $Directory,

		[switch] $Release
	)

	$check = @('winget', 'check', $Directory)
	if ($Release) {
		$check += '--release'
	}
	Invoke-Prefixed -Prefix $FochDev -ArgumentList $check | Out-Null
	# Warnings, such as a schema header this client does not know, exit non-zero too.
	Invoke-Native -FilePath $WingetExe -ArgumentList @('validate', '--manifest', $Directory, '--disable-interactivity') | Out-Null
}

function Build-ManifestSet {
	[CmdletBinding()]
	param(
		[Parameter(Mandatory)]
		[string] $PackageVersion,

		[Parameter(Mandatory)]
		[string] $Asset,

		[Parameter(Mandatory)]
		[string] $OutDirectory,

		[string] $InstallerUrl = '',

		[switch] $Release
	)

	$render = @(
		'winget', 'render',
		'--asset', $Asset,
		'--version', $PackageVersion,
		'--release-date', $ReleaseDate,
		'--out', $OutDirectory
	)
	if ($InstallerUrl -ne '') {
		$render += @('--installer-url', $InstallerUrl)
	}
	# render prints the manifest directory it wrote as its last line.
	$directory = @(Invoke-Prefixed -Prefix $FochDev -ArgumentList $render)[-1]
	Test-ManifestSet -Directory $directory -Release:$Release
	return $directory
}

function Assert-Installed {
	[CmdletBinding()]
	param(
		[Parameter(Mandatory)]
		[string] $PackageVersion
	)

	if (-not (Test-Path -LiteralPath $UninstallKey)) {
		throw "winget did not register the uninstall key '$UninstallKey'"
	}
	$displayVersion = Get-ItemPropertyValue -LiteralPath $UninstallKey -Name 'DisplayVersion'
	if ($displayVersion -ne $PackageVersion) {
		throw "Uninstall key reports DisplayVersion '$displayVersion', expected '$PackageVersion'"
	}
	if (-not (Test-Path -LiteralPath $PackagedExecutable -PathType Leaf)) {
		throw "winget did not install '$PackagedExecutable'"
	}
	if (-not (Test-Path -LiteralPath $LinkPath)) {
		throw "winget did not create the command alias '$LinkPath'"
	}
	$link = Get-Item -LiteralPath $LinkPath -Force
	if ($link.LinkType -ne 'SymbolicLink') {
		throw "'$LinkPath' must be a symbolic link; found link type '$($link.LinkType)'"
	}
	$target = [IO.Path]::GetFullPath($link.ResolveLinkTarget($true).FullName)
	if (-not [string]::Equals($target, [IO.Path]::GetFullPath($PackagedExecutable), [StringComparison]::OrdinalIgnoreCase)) {
		throw "'$LinkPath' points at '$target', expected '$PackagedExecutable'"
	}
	Write-Host "$PackageIdentifier $PackageVersion is installed: '$LinkPath' -> '$target'"
}

function Assert-SourceInstalled {
	if (-not (Test-Path -LiteralPath $LinkPath -PathType Leaf)) {
		throw "winget did not create the command alias '$LinkPath'"
	}
	$link = Get-Item -LiteralPath $LinkPath -Force
	if ($link.LinkType -ne 'SymbolicLink') {
		throw "'$LinkPath' must be a symbolic link; found link type '$($link.LinkType)'"
	}
	Write-Host "$PackageIdentifier $Version is installed from the winget source: '$LinkPath' -> '$($link.ResolveLinkTarget($true).FullName)'"
}

function Get-InstalledState {
	# Every package directory and ARP key WinGet names after this package, whatever the source.
	@(
		if (Test-Path -LiteralPath $PackagesRoot) {
			Get-ChildItem -LiteralPath $PackagesRoot -Directory -Filter "${PackageIdentifier}_*" | ForEach-Object { $_.FullName }
		}
		if (Test-Path -LiteralPath $UninstallRoot) {
			Get-ChildItem -LiteralPath $UninstallRoot | Where-Object { $_.PSChildName -like "${PackageIdentifier}_*" } | ForEach-Object { $_.Name }
		}
	)
}

function Assert-Uninstalled {
	$linkRemains = (Test-Path -LiteralPath $LinkPath) -or ($null -ne [IO.FileInfo]::new($LinkPath).LinkTarget)
	$remaining = @(
		if ($linkRemains) { $LinkPath }
		Get-InstalledState
	)
	if ($remaining.Count -gt 0) {
		throw "winget uninstall left behind: $($remaining -join ', ')"
	}
	Write-Host "$PackageIdentifier is uninstalled"
}

function Test-Foch {
	# The installed-binary checks every channel runs, in a scratch home without
	# FOCH_* overrides.
	Invoke-Prefixed -Prefix $FochDev -ArgumentList @(
		'dist', 'check-binary', $LinkPath,
		'--expect-version', $Version,
		'--expect-cwt-schema-id', $ExpectedCwtSchemaId
	) | Out-Null
}

function Install-FochManifest {
	[CmdletBinding()]
	param(
		[Parameter(Mandatory)]
		[ValidateSet('install', 'upgrade')]
		[string] $Command,

		[Parameter(Mandatory)]
		[string] $Directory
	)

	Invoke-Native -FilePath $WingetExe -ArgumentList (@($Command, '--manifest', $Directory) + $PackageAgreements) | Out-Null
}

function Install-FochSource {
	Invoke-Native -FilePath $WingetExe -ArgumentList (@(
		'install', '--id', $PackageIdentifier, '--exact', '--source', 'winget', '--version', $Version
	) + $PackageAgreements) | Out-Null
}

function Uninstall-Foch {
	$selector = if ($Mode -eq 'source') {
		@('--id', $PackageIdentifier, '--source', 'winget')
	} else {
		@('--product-code', $ProductCode)
	}
	Invoke-Native -FilePath $WingetExe -ArgumentList (@('uninstall') + $selector + @(
		'--exact', '--accept-source-agreements', '--disable-interactivity', '--verbose-logs'
	)) | Out-Null
}

function Open-LocalServer {
	[CmdletBinding()]
	param(
		[Parameter(Mandatory)]
		[string] $Directory,

		[Parameter(Mandatory)]
		[string] $LogDirectory
	)

	$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
	$listener.Start()
	try {
		$port = ([System.Net.IPEndPoint] $listener.LocalEndpoint).Port
	} finally {
		$listener.Stop()
	}
	$process = Start-Process -FilePath $Python -PassThru -NoNewWindow `
		-ArgumentList @('-m', 'http.server', "$port", '--bind', '127.0.0.1', '--directory', "`"$Directory`"") `
		-RedirectStandardOutput (Join-Path $LogDirectory 'http-server.out.log') `
		-RedirectStandardError (Join-Path $LogDirectory 'http-server.err.log')
	$baseUrl = "http://127.0.0.1:$port"
	$deadline = [DateTime]::UtcNow.AddSeconds($ServerTimeoutSeconds)
	while ($true) {
		if ($process.HasExited) {
			throw "Local HTTP server exited with code $($process.ExitCode); see '$LogDirectory'"
		}
		try {
			Invoke-WebRequest -Uri "$baseUrl/" -Method Head -TimeoutSec 2 | Out-Null
			break
		} catch {
			# Polling until the server listens is the recovery strategy here.
			if ([DateTime]::UtcNow -ge $deadline) {
				throw "Local HTTP server did not answer on $baseUrl within $ServerTimeoutSeconds seconds: $($_.Exception.Message)"
			}
			Start-Sleep -Milliseconds 250
		}
	}
	Write-Host "Serving '$Directory' on $baseUrl"
	return [pscustomobject] @{ Process = $process; BaseUrl = $baseUrl }
}

function Save-WingetLog {
	[CmdletBinding()]
	param(
		[Parameter(Mandatory)]
		[string] $Destination
	)

	Write-Host "winget diagnostics: '$WingetDiagnostics'"
	if (-not (Test-Path -LiteralPath $WingetDiagnostics)) {
		Write-Host 'winget wrote no diagnostics'
		return
	}
	$target = Join-Path $Destination 'winget'
	New-Item -ItemType Directory -Force -Path $target | Out-Null
	Copy-Item -Path (Join-Path $WingetDiagnostics '*') -Destination $target -Recurse -Force
	Write-Host "Copied winget logs to '$target'"
}

$tempBase = if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
	[IO.Path]::GetTempPath()
} else {
	$env:RUNNER_TEMP
}
$work = if ($WorkDirectory -eq '') {
	Join-Path $tempBase 'foch-winget-smoke'
} else {
	[IO.Path]::GetFullPath($WorkDirectory)
}
if ((Test-Path -LiteralPath $work) -and @(Get-ChildItem -LiteralPath $work -Force).Count -gt 0) {
	throw "Work directory '$work' must be empty or absent"
}
$assetDirectory = Join-Path $work 'assets'
$logDirectory = Join-Path $work 'logs'
foreach ($directory in @($work, $assetDirectory, $logDirectory)) {
	New-Item -ItemType Directory -Force -Path $directory | Out-Null
}
if ($Mode -eq 'local' -and $ZipPath -eq '') {
	throw '-ZipPath is required in local mode'
}
if ($Mode -eq 'release' -and $ManifestArchive -eq '') {
	throw '-ManifestArchive is required in release mode'
}

$server = $null
$primaryFailure = $null
$cleanupFailures = [System.Collections.Generic.List[string]]::new()
try {
	Initialize-Winget -Directory (Join-Path $work 'winget')
	if ($Mode -eq 'source') {
		Install-FochSource
		Assert-SourceInstalled
		Test-Foch
	} elseif ($Mode -eq 'release') {
		$releaseRoot = Join-Path $work 'release'
		Expand-Archive -LiteralPath (Resolve-Path -LiteralPath $ManifestArchive).Path -DestinationPath $releaseRoot
		$releaseManifests = Join-Path $releaseRoot "manifests\a\Acture\Foch\$Version"
		Test-ManifestSet -Directory $releaseManifests -Release
		Install-FochManifest -Command install -Directory $releaseManifests
		Assert-Installed -PackageVersion $Version
		Test-Foch
	} else {
		$zip = (Resolve-Path -LiteralPath $ZipPath).Path
		$releaseAsset = Copy-Asset -Source $zip -Directory $assetDirectory -PackageVersion $Version
		Build-ManifestSet -PackageVersion $Version -Asset $releaseAsset `
			-OutDirectory (Join-Path $work 'release') -Release | Out-Null
		# A suffix on the last numeric part sorts below the plain version in winget.
		$previousVersion = "$Version-smoke"
		$previousAsset = Copy-Asset -Source $zip -Directory $assetDirectory -PackageVersion $previousVersion
		$server = Open-LocalServer -Directory $assetDirectory -LogDirectory $logDirectory
		$localRoot = Join-Path $work 'local'
		$previousManifests = Build-ManifestSet -PackageVersion $previousVersion -Asset $previousAsset `
			-OutDirectory $localRoot -InstallerUrl "$($server.BaseUrl)/$(Split-Path -Leaf $previousAsset)"
		$currentManifests = Build-ManifestSet -PackageVersion $Version -Asset $releaseAsset `
			-OutDirectory $localRoot -InstallerUrl "$($server.BaseUrl)/$(Split-Path -Leaf $releaseAsset)"

		Install-FochManifest -Command install -Directory $previousManifests
		Assert-Installed -PackageVersion $previousVersion
		Test-Foch

		Install-FochManifest -Command upgrade -Directory $currentManifests
		Assert-Installed -PackageVersion $Version
		Test-Foch
	}

	Uninstall-Foch
	Assert-Uninstalled
	Write-Host "WinGet $Mode smoke passed"
} catch {
	$primaryFailure = $_
} finally {
	if ($null -ne $server) {
		try {
			if (-not $server.Process.HasExited) {
				Stop-Process -Id $server.Process.Id -Force
			}
		} catch {
			$cleanupFailures.Add("local HTTP server: $($_.Exception.Message)")
		} finally {
			$server.Process.Dispose()
		}
	}
	if ((Test-Path -LiteralPath $LinkPath) -or @(Get-InstalledState).Count -gt 0) {
		try {
			Uninstall-Foch
		} catch {
			$cleanupFailures.Add("winget uninstall: $($_.Exception.Message)")
		}
	}
	if ($null -ne $primaryFailure -or $cleanupFailures.Count -gt 0) {
		try {
			Save-WingetLog -Destination $logDirectory
		} catch {
			$cleanupFailures.Add("saving winget logs: $($_.Exception.Message)")
		}
	}
}

if ($null -ne $primaryFailure) {
	if ($cleanupFailures.Count -gt 0) {
		throw "$($primaryFailure.Exception.Message)`nCleanup also failed: $($cleanupFailures -join '; ')"
	}
	throw $primaryFailure
}
if ($cleanupFailures.Count -gt 0) {
	throw "WinGet smoke cleanup failed: $($cleanupFailures -join '; ')"
}
