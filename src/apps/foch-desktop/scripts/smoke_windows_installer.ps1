[CmdletBinding()]
param(
	[Parameter(Mandatory)]
	[ValidateNotNullOrEmpty()]
	[string] $InstallerDirectory,

	[ValidateRange(1, 300)]
	[int] $WindowTimeoutSeconds = 30,

	[ValidateRange(1, 300)]
	[int] $ProcessTimeoutSeconds = 120
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Invoke-BoundedProcess {
	[CmdletBinding()]
	param(
		[Parameter(Mandatory)]
		[string] $FilePath,

		[Parameter(Mandatory)]
		[string] $ArgumentList,

		[Parameter(Mandatory)]
		[string] $Description,

		[Parameter(Mandatory)]
		[int] $TimeoutSeconds
	)

	$process = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -PassThru
	try {
		if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
			try {
				Stop-Process -Id $process.Id -Force -ErrorAction Stop
			} catch {
				Write-Warning "Failed to stop timed-out ${Description}: $($_.Exception.Message)"
			}
			throw "$Description did not exit within $TimeoutSeconds seconds"
		}

		if ($process.ExitCode -ne 0) {
			throw "$Description exited with code $($process.ExitCode)"
		}
	} finally {
		$process.Dispose()
	}
}

$resolvedInstallerDirectory = (Resolve-Path -LiteralPath $InstallerDirectory).Path
$installers = @(
	Get-ChildItem -LiteralPath $resolvedInstallerDirectory -Filter "*-setup.exe" -File
)
if ($installers.Count -ne 1) {
	$found = if ($installers.Count -eq 0) {
		"none"
	} else {
		($installers.Name | Sort-Object) -join ", "
	}
	throw "Expected exactly one NSIS installer in '$resolvedInstallerDirectory'; found $found"
}

$tempBase = if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
	[System.IO.Path]::GetTempPath()
} else {
	$env:RUNNER_TEMP
}
$smokeRoot = Join-Path $tempBase "foch-desktop-smoke-$([Guid]::NewGuid().ToString('N'))"
$installDirectory = Join-Path $smokeRoot "install"
$desktopPath = Join-Path $installDirectory "foch-desktop.exe"
$uninstallerPath = Join-Path $installDirectory "uninstall.exe"
$desktopProcess = $null
$primaryFailure = $null
$cleanupFailures = [System.Collections.Generic.List[string]]::new()

try {
	New-Item -ItemType Directory -Path $smokeRoot | Out-Null
	Write-Host "Installing '$($installers[0].FullName)' into '$installDirectory'"
	# NSIS requires /D= to be the final argument. Tauri's /NS flag prevents
	# the silent installer from leaving desktop and Start Menu shortcuts behind.
	Invoke-BoundedProcess `
		-FilePath $installers[0].FullName `
		-ArgumentList "/S /NS /D=$installDirectory" `
		-Description "NSIS installer" `
		-TimeoutSeconds $ProcessTimeoutSeconds

	if (-not (Test-Path -LiteralPath $installDirectory -PathType Container)) {
		throw "NSIS installer did not create '$installDirectory'"
	}

	$relativeExecutables = @(
		Get-ChildItem -LiteralPath $installDirectory -Filter "*.exe" -File -Recurse |
			ForEach-Object {
				[System.IO.Path]::GetRelativePath($installDirectory, $_.FullName)
			}
	)
	$forbiddenExecutables = @(
		$relativeExecutables | Where-Object {
			[System.IO.Path]::GetFileName($_) -ieq "foch.exe"
		}
	)
	if ($forbiddenExecutables.Count -gt 0) {
		throw "Installed payload contains the forbidden CLI executable: $($forbiddenExecutables -join ', ')"
	}

	$expectedExecutables = @("foch-desktop.exe", "uninstall.exe")
	$unexpectedExecutables = @(
		$relativeExecutables | Where-Object { $_ -notin $expectedExecutables }
	)
	if ($unexpectedExecutables.Count -gt 0) {
		throw "Installed payload contains unexpected executables: $($unexpectedExecutables -join ', ')"
	}
	foreach ($expectedExecutable in $expectedExecutables) {
		if ($expectedExecutable -notin $relativeExecutables) {
			throw "Installed payload is missing '$expectedExecutable'"
		}
	}

	Write-Host "Launching installed desktop executable '$desktopPath'"
	$desktopProcess = Start-Process -FilePath $desktopPath -PassThru
	$windowDeadline = [DateTime]::UtcNow.AddSeconds($WindowTimeoutSeconds)
	do {
		Start-Sleep -Milliseconds 250
		$desktopProcess.Refresh()
		if ($desktopProcess.HasExited) {
			throw "Installed foch-desktop exited before opening a window with code $($desktopProcess.ExitCode)"
		}
		if ($desktopProcess.MainWindowHandle -ne [IntPtr]::Zero) {
			Write-Host "Installed Foch window opened with handle $($desktopProcess.MainWindowHandle)"
			break
		}
	} while ([DateTime]::UtcNow -lt $windowDeadline)

	if ($desktopProcess.MainWindowHandle -eq [IntPtr]::Zero) {
		throw "Installed foch-desktop remained alive but did not open a window within $WindowTimeoutSeconds seconds"
	}
} catch {
	$primaryFailure = $_
} finally {
	if ($null -ne $desktopProcess) {
		try {
			$desktopProcess.Refresh()
			if (-not $desktopProcess.HasExited) {
				Stop-Process -Id $desktopProcess.Id -Force -ErrorAction Stop
				if (-not $desktopProcess.WaitForExit(10000)) {
					throw "foch-desktop did not stop within 10 seconds"
				}
			}
		} catch {
			$cleanupFailures.Add("desktop process: $($_.Exception.Message)")
		} finally {
			$desktopProcess.Dispose()
		}
	}

	if (Test-Path -LiteralPath $uninstallerPath -PathType Leaf) {
		try {
			# _?= keeps the uninstaller in this process and must remain last.
			Invoke-BoundedProcess `
				-FilePath $uninstallerPath `
				-ArgumentList "/S _?=$installDirectory" `
				-Description "NSIS uninstaller" `
				-TimeoutSeconds $ProcessTimeoutSeconds
		} catch {
			$cleanupFailures.Add("NSIS uninstaller: $($_.Exception.Message)")
		}
	}

	if (Test-Path -LiteralPath $smokeRoot) {
		try {
			Remove-Item -LiteralPath $smokeRoot -Recurse -Force
		} catch {
			$cleanupFailures.Add("temporary directory: $($_.Exception.Message)")
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
	throw "Desktop package smoke cleanup failed: $($cleanupFailures -join '; ')"
}
