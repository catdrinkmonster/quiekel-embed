# The release zip, Quiekel-Embed-<version>-windows-x64.zip: the app and the libraries it loads side
# by side, their licenses in licenses\. After .github\libraries.ps1 and `cargo build --release --locked`.
#
#   powershell -File .github\release.ps1 -Version <the version in Cargo.toml>

param([Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root "dist"
$zip = Join-Path $root "Quiekel-Embed-$Version-windows-x64.zip"
$exe = Join-Path $root "target\release\quiekel-embed.exe"

$cargo = (Select-String -LiteralPath (Join-Path $root "Cargo.toml") -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if ($cargo -ne $Version) { throw "Cargo.toml says $cargo, not $Version." }
if ((Get-Item $exe).VersionInfo.ProductVersion -ne $Version) { throw "$exe isn't version ${Version}: build it first." }

if (Test-Path -LiteralPath $dist) { Remove-Item -LiteralPath $dist -Recurse -Force }
New-Item -ItemType Directory $dist | Out-Null
Copy-Item $exe $dist
Get-ChildItem (Join-Path $root "vendor") -Recurse -Filter *.dll | Copy-Item -Destination $dist
Copy-Item (Join-Path $root "vendor\licenses") $dist -Recurse
Copy-Item (Join-Path $root "LICENSE") (Join-Path $dist "licenses\quiekel-embed.txt")

# The crates compiled into the exe (cargo tree: the dependencies built for it, not those of build
# scripts or tests) and the license texts their packages carry: each text once, under the list of
# crates that carry it.
$inExe = @{}
foreach ($line in (cargo tree --locked -e normal --target x86_64-pc-windows-msvc --prefix none -f "{p}")) {
    if ($line -match '^(\S+) v(\S+)') { $inExe["$($Matches[1]) $($Matches[2])"] = $true }
}
if ($LASTEXITCODE) { throw "cargo tree failed." }
$meta = (cargo metadata --format-version 1 --locked --filter-platform x86_64-pc-windows-msvc | Out-String) | ConvertFrom-Json
if ($LASTEXITCODE) { throw "cargo metadata failed." }
$texts = [Collections.Generic.Dictionary[string, object]]::new([StringComparer]::Ordinal)  # (by the text without its spacing)
$bare = @()
foreach ($p in @($meta.packages | Where-Object { $_.source -and $inExe.ContainsKey("$($_.name) $($_.version)") } | Sort-Object name, version)) {
    $crate = "$($p.name) $($p.version)"
    $dir = Split-Path -Parent $p.manifest_path
    $files = @(Get-ChildItem -LiteralPath $dir -File | Where-Object { $_.Name -match '^(licen[cs]e|copying|notice|unlicense|copyright)' })
    if ($p.license_file -and (Test-Path -LiteralPath (Join-Path $dir $p.license_file))) { $files += Get-Item -LiteralPath (Join-Path $dir $p.license_file) }
    if (-not $files.Count) {
        $bare += "  $crate ($($p.license))$(if ($p.repository) { ": $($p.repository)" })"
        continue
    }
    foreach ($f in $files) {
        $text = [IO.File]::ReadAllText($f.FullName).Trim() -replace "\r\n?", "`n"
        $key = $text -replace '\s+', ' '
        if (-not $texts.ContainsKey($key)) { $texts[$key] = @{ Text = $text; Crates = [Collections.Generic.List[string]]::new() } }
        if (-not $texts[$key].Crates.Contains($crate)) { $texts[$key].Crates.Add($crate) }
    }
}
$out = [Text.StringBuilder]::new()
[void]$out.Append("The crates compiled into quiekel-embed.exe and the license texts they come with.`n")
foreach ($t in @($texts.Values | Sort-Object { $_.Crates[0] })) {
    $names = @(); $line = ""
    foreach ($c in $t.Crates) {
        if ($line -and $line.Length + $c.Length + 2 -gt 100) { $names += "$line,"; $line = $c } elseif ($line) { $line += ", $c" } else { $line = $c }
    }
    [void]$out.Append("`n$("=" * 100)`n$(($names + $line) -join "`n")`n$("-" * 100)`n$($t.Text)`n")
}
if ($bare.Count) {
    [void]$out.Append("`n$("=" * 100)`nCrates whose packages carry no license text (license, source):`n$($bare -join "`n")`n")
}
[IO.File]::WriteAllText((Join-Path $dist "licenses\crates.txt"), $out.ToString().Replace("`n", "`r`n"))

# (forward slashes in the zip, whichever PowerShell makes it)
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip }
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$archive = [IO.Compression.ZipFile]::Open($zip, [IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($f in Get-ChildItem -LiteralPath $dist -Recurse -File) {
        $name = $f.FullName.Substring($dist.Length + 1).Replace("\", "/")
        [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $f.FullName, $name, [IO.Compression.CompressionLevel]::Optimal)
    }
} finally {
    $archive.Dispose()
}
Write-Host "  $zip ($([math]::Round((Get-Item $zip).Length / 1MB, 1)) MB, SHA-256 $((Get-FileHash -Algorithm SHA256 $zip).Hash))"
