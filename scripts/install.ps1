# Installs rho-ui from a GitHub release, on Windows (x86_64). The docs site
# serves it, so this installs the latest release:
#
#   irm https://matthewyjiang.github.io/rho-ui/install.ps1 | iex
#
# $env:RHO_UI_VERSION = "0.1.0" installs that release instead. It puts
# rho-ui.exe in $env:RHO_UI_INSTALL_DIR (%LOCALAPPDATA%\Programs\rho-ui),
# adds that folder to your user PATH, and adds Rho to the Start menu.
# Running it again updates rho-ui in place.

# In a script block, so `irm | iex` leaves none of its settings, variables
# or functions in your session.
& {
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
    #
    # It edits the registry's value as written, a REG_EXPAND_SZ, so entries
    # such as %USERPROFILE%\bin keep their variables:
    # [Environment]::SetEnvironmentVariable would write them out expanded,
    # as a plain string. So it tells running programs of the change itself,
    # as rustup does, waiting up to rustup's 5 seconds on each.
    function Add-ToUserPath($Dir) {
        $Key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey("Environment")
        try {
            $UserPath = $Key.GetValue("Path", "", [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $Parts = $UserPath -split ";" | Where-Object { $_ } |
                ForEach-Object { [Environment]::ExpandEnvironmentVariables($_) }
            if ($Parts -contains $Dir) {
                return $false
            }
            $NewPath = if ($UserPath) { "$($UserPath.TrimEnd(';'));$Dir" } else { $Dir }
            $Key.SetValue("Path", $NewPath, [Microsoft.Win32.RegistryValueKind]::ExpandString)
        } finally {
            $Key.Close()
        }

        if (-not ("RhoUiInstall.Native" -as [type])) {
            Add-Type -Namespace RhoUiInstall -Name Native -MemberDefinition @"
[DllImport("user32.dll", CharSet = CharSet.Unicode)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint msg, UIntPtr wParam, string lParam, uint flags, uint timeout, out UIntPtr result);
"@
        }
        $HwndBroadcast = [IntPtr]0xffff
        $WmSettingChange = 0x1a
        $SmtoAbortIfHung = 2
        $Result = [UIntPtr]::Zero
        [RhoUiInstall.Native]::SendMessageTimeout($HwndBroadcast, $WmSettingChange, [UIntPtr]::Zero, "Environment", $SmtoAbortIfHung, 5000, [ref]$Result) | Out-Null

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
        $NewExe = Join-Path $TempDir "rho-ui-$Target\rho-ui.exe"

        # Checks that the new rho-ui.exe runs here before it replaces
        # anything. `--help` exits with 2 before any window opens (main.rs).
        $Check = Start-Process -FilePath $NewExe -ArgumentList "--help" -Wait -PassThru -WindowStyle Hidden
        if ($Check.ExitCode -eq -1073741515) {
            throw "this rho-ui build doesn't run here: Windows can't find a DLL it needs. Install the Microsoft Visual C++ Redistributable (x64), then run this again. Nothing was installed."
        } elseif ($Check.ExitCode -ne 2) {
            throw "this rho-ui build doesn't run here: it exited with $($Check.ExitCode). Nothing was installed."
        }

        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
        $Exe = Join-Path $InstallDir "rho-ui.exe"
        try {
            Copy-Item -Path $NewExe -Destination $Exe -Force
        } catch {
            throw "can't replace $Exe; quit rho-ui and run this again. $($_.Exception.Message)"
        }

        $Programs = [Environment]::GetFolderPath("Programs")
        $Shell = New-Object -ComObject WScript.Shell
        $Shortcut = $Shell.CreateShortcut((Join-Path $Programs "Rho.lnk"))
        $Shortcut.TargetPath = $Exe
        # Started in its own folder, rho-ui opens new sessions in your home
        # folder; with none set, it would start in Explorer's, System32.
        $Shortcut.WorkingDirectory = $InstallDir
        $Shortcut.Description = "The desktop app for the rho coding agent"
        $Shortcut.Save()

        Write-Host "rho-ui installed to $Exe, and Rho added to the Start menu"
        if (Add-ToUserPath $InstallDir) {
            Write-Host "added $InstallDir to your user PATH"
        }
    } finally {
        Remove-Item -Recurse -Force $TempDir -ErrorAction SilentlyContinue
    }
}
