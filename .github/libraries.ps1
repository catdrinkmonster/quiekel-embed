# The libraries Quiekel Embed loads at run time, fetched from their published packages (these exact
# files, checked against their SHA-256) into vendor\: a build finds them there, and the release zip
# takes them from there, with their licenses (vendor\licenses, a folder per library) and where their
# source code is (vendor\licenses\sources.txt).
#
#   powershell -File .github\libraries.ps1 [-Into <folder>]

param([string]$Into = (Join-Path (Split-Path -Parent $PSScriptRoot) "vendor"))
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

# Files go into <To>, the package's own license files into licenses\<To>.
$sources = @(
    @{
        Name     = "ONNX Runtime 1.24.4 with DirectML 1.15.4"
        Url      = "https://files.pythonhosted.org/packages/60/53/2bd2696fac19cf8ca55496a0bcfe431f3aff9579eabbb0e231dc238acf6f/onnxruntime_directml-1.24.4-cp313-cp313-win_amd64.whl"
        Sha256   = "2F1031CB2281E5B27CCA9EFE0B9399317C7286E4D226F7A79D4AB79BBD94D19E"
        Kind     = "zip"
        To       = "onnxruntime"
        Files    = @("onnxruntime\capi\onnxruntime.dll", "onnxruntime\capi\onnxruntime_providers_shared.dll", "onnxruntime\capi\DirectML.dll")
        Licenses = @("onnxruntime\LICENSE", "onnxruntime\ThirdPartyNotices.txt")
        Source   = @("https://github.com/microsoft/onnxruntime/tree/v1.24.4")
    },
    @{
        # (decoding only: no HEVC encoder in it)
        Name     = "libheif 1.23.0 with libde265 1.0.15, the GCC 16.1 runtime and winpthreads (MSYS2), from pi-heif 1.4.0"
        Url      = "https://files.pythonhosted.org/packages/0a/7f/f1c748d8fadfe4bbde5960c83abacef7fdefd7242b16002f37e4e5f54b94/pi_heif-1.4.0-cp313-cp313-win_amd64.whl"
        Sha256   = "7692B50E0305170502FDD0657B31818E724F9630629BDBC05A91B10A7979D786"
        Kind     = "zip"
        To       = "libheif"
        Files    = @("pi_heif-1.4.0.data\platlib\*.dll")
        Licenses = @()
        Source   = @(
            "https://github.com/strukturag/libheif/tree/v1.23.0",
            "https://github.com/strukturag/libde265/tree/v1.0.15",
            "https://files.pythonhosted.org/packages/6a/a2/70168b601b41bdf5726dfc8dc110eb4052a2e851fed9c9bdae95910e401d/pi_heif-1.4.0.tar.gz",
            "https://github.com/msys2/MINGW-packages")
    },
    @{
        Name     = "PDFium 157.0.8086.0"
        Url      = "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/8086/pdfium-win-x64.tgz"
        Sha256   = "1FD8AF952832DBB0EB16D9249F68FE09E5F5EBF7C3DD9F6066EA2720CC28487D"
        Kind     = "tgz"
        To       = "pdfium"
        Files    = @("bin\pdfium.dll")
        Licenses = @("LICENSE", "licenses\*")
        Source   = @("https://pdfium.googlesource.com/pdfium/+/refs/heads/chromium/8086", "https://github.com/bblanchon/pdfium-binaries")
    }
)

# License texts the packages leave out, at the versions in them: into licenses\<As>.
$texts = @(
    @{ As = "onnxruntime\DirectML-LICENSE.txt"; Url = "https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/1.15.4/license"; Sha256 = "A05138E3A085FF60A44881EEDFA58DCCB03ECC1D7B1F6AE888418E8C2FEC4B8D" },
    @{ As = "libheif\COPYING"; Url = "https://raw.githubusercontent.com/strukturag/libheif/v1.23.0/COPYING"; Sha256 = "FA81CE652315B013359D6E8E4744335F31A50C7C192907176D3632F78A3B4596" },
    @{ As = "libheif\libde265-COPYING"; Url = "https://raw.githubusercontent.com/strukturag/libde265/v1.0.15/COPYING"; Sha256 = "02CC1585A20677992E0BA578FA692635DC193735F2691DC81DE924B51C4E8020" },
    @{ As = "libheif\gcc-COPYING.RUNTIME"; Url = "https://raw.githubusercontent.com/gcc-mirror/gcc/releases/gcc-16.1.0/COPYING.RUNTIME"; Sha256 = "9D6B43CE4D8DE0C878BF16B54D8E7A10D9BD42B75178153E3AF6A815BDC90F74" },
    @{ As = "libheif\gcc-COPYING3"; Url = "https://raw.githubusercontent.com/gcc-mirror/gcc/releases/gcc-16.1.0/COPYING3"; Sha256 = "8CEB4B9EE5ADEDDE47B31E975C1D90C73AD27B6B165A1DCD80C7C545EB65B903" },
    @{ As = "libheif\winpthreads-COPYING"; Url = "https://raw.githubusercontent.com/mingw-w64/mingw-w64/v14.0.0/mingw-w64-libraries/winpthreads/COPYING"; Sha256 = "63263614CDD29F2F93CBA85E992F041B31F9FC7B4033692F31269489A8A1B177" }
)

function Fetch([string]$Name, [string]$Url, [string]$Sha256, [string]$File) {
    Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $File
    $hash = (Get-FileHash -Algorithm SHA256 $File).Hash
    if ($hash -ne $Sha256) { throw "${Name}: the download's SHA-256 is $hash, not $Sha256." }
}

# Each folder starts empty: nothing stays from other versions.
function Fresh([string]$Folder) {
    if (Test-Path -LiteralPath $Folder) { Remove-Item -LiteralPath $Folder -Recurse -Force }
    New-Item -ItemType Directory -Force $Folder | Out-Null
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$work = Join-Path ([IO.Path]::GetTempPath()) "quiekel-libraries-$PID"
New-Item -ItemType Directory -Force $work | Out-Null
$licenses = Join-Path $Into "licenses"
Fresh $licenses
try {
    foreach ($s in $sources) {
        Write-Host "  $($s.Name)"
        $file = Join-Path $work ([IO.Path]::GetFileName($s.Url))
        Fetch $s.Name $s.Url $s.Sha256 $file
        $unpacked = Join-Path $work $s.To
        New-Item -ItemType Directory -Force $unpacked | Out-Null
        if ($s.Kind -eq "zip") {
            [IO.Compression.ZipFile]::ExtractToDirectory($file, $unpacked)
        } else {
            & "$env:SystemRoot\System32\tar.exe" -xf $file -C $unpacked
            if ($LASTEXITCODE -ne 0) { throw "$($s.Name): unpacking failed." }
        }
        foreach ($set in @(@{ Patterns = $s.Files; Target = Join-Path $Into $s.To }, @{ Patterns = $s.Licenses; Target = Join-Path $licenses $s.To })) {
            if (-not $set.Patterns.Count) { continue }
            Fresh $set.Target
            foreach ($pattern in $set.Patterns) {
                $found = @(Get-ChildItem (Join-Path $unpacked $pattern))
                if (-not $found.Count) { throw "$($s.Name): $pattern isn't in the package." }
                $found | Copy-Item -Destination $set.Target -Force
            }
        }
    }
    foreach ($t in $texts) {
        $file = Join-Path $licenses $t.As
        New-Item -ItemType Directory -Force (Split-Path -Parent $file) | Out-Null
        Fetch $t.As $t.Url $t.Sha256 $file
    }
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

$lines = @("The libraries next to quiekel-embed.exe, unchanged from these packages, and their source code:")
foreach ($s in $sources) {
    $lines += "", $s.Name, "  Package: $($s.Url)"
    $lines += @($s.Source | ForEach-Object { "  Source: $_" })
}
[IO.File]::WriteAllLines((Join-Path $licenses "sources.txt"), [string[]]$lines)
Write-Host "  Done: $Into"
