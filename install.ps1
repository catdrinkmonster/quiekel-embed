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

        # A running copy lets go first: the app and its model process, also an older version's. (One
        # that ran on Python runs from launchers in this folder; the interpreters they start, from
        # elsewhere, end with them, or are found by their command line.)
        $inDir = { param($text) "$text".IndexOf("$dir\", [StringComparison]::OrdinalIgnoreCase) -ge 0 }
        $running = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {
            $_.ProcessId -ne $PID -and ((& $inDir $_.ExecutablePath) -or ($_.Name -match '^pythonw?\.exe$' -and (& $inDir $_.CommandLine)))
        })
        if ($running.Count) {
            Step "Closing the running Quiekel Embed..."
            $running | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
            $running | ForEach-Object { Wait-Process -Id $_.ProcessId -Timeout 15 -ErrorAction SilentlyContinue }
        }
        # The folder holds only the program (the index and settings are in %LOCALAPPDATA%\QuiekelEmbed):
        # what was there goes, an older version's too (also one that ran on Python). Windows may hold
        # a file a moment after its program has closed: tried again.
        Step "Installing to $dir..."
        for ($try = 1; Test-Path -LiteralPath $dir; $try++) {
            Remove-Item -LiteralPath $dir -Recurse -Force -ErrorAction SilentlyContinue
            if (-not (Test-Path -LiteralPath $dir)) { break }
            if ($try -ge 10) { throw "Couldn't replace ${dir}: a program still has a file in it open. Close it and run the line again." }
            Start-Sleep -Seconds 1
        }
        New-Item -ItemType Directory -Force $dir | Out-Null
        Get-ChildItem $temp | Copy-Item -Destination $dir -Recurse -Force
    } finally {
        Remove-Item -Force $zip -ErrorAction SilentlyContinue
        Remove-Item -Recurse -Force $temp -ErrorAction SilentlyContinue
    }

    if (-not $env:QUIEKEL_NO_SHORTCUTS) {
        # (Started with Windows or pinned to the taskbar before, also by an older version: still,
        # now with this program.)
        Step "Adding Desktop and Start Menu shortcuts..."
        & $exe --shortcuts | Out-Null
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
