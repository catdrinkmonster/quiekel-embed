# install.ps1 in Windows PowerShell and in PowerShell 7, against a list of releases in GitHub's
# format served on 127.0.0.1: both have to install v1.10.0 and pass over a draft, a prerelease, a
# tag that isn't vX.Y.Z, a zip without a checksum and v1.5.0 (their zips aren't there to download).
# Into a temp folder, with no shortcuts and nothing started.
#
#   powershell -File .github\install-test.ps1

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$installer = Join-Path (Split-Path -Parent $PSScriptRoot) "install.ps1"
$work = Join-Path $(if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }) "quiekel-install-test-$PID"
$served = Join-Path $work "served"
$packed = Join-Path $work "zip"
$server = $null
New-Item -ItemType Directory -Force $served, $packed | Out-Null
try {
    # The release to install: a zip with a stand-in quiekel-embed.exe that only names its version.
    $pick = "v1.10.0"
    $text = "Quiekel Embed $pick"
    [IO.File]::WriteAllText((Join-Path $packed "quiekel-embed.exe"), $text)
    $zip = Join-Path $served "Quiekel-Embed-$($pick.Substring(1))-windows-x64.zip"
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::CreateFromDirectory($packed, $zip)
    $size = (Get-Item $zip).Length
    $sha256 = "sha256:$((Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower())"

    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)  # (a free port)
    $listener.Start(); $port = $listener.LocalEndpoint.Port; $listener.Stop()
    $base = "http://127.0.0.1:$port"

    # What install.ps1 reads of GitHub's list, newest first. Each zip's address is in the served
    # folder, but only v1.10.0's is there.
    function Release($Tag, $Digest = "sha256:$("0" * 64)", [switch]$Draft, [switch]$Prerelease) {
        $name = "Quiekel-Embed-$($Tag.Substring(1))-windows-x64.zip"
        [ordered]@{
            tag_name   = $Tag
            draft      = $Draft.IsPresent
            prerelease = $Prerelease.IsPresent
            assets     = @([ordered]@{ name = $name; size = $size; digest = $Digest; browser_download_url = "$base/$name" })
        }
    }
    $releases = @(
        (Release "v3.0.0" -Draft),
        (Release "v2.2.0" -Prerelease),
        (Release "v2.1.0-beta"),
        (Release "v2.0.0" -Digest $null),
        (Release "v1.5.0"),  # (above v1.10.0 as text)
        (Release $pick -Digest $sha256)
    )
    [IO.File]::WriteAllText((Join-Path $served "releases.json"), (ConvertTo-Json -InputObject $releases -Depth 5))

    $log = Join-Path $work "server.log"
    $server = Start-Process python -ArgumentList "-m http.server $port --bind 127.0.0.1" -WorkingDirectory $served `
        -NoNewWindow -PassThru -RedirectStandardOutput (Join-Path $work "server.out") -RedirectStandardError $log
    for ($try = 1; ; $try++) {
        try { Invoke-WebRequest -UseBasicParsing -Uri "$base/releases.json" | Out-Null; break } catch {
            if ($server.HasExited -or $try -ge 50) { throw "The list isn't served: $(Get-Content -Raw $log)" }
            Start-Sleep -Milliseconds 200
        }
    }

    $env:QUIEKEL_RELEASES = "$base/releases.json"
    $env:QUIEKEL_NO_SHORTCUTS = "1"
    $env:QUIEKEL_NO_START = "1"
    foreach ($shell in "powershell", "pwsh") {
        $version = & $shell -NoProfile -Command '$PSVersionTable.PSVersion.ToString()'
        Write-Host "`n  install.ps1 in $shell $version"
        $env:QUIEKEL_DIR = Join-Path $work "installed-$shell"
        & $shell -NoProfile -ExecutionPolicy Bypass -File $installer
        if ($LASTEXITCODE) { throw "install.ps1 failed in $shell $version." }
        $installed = [IO.File]::ReadAllText((Join-Path $env:QUIEKEL_DIR "quiekel-embed.exe"))
        if ($installed -ne $text) { throw "$shell $version installed `"$installed`", not $pick." }
    }
} finally {
    if ($server) { Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue; [void]$server.WaitForExit(10000) }
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
Write-Host "`n  Done: both installed $pick."
