# Quiekel Embed installer for Windows. In PowerShell:
#
#   irm https://github.com/catdrinkmonster/quiekel-embed/raw/main/install.ps1 | iex
#
# Installs uv (which brings its own Python) if it's missing, and if Git is missing a private copy
# of it (MinGit) in the app's folder; puts the app in %LOCALAPPDATA%\Programs\Quiekel Embed, adds
# Desktop and Start Menu shortcuts and starts it. Run it again to update. It needs no administrator
# rights: run it as yourself, in a normal PowerShell window. QUIEKEL_DIR installs somewhere else;
# QUIEKEL_REPO clones from elsewhere.

& {
    $ErrorActionPreference = "Stop"
    $ProgressPreference = "SilentlyContinue"  # (Windows PowerShell downloads crawl with a progress bar)
    $repo = if ($env:QUIEKEL_REPO) { $env:QUIEKEL_REPO } else { "https://github.com/catdrinkmonster/quiekel-embed.git" }
    $dir = if ($env:QUIEKEL_DIR) { $env:QUIEKEL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\Quiekel Embed" }
    # MinGit: the official minimal Git for Windows, this exact file.
    $minGitUrl = "https://github.com/git-for-windows/git/releases/download/v2.56.0.windows.2/MinGit-2.56.0.2-64-bit.zip"
    $minGitSha256 = "DA35E72AA21C005A5A0D298CFBAE110BC1609A815730EA0DDE84B01A1B3CD3BE"

    function Step([string]$text) { Write-Host "`n  $text" -ForegroundColor Magenta }
    function Have([string]$name) { [bool](Get-Command $name -ErrorAction SilentlyContinue) }
    function Run([string]$exe, [string[]]$arguments) {
        & $exe @arguments
        if ($LASTEXITCODE -ne 0) { throw "'$exe $($arguments -join ' ')' failed (exit code $LASTEXITCODE)." }
    }

    Write-Host "`n  Quiekel Embed: search your own files by meaning, right on your PC." -ForegroundColor White

    # Everything goes into your own user folder, so it has to run as you: an administrator window
    # signed in as another account would install it for that account.
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $elevated = ([Security.Principal.WindowsPrincipal]$identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    $signedIn = (Get-CimInstance Win32_ComputerSystem -ErrorAction SilentlyContinue).UserName
    if ($elevated -and $signedIn -and $signedIn -ne $identity.Name) {
        throw "This window runs as $($identity.Name), but $signedIn is signed in. Open PowerShell normally (not as administrator) and run the line again."
    }

    if (-not (Have "uv")) {
        Step "Installing uv (it brings its own Python)..."
        Run "powershell" @("-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", "irm https://astral.sh/uv/install.ps1 | iex")
        $env:Path = "$env:USERPROFILE\.local\bin;$env:Path"
        if (-not (Have "uv")) { throw "uv was installed but isn't on PATH yet: open a new PowerShell window and run this again." }
    }

    # Git fetches the app and its updates. Without one, the app keeps its own in its folder: no
    # installer, no administrator rights, nothing else on the PC changes.
    $privateGit = Join-Path $dir ".mingit\cmd\git.exe"
    $git = if (Test-Path $privateGit) { $privateGit } elseif (Have "git") { "git" } else { $null }
    $unpacked = $null
    try {
        if (-not $git) {
            Step "Downloading Git for the app (about 40 MB)..."
            $unpacked = Join-Path ([IO.Path]::GetTempPath()) "quiekel-mingit-$PID"
            $zip = "$unpacked.zip"
            Invoke-WebRequest -UseBasicParsing -Uri $minGitUrl -OutFile $zip
            if ((Get-FileHash -Algorithm SHA256 $zip).Hash -ne $minGitSha256) {
                Remove-Item $zip -Force
                throw "The Git download didn't match its checksum: run the line again."
            }
            Add-Type -AssemblyName System.IO.Compression.FileSystem
            [IO.Compression.ZipFile]::ExtractToDirectory($zip, $unpacked)
            Remove-Item $zip -Force
            $git = Join-Path $unpacked "cmd\git.exe"
        }
        if (Test-Path (Join-Path $dir ".git")) {
            # The folder holds only the app (the index and settings are elsewhere): it takes main as
            # it is, also when the repository's history was started anew.
            Step "Updating $dir..."
            Run $git @("-C", $dir, "fetch", "--quiet", "origin", "main")
            Run $git @("-C", $dir, "checkout", "--quiet", "--force", "-B", "main", "origin/main")
        } else {
            Step "Downloading Quiekel Embed to $dir..."
            Run $git @("clone", $repo, $dir)
        }
        if ($unpacked) { Move-Item $unpacked (Join-Path $dir ".mingit") }  # kept for updates
    } finally {
        if ($unpacked -and (Test-Path $unpacked)) { Remove-Item -Recurse -Force $unpacked }
    }

    # One AI engine for every PC: ONNX Runtime, on the graphics card through DirectML or on the
    # processor. (Copies from before 0.12 noted a choice of engine here.)
    Remove-Item (Join-Path $dir "engine.txt") -ErrorAction SilentlyContinue
    Step "Installing what it needs (about 300 MB to download, 1 GB on disk)..."
    Push-Location $dir
    try {
        Run "uv" @("sync", "--locked")  # exactly the locked, hash-checked versions, like in-app updates
        if (-not $env:QUIEKEL_NO_SHORTCUTS) {
            Step "Adding Desktop and Start Menu shortcuts..."
            Run "uv" @("run", "--no-sync", "quiekel-embed-shortcuts")
        }
    } finally {
        Pop-Location
    }

    if (-not $env:QUIEKEL_NO_START) {
        Step "Starting Quiekel Embed. Its first start downloads the AI model (0.5 to 0.9 GB)."
        $app = Join-Path $dir ".venv\Scripts\quiekel-embed-app.exe"
        if ($elevated) {
            Start-Process explorer.exe -ArgumentList "`"$app`""  # as you, without administrator rights
        } else {
            Start-Process -FilePath $app -WorkingDirectory $dir
        }
    }
    Write-Host "`n  Done: Quiekel Embed is open, and on your Desktop and in the Start Menu.`n" -ForegroundColor Green
}
