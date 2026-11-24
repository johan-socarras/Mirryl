# Installs Mirryl on Windows from the latest release, for the current user.
# In PowerShell:
#
#   irm https://raw.githubusercontent.com/johan-socarras/Mirryl/main/install.ps1 | iex
#
# It downloads Mirryl_<version>_x64-setup.exe from GitHub, checks it against
# the SHA-256 GitHub publishes for it, and runs it without questions. On the
# first install Windows asks once for permission to add the firewall rules
# and the reconnect task. Running it again updates Mirryl in place; an open
# Mirryl is closed first. Optional:
#
#   $env:MIRRYL_VERSION = 'v1.0.12'   install that release instead of the latest
#   $env:GITHUB_TOKEN = '...'         for a private fork, or to avoid rate limits
#
# Works in Windows PowerShell 5.1 and PowerShell 7.

& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is much faster without it
    $repo = 'johan-socarras/Mirryl'

    function Stop-Install($message) {
        Write-Host $message -ForegroundColor Red
        throw 'Mirryl was not installed.'
    }

    if ([Environment]::OSVersion.Platform -ne 'Win32NT') {
        Stop-Install 'This installer is for Windows. On Linux, use install.sh: see the README.'
    }
    if (-not [Environment]::Is64BitOperatingSystem) {
        Stop-Install 'Mirryl needs 64-bit Windows 10 or 11.'
    }
    # Windows PowerShell 5.1 may still offer only older TLS versions.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $headers = @{ 'User-Agent' = 'mirryl-install'; 'Accept' = 'application/vnd.github+json' }
    if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "Bearer $env:GITHUB_TOKEN" }
    $which = if ($env:MIRRYL_VERSION) { "tags/$env:MIRRYL_VERSION" } else { 'latest' }
    try {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/$which" -Headers $headers
    } catch {
        Stop-Install "Could not find the release '$which' of $repo on GitHub: $($_.Exception.Message)"
    }
    $asset = $release.assets | Where-Object { $_.name -match '^Mirryl_.+_x64-setup\.exe$' } | Select-Object -First 1
    if (-not $asset) { Stop-Install "The release $($release.tag_name) has no Windows installer." }

    $folder = Join-Path ([IO.Path]::GetTempPath()) ("mirryl-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory $folder | Out-Null
    try {
        $setup = Join-Path $folder $asset.name
        Write-Host "Downloading $($asset.name) ($([Math]::Round($asset.size / 1MB, 1)) MB)..."
        if ($env:GITHUB_TOKEN) {
            # A private repository only serves its files through the API.
            $download = @{ 'User-Agent' = 'mirryl-install'; 'Accept' = 'application/octet-stream'; 'Authorization' = "Bearer $env:GITHUB_TOKEN" }
            Invoke-WebRequest -Uri $asset.url -Headers $download -OutFile $setup -UseBasicParsing
        } else {
            Invoke-WebRequest -Uri $asset.browser_download_url -Headers @{ 'User-Agent' = 'mirryl-install' } -OutFile $setup -UseBasicParsing
        }

        $size = (Get-Item $setup).Length
        if ($size -ne $asset.size) { Stop-Install "The download is incomplete: $size bytes of $($asset.size)." }
        if ($asset.digest -match '^sha256:([0-9a-f]{64})$') {
            $hash = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($hash -ne $Matches[1]) { Stop-Install 'The download does not match the SHA-256 GitHub publishes for it.' }
            Write-Host 'Checked against the SHA-256 published on GitHub.'
        } else {
            Write-Host 'GitHub publishes no SHA-256 for this file; checked its size only.' -ForegroundColor Yellow
        }

        # /P is the installer's passive mode: a progress bar and no questions.
        Write-Host "Installing Mirryl $($release.tag_name) for $env:USERNAME..."
        $process = Start-Process $setup -ArgumentList '/P' -Wait -PassThru
        if ($process.ExitCode -ne 0) { Stop-Install "The installer stopped with code $($process.ExitCode)." }
    } finally {
        Remove-Item $folder -Recurse -Force -ErrorAction SilentlyContinue
    }

    $key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Mirryl'
    $installed = Get-ItemProperty $key -ErrorAction SilentlyContinue
    if (-not $installed) { Stop-Install 'The installer finished, but Windows does not list Mirryl as installed.' }
    Write-Host "Mirryl $($installed.DisplayVersion) is installed in $($installed.InstallLocation.Trim('"'))." -ForegroundColor Green
    Write-Host 'Open it from the Start menu or the desktop shortcut.'
}
