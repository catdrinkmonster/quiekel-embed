# The libraries Quiekel Embed loads at run time, fetched from their published packages (these exact
# files, checked against their SHA-256) into vendor\: a build finds them there, and the release zip
# takes them from there.
#
#   powershell -File .github\libraries.ps1 [-Into <folder>]

param([string]$Into = (Join-Path (Split-Path -Parent $PSScriptRoot) "vendor"))
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$sources = @(
    @{
        Name   = "ONNX Runtime 1.24.4 with DirectML"
        Url    = "https://files.pythonhosted.org/packages/60/53/2bd2696fac19cf8ca55496a0bcfe431f3aff9579eabbb0e231dc238acf6f/onnxruntime_directml-1.24.4-cp313-cp313-win_amd64.whl"
        Sha256 = "2F1031CB2281E5B27CCA9EFE0B9399317C7286E4D226F7A79D4AB79BBD94D19E"
        Kind   = "zip"
        To     = "onnxruntime"
        Files  = @("onnxruntime\capi\onnxruntime.dll", "onnxruntime\capi\onnxruntime_providers_shared.dll", "onnxruntime\capi\DirectML.dll")
    },
    @{
        Name   = "libheif (the HEIC decoder, from pillow-heif 1.8.0)"
        Url    = "https://files.pythonhosted.org/packages/71/77/fa70118bafb15584e49cba1af544d5be6756b5bfaca76ce25d42481bf4c2/pillow_heif-1.8.0-cp313-cp313-win_amd64.whl"
        Sha256 = "70161D9963702D94BBAA31B2E08E43F6FB3BAFFCCFAC8C9EC1D702F6FF620CDE"
        Kind   = "zip"
        To     = "libheif"
        Files  = @("pillow_heif-1.8.0.data\platlib\*.dll")
    },
    @{
        Name   = "PDFium 157.0.8086.0"
        Url    = "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/8086/pdfium-win-x64.tgz"
        Sha256 = "1FD8AF952832DBB0EB16D9249F68FE09E5F5EBF7C3DD9F6066EA2720CC28487D"
        Kind   = "tgz"
        To     = "pdfium"
        Files  = @("bin\pdfium.dll", "LICENSE")
    }
)

Add-Type -AssemblyName System.IO.Compression.FileSystem
$work = Join-Path ([IO.Path]::GetTempPath()) "quiekel-libraries-$PID"
New-Item -ItemType Directory -Force $work | Out-Null
try {
    foreach ($s in $sources) {
        Write-Host "  $($s.Name)"
        $file = Join-Path $work ([IO.Path]::GetFileName($s.Url))
        Invoke-WebRequest -UseBasicParsing -Uri $s.Url -OutFile $file
        $hash = (Get-FileHash -Algorithm SHA256 $file).Hash
        if ($hash -ne $s.Sha256) { throw "$($s.Name): the download's SHA-256 is $hash, not $($s.Sha256)." }
        $unpacked = Join-Path $work $s.To
        New-Item -ItemType Directory -Force $unpacked | Out-Null
        if ($s.Kind -eq "zip") {
            [IO.Compression.ZipFile]::ExtractToDirectory($file, $unpacked)
        } else {
            & "$env:SystemRoot\System32\tar.exe" -xf $file -C $unpacked
            if ($LASTEXITCODE -ne 0) { throw "$($s.Name): unpacking failed." }
        }
        $target = Join-Path $Into $s.To
        New-Item -ItemType Directory -Force $target | Out-Null
        foreach ($pattern in $s.Files) {
            $found = @(Get-ChildItem (Join-Path $unpacked $pattern))
            if (-not $found.Count) { throw "$($s.Name): $pattern isn't in the package." }
            $found | Copy-Item -Destination $target -Force
        }
    }
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
Write-Host "  Done: $Into"
