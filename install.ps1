# Ken installer for Windows
#
# Install the latest release of Ken (paste into PowerShell):
#   powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/smo-key/ken/main/install.ps1 | iex"
#
# Prefer clicking? Download an installer instead:
#   https://github.com/smo-key/ken/releases/latest
#
# What this script does:
#   - checks your processor (Ken ships an x64 Windows build)
#   - downloads the Ken setup program and the ken-mcp helper from the
#     latest GitHub release (asset names are the contract with
#     .github/workflows/release.yml: Ken-<target>-setup.exe,
#     ken-mcp-<target>.exe)
#   - runs the setup program silently (installs just for your user
#     account - no administrator prompt)
#   - checks whether Claude Code is installed (Ken's AI features use it)
#
# Environment overrides (mainly for testing):
#   KEN_INSTALL_PREFIX  put ken-mcp under <prefix>\bin instead of
#                       %LOCALAPPDATA%\Ken\bin, and skip running the app
#                       setup program (so tests never touch the real system)
#   KEN_DOWNLOAD_BASE   fetch release assets from this base URL instead of
#                       https://github.com/smo-key/ken/releases/latest/download

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
# Windows PowerShell 5.1 defaults to old TLS; GitHub needs 1.2+.
try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12 } catch {}

$Repo = 'smo-key/ken'
$RepoUrl = "https://github.com/$Repo"
$ReleasesPage = "$RepoUrl/releases/latest"
$DownloadBase = if ($env:KEN_DOWNLOAD_BASE) { $env:KEN_DOWNLOAD_BASE } else { "$RepoUrl/releases/latest/download" }

function Fail([string[]]$Lines) {
    Write-Host "Sorry - $($Lines[0])" -ForegroundColor Red
    foreach ($line in $Lines | Select-Object -Skip 1) { Write-Host $line -ForegroundColor Red }
    exit 1
}

# --- Figure out where we are running -----------------------------------

if ($env:OS -ne 'Windows_NT') {
    Fail @(
        'this script is for Windows. On macOS or Linux, run:',
        '  curl -fsSL https://raw.githubusercontent.com/smo-key/ken/main/install.sh | sh'
    )
}

$arch = $env:PROCESSOR_ARCHITECTURE
if ($env:PROCESSOR_ARCHITEW6432) { $arch = $env:PROCESSOR_ARCHITEW6432 }
if ($arch -ne 'AMD64') {
    Fail @(
        "Ken doesn't have a Windows build for the '$arch' processor yet.",
        "You can find all downloads at $ReleasesPage"
    )
}
$Target = 'x86_64-pc-windows-msvc'

# Where the ken-mcp helper goes. KEN_INSTALL_PREFIX reroutes it (and skips
# the app setup program) so the script can be tested without touching the
# real system.
if ($env:KEN_INSTALL_PREFIX) {
    $BinDir = Join-Path $env:KEN_INSTALL_PREFIX 'bin'
} else {
    $BinDir = Join-Path $env:LOCALAPPDATA 'Ken\bin'
}

Write-Host 'Installing Ken for Windows (x64).'

$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("ken-install-" + [System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $TmpDir -Force | Out-Null

try {
    # download <url> <destination>: $true on success, $false if the file
    # doesn't exist on the server (404). Any other problem stops the script.
    function Download([string]$Url, [string]$Dest) {
        try {
            Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing
            return $true
        } catch {
            $status = $null
            if ($_.Exception.Response) {
                try { $status = [int]$_.Exception.Response.StatusCode } catch {}
            }
            if ($status -eq 404) { return $false }
            Fail @(
                "couldn't download Ken. Please check your internet connection",
                "and run this again. (URL: $Url)",
                "Details: $($_.Exception.Message)"
            )
        }
    }

    $setupExe = Join-Path $TmpDir 'ken-setup.exe'
    Write-Host 'Downloading Ken...'
    if (-not (Download "$DownloadBase/Ken-$Target-setup.exe" $setupExe)) {
        Fail @(
            "there's no packaged Windows release yet.",
            "You can check for one at $ReleasesPage, or build from source -",
            "see the Development section of $RepoUrl#readme"
        )
    }

    $mcpExe = Join-Path $TmpDir 'ken-mcp.exe'
    Write-Host 'Downloading the ken-mcp helper (lets other AI agents use your knowledge base)...'
    if (-not (Download "$DownloadBase/ken-mcp-$Target.exe" $mcpExe)) {
        Fail @(
            "the ken-mcp helper wasn't found in the latest release.",
            "Please download Ken by hand from $ReleasesPage"
        )
    }

    if ($env:KEN_INSTALL_PREFIX) {
        Write-Host 'KEN_INSTALL_PREFIX is set - skipping the app setup program (test mode).'
    } else {
        Write-Host 'Running the Ken setup program (silent, per-user)...'
        $proc = Start-Process -FilePath $setupExe -ArgumentList '/S' -Wait -PassThru
        if ($proc.ExitCode -ne 0) {
            Fail @(
                "the Ken setup program didn't finish (exit code $($proc.ExitCode)).",
                "You can run the installer yourself - download it from $ReleasesPage"
            )
        }
        Write-Host 'Installed the Ken app.'
    }

    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
    Copy-Item $mcpExe (Join-Path $BinDir 'ken-mcp.exe') -Force
    Write-Host "Installed ken-mcp to $BinDir\ken-mcp.exe"
} finally {
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
}

# --- Wrap up ------------------------------------------------------------

Write-Host ''
Write-Host 'All done! Find Ken in your Start menu.'

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $BinDir) {
    Write-Host ''
    Write-Host "One small thing: $BinDir isn't on your PATH yet, so your"
    Write-Host "terminal won't find ken-mcp. To fix it, run:"
    Write-Host "  [Environment]::SetEnvironmentVariable('Path', `"$BinDir;`" + [Environment]::GetEnvironmentVariable('Path', 'User'), 'User')"
    Write-Host 'then open a new terminal.'
}

Write-Host ''
if (Get-Command claude -ErrorAction SilentlyContinue) {
    Write-Host "Claude Code is installed - Ken's AI features are ready to go."
} else {
    Write-Host "One more thing: Ken's AI features (ingests, chat, deep research)"
    Write-Host 'use Claude Code, which isn''t installed yet. Everything else -'
    Write-Host 'indexing, search, editing - works without it. When you''re ready:'
    Write-Host '  1. Install it:  npm install -g @anthropic-ai/claude-code'
    Write-Host '  2. Log in once by running:  claude'
}
