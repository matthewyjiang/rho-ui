# Installs rho-ui from a GitHub release, on Windows (x86_64). The docs site
# serves it, so this installs the latest release:
#
#   irm https://matthewyjiang.github.io/rho-ui/install.ps1 | iex
#
# $env:RHO_UI_VERSION = "0.1.0" installs that release instead. It puts
# rho-ui.exe in $env:RHO_UI_INSTALL_DIR (%LOCALAPPDATA%\Programs\rho-ui),
# adds that folder to your user PATH, and adds Rho to the Start menu.
# Running it again updates rho-ui in place.
$ErrorActionPreference = "Stop"
# Windows PowerShell's download progress bar slows the download many times over.
$ProgressPreference = "SilentlyContinue"

$Repo = "matthewyjiang/rho-ui"
$Version = if ($env:RHO_UI_VERSION) { $env:RHO_UI_VERSION } else { "latest" }
$InstallDir = if ($env:RHO_UI_INSTALL_DIR) { $env:RHO_UI_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\rho-ui" }
$FromSource = "https://matthewyjiang.github.io/rho-ui/guide/getting-started#from-source"
$Target = "x86_64-windows"
$Asset = "rho-ui-$Target.zip"

$IsWindowsHost = if (Get-Variable -Name IsWindows -ErrorAction SilentlyContinue) { $IsWindows } else { $true }
if (-not $IsWindowsHost) {
    throw "install.ps1 is for Windows. On Linux or macOS, use install.sh."
}
# A 32-bit PowerShell names the OS's architecture in PROCESSOR_ARCHITEW6432.
$Arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
if ($Arch -ne "AMD64") {
    throw "no rho-ui build for Windows on $Arch; build it from source: $FromSource"
}

$Url = switch -Wildcard ($Version) {
    "latest" { "https://github.com/$Repo/releases/latest/download" }
    "v*" { "https://github.com/$Repo/releases/download/$Version" }
    default { "https://github.com/$Repo/releases/download/v$Version" }
}

function Get-File($Name, $OutFile) {
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$Url/$Name" -OutFile $OutFile
    } catch {
        throw "can't download $Url/${Name}: check that release $Version exists. $($_.Exception.Message)"
    }
}

# Adds $Dir to the user's PATH, for new terminals and this one. Returns
# whether it wasn't there already.
function Add-ToUserPath($Dir) {
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $Parts = if ($UserPath) { $UserPath -split ";" | Where-Object { $_ } } else { @() }
    if ($Parts -contains $Dir) {
        return $false
    }
    $NewPath = if ($UserPath) { "$UserPath;$Dir" } else { $Dir }
    [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
    $env:Path = "$env:Path;$Dir"
    return $true
}

$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) "rho-ui-install-$([System.Guid]::NewGuid())"
New-Item -ItemType Directory -Force -Path $TempDir | Out-Null
try {
    Write-Host "downloading $Asset ($Version)..."
    $Archive = Join-Path $TempDir $Asset
    $Sums = Join-Path $TempDir "SHA256SUMS"
    Get-File $Asset $Archive
    Get-File "SHA256SUMS" $Sums
    $Expected = Get-Content $Sums |
        ForEach-Object { $Sum, $Name = $_ -split "\s+", 2; if ($Name -eq $Asset) { $Sum.ToLowerInvariant() } } |
        Select-Object -First 1
    $Actual = (Get-FileHash -Algorithm SHA256 $Archive).Hash.ToLowerInvariant()
    if (-not $Expected -or $Actual -ne $Expected) {
        throw "$Asset doesn't match its checksum in SHA256SUMS"
    }

    Expand-Archive -Path $Archive -DestinationPath $TempDir -Force
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $Exe = Join-Path $InstallDir "rho-ui.exe"
    try {
        Copy-Item -Path (Join-Path $TempDir "rho-ui-$Target\rho-ui.exe") -Destination $Exe -Force
    } catch {
        throw "can't replace $Exe; quit rho-ui and run this again. $($_.Exception.Message)"
    }

    # `--help` exits before any window opens, with 2, so this checks that
    # rho-ui.exe loads here: a missing DLL fails now, not at the first launch.
    $Check = Start-Process -FilePath $Exe -ArgumentList "--help" -Wait -PassThru -WindowStyle Hidden
    if ($Check.ExitCode -ne 2) {
        throw "rho-ui is installed at $Exe but doesn't run here: it exited with $($Check.ExitCode)"
    }

    $Programs = [Environment]::GetFolderPath("Programs")
    $Shell = New-Object -ComObject WScript.Shell
    $Shortcut = $Shell.CreateShortcut((Join-Path $Programs "Rho.lnk"))
    $Shortcut.TargetPath = $Exe
    $Shortcut.Description = "The desktop app for the rho coding agent"
    $Shortcut.Save()

    Write-Host "rho-ui installed to $Exe, and Rho added to the Start menu"
    if (Add-ToUserPath $InstallDir) {
        Write-Host "added $InstallDir to your user PATH: open a new terminal to run rho-ui there"
    }
} finally {
    Remove-Item -Recurse -Force $TempDir -ErrorAction SilentlyContinue
}
