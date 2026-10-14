# Quiekel Embed installer for Windows. In PowerShell:
#
#   irm https://github.com/catdrinkmonster/quiekel-embed/raw/main/install.ps1 | iex
#
# Downloads the latest release (one zip: the app and the libraries it loads), checks it against the
# SHA-256 GitHub lists for it, puts it in %LOCALAPPDATA%\Programs\Quiekel Embed, adds Desktop and
# Start Menu shortcuts and starts it. Run it again to update (the app also updates itself). It needs
# no administrator rights: run it as yourself, in a normal PowerShell window. QUIEKEL_DIR installs
# somewhere else; QUIEKEL_RELEASES lists releases from elsewhere (the same format as GitHub's API).

& {
    $ErrorActionPreference = "Stop"
    $ProgressPreference = "SilentlyContinue"  # (Windows PowerShell downloads crawl with a progress bar)
    $api = if ($env:QUIEKEL_RELEASES) { $env:QUIEKEL_RELEASES } else { "https://api.github.com/repos/catdrinkmonster/quiekel-embed/releases?per_page=30" }
    $dir = if ($env:QUIEKEL_DIR) { $env:QUIEKEL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\Quiekel Embed" }
    $exe = Join-Path $dir "quiekel-embed.exe"

    function Step([string]$text) { Write-Host "`n  $text" -ForegroundColor Magenta }

    Write-Host "`n  Quiekel Embed: search your own files by meaning, right on your PC." -ForegroundColor White

    # Everything goes into your own user folder, so it has to run as you: an administrator window
    # signed in as another account would install it for that account.
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $elevated = ([Security.Principal.WindowsPrincipal]$identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    $signedIn = (Get-CimInstance Win32_ComputerSystem -ErrorAction SilentlyContinue).UserName
    if ($elevated -and $signedIn -and $signedIn -ne $identity.Name) {
        throw "This window runs as $($identity.Name), but $signedIn is signed in. Open PowerShell normally (not as administrator) and run the line again."
    }

    Step "Looking for the latest release..."
    $headers = @{ "Accept" = "application/vnd.github+json"; "User-Agent" = "quiekel-embed-installer" }
    $releases = @(Invoke-RestMethod -Uri $api -Headers $headers)
    $zipOf = { param($r) @($r.assets) | Where-Object { $_.name -like "*-windows-x64.zip" -and "$($_.digest)" -like "sha256:*" } | Select-Object -First 1 }
    $release = $releases |
        Where-Object { -not $_.draft -and -not $_.prerelease -and $_.tag_name -match '^v\d+\.\d+\.\d+$' -and (& $zipOf $_) } |
        Sort-Object { [version]$_.tag_name.Substring(1) } -Descending |
        Select-Object -First 1
    if (-not $release) { throw "There's no release to install yet." }
    $asset = & $zipOf $release
    $sha256 = $asset.digest.Substring(7).ToUpper()

    $temp = Join-Path ([IO.Path]::GetTempPath()) "quiekel-embed-$PID"
    $zip = "$temp.zip"
    try {
        Step "Downloading Quiekel Embed $($release.tag_name) ($([math]::Ceiling($asset.size / 1MB)) MB)..."
        Invoke-WebRequest -UseBasicParsing -Uri $asset.browser_download_url -OutFile $zip
        if ((Get-FileHash -Algorithm SHA256 $zip).Hash -ne $sha256) {
            throw "The download didn't match its checksum: run the line again."
        }
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        [IO.Compression.ZipFile]::ExtractToDirectory($zip, $temp)
        if (-not (Test-Path (Join-Path $temp "quiekel-embed.exe"))) { throw "The release has no quiekel-embed.exe." }

        # A running copy (the app and its model process, also an older version's) lets go first.
        $running = @(Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path -like "$dir\*" })
        if ($running.Count) {
            Step "Closing the running Quiekel Embed..."
            $running | Stop-Process -Force
            $running | Wait-Process -Timeout 15 -ErrorAction SilentlyContinue
            Start-Sleep -Milliseconds 800  # (Windows lets go of the files a moment later)
        }
        # The folder holds only the program (the index and settings are in %LOCALAPPDATA%\QuiekelEmbed):
        # what was there goes, an older version's too (also one that ran on Python).
        Step "Installing to $dir..."
        if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
        New-Item -ItemType Directory -Force $dir | Out-Null
        Get-ChildItem $temp | Copy-Item -Destination $dir -Recurse -Force
    } finally {
        Remove-Item -Force $zip -ErrorAction SilentlyContinue
        Remove-Item -Recurse -Force $temp -ErrorAction SilentlyContinue
    }

    if (-not $env:QUIEKEL_NO_SHORTCUTS) {
        Step "Adding Desktop and Start Menu shortcuts..."
        & $exe --shortcuts | Out-Null
        # Started with Windows before: still, now with this program.
        $startup = Join-Path ([Environment]::GetFolderPath("Startup")) "Quiekel Embed.lnk"
        if (Test-Path $startup) {
            $link = (New-Object -ComObject WScript.Shell).CreateShortcut($startup)
            $link.TargetPath = $exe; $link.Arguments = "--hidden"; $link.WorkingDirectory = $dir; $link.Save()
        }
    }

    if (-not $env:QUIEKEL_NO_START) {
        Step "Starting Quiekel Embed. Its first start downloads the AI model (0.5 to 0.9 GB)."
        if ($elevated) {
            Start-Process explorer.exe -ArgumentList "`"$exe`""  # as you, without administrator rights
        } else {
            Start-Process -FilePath $exe -WorkingDirectory $dir
        }
    }
    Write-Host "`n  Done: Quiekel Embed $($release.tag_name) is open, and on your Desktop and in the Start Menu.`n" -ForegroundColor Green
}
