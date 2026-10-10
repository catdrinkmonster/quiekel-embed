# Quiekel Embed installer for Windows. In PowerShell:
#
#   irm https://github.com/catdrinkmonster/quiekel-embed/raw/main/install.ps1 | iex
#
# Installs uv (which brings its own Python) and Git if they're missing, puts the app in
# %LOCALAPPDATA%\Programs\Quiekel Embed, adds Desktop and Start Menu shortcuts and starts it.
# Run it again to update. QUIEKEL_DIR installs somewhere else; QUIEKEL_REPO clones from elsewhere.

& {
    $ErrorActionPreference = "Stop"
    $repo = if ($env:QUIEKEL_REPO) { $env:QUIEKEL_REPO } else { "https://github.com/catdrinkmonster/quiekel-embed.git" }
    $dir = if ($env:QUIEKEL_DIR) { $env:QUIEKEL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\Quiekel Embed" }

    function Step([string]$text) { Write-Host "`n  $text" -ForegroundColor Magenta }
    function Have([string]$name) { [bool](Get-Command $name -ErrorAction SilentlyContinue) }
    function Run([string]$exe, [string[]]$arguments) {
        & $exe @arguments
        if ($LASTEXITCODE -ne 0) { throw "'$exe $($arguments -join ' ')' failed (exit code $LASTEXITCODE)." }
    }

    Write-Host "`n  Quiekel Embed: search your own files by meaning, right on your PC." -ForegroundColor White

    if (-not (Have "uv")) {
        Step "Installing uv (it brings its own Python)..."
        Run "powershell" @("-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", "irm https://astral.sh/uv/install.ps1 | iex")
        $env:Path = "$env:USERPROFILE\.local\bin;$env:Path"
        if (-not (Have "uv")) { throw "uv was installed but isn't on PATH yet: open a new PowerShell window and run this again." }
    }
    if (-not (Have "git")) {
        if (-not (Have "winget")) { throw "Git is missing: install it from https://git-scm.com, then run this again." }
        Step "Installing Git (Windows may ask for permission)..."
        Run "winget" @("install", "--id", "Git.Git", "--exact", "--source", "winget")
        $env:Path = "$env:ProgramFiles\Git\cmd;$env:Path"
    }

    if (Test-Path (Join-Path $dir ".git")) {
        Step "Updating $dir..."
        Run "git" @("-C", $dir, "pull", "--ff-only")
    } else {
        Step "Downloading Quiekel Embed to $dir..."
        Run "git" @("clone", $repo, $dir)
    }

    Step "Installing what it needs (about 3 GB to download, 5 GB on disk)..."
    Push-Location $dir
    try {
        Run "uv" @("sync", "--locked")  # exactly the locked, hash-checked versions, like in-app updates
        if (-not $env:QUIEKEL_NO_SHORTCUTS) {
            Step "Adding Desktop and Start Menu shortcuts..."
            Run "uv" @("run", "quiekel-embed-shortcuts")
        }
    } finally {
        Pop-Location
    }

    if (-not $env:QUIEKEL_NO_START) {
        Step "Starting Quiekel Embed. Its first start downloads the AI model (1.5 GB)."
        Start-Process -FilePath (Join-Path $dir ".venv\Scripts\quiekel-embed-app.exe") -WorkingDirectory $dir
    }
    Write-Host "`n  Done: Quiekel Embed is on your Desktop and in the Start Menu.`n" -ForegroundColor Green
}
