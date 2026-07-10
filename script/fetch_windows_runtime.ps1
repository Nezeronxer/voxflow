[CmdletBinding()]
param(
  [string]$RuntimeRoot = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($RuntimeRoot)) {
  $RuntimeRoot = Join-Path $repoRoot "voxflow/src-tauri/resources"
}

# GitHub release asset digests for whisper.cpp v1.8.6 and a commit-pinned
# Silero VAD model. Update URL and SHA256 together after compatibility testing.
$cpuSource = @{
  Url = "https://github.com/ggml-org/whisper.cpp/releases/download/v1.8.6/whisper-bin-x64.zip"
  Sha256 = "b07ea0b1b4115a38e1a7b07debf581f0b77d999925f8acb8f39d322b0ba0a822"
  File = "whisper-bin-x64.zip"
}
$cudaSource = @{
  Url = "https://github.com/ggml-org/whisper.cpp/releases/download/v1.8.6/whisper-cublas-12.4.0-bin-x64.zip"
  Sha256 = "63b70c91fe2fd7449865c45f6422ab628439eacc6985d8309c77bfb65cc68a19"
  File = "whisper-cublas-12.4.0-bin-x64.zip"
}
$vadSource = @{
  Url = "https://raw.githubusercontent.com/snakers4/silero-vad/b163605b3f44c3aadf28f97b125a2f7c461e9a7f/src/silero_vad/data/silero_vad.onnx"
  Sha256 = "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3"
  File = "silero_vad.onnx"
}
# Microsoft's Evergreen bootstrapper is intentionally not pinned by hash: the
# official redirect serves a moving, security-updated binary. Trust is enforced
# with Authenticode status + Microsoft signer + WebView2 product metadata.
$webViewSource = @{
  Url = "https://go.microsoft.com/fwlink/p/?LinkId=2124703"
  File = "MicrosoftEdgeWebview2Setup.exe"
}
$requiredVcRuntimeFiles = @(
  "msvcp140.dll",
  "msvcp140_1.dll",
  "vcruntime140.dll",
  "vcruntime140_1.dll",
  "vcomp140.dll"
)

$cpuFiles = @(
  "ggml.dll",
  "ggml-base.dll",
  "ggml-cpu.dll",
  "whisper.dll",
  "whisper-cli.exe",
  "whisper-server.exe"
)
$cudaFiles = @(
  "cublas64_12.dll",
  "cublasLt64_12.dll",
  "cudart64_12.dll",
  "ggml.dll",
  "ggml-base.dll",
  "ggml-cpu.dll",
  "ggml-cuda.dll",
  "nvrtc-builtins64_124.dll",
  "nvrtc64_120_0.dll",
  "whisper.dll",
  "whisper-cli.exe",
  "whisper-server.exe"
)

function Assert-Sha256 {
  param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$Expected
  )
  $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
  if ($actual -ne $Expected.ToLowerInvariant()) {
    throw "SHA256 mismatch for $Path`: expected $Expected, got $actual"
  }
}

function Download-Verified {
  param(
    [Parameter(Mandatory = $true)][hashtable]$Source,
    [Parameter(Mandatory = $true)][string]$Destination
  )
  & curl.exe -L --fail --silent --show-error --retry 3 --retry-all-errors `
    --connect-timeout 30 --max-time 1800 `
    --output $Destination $Source.Url
  if ($LASTEXITCODE -ne 0) {
    throw "Download failed: $($Source.Url)"
  }
  Assert-Sha256 -Path $Destination -Expected $Source.Sha256
}

function Assert-MicrosoftAuthenticode {
  param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$ExpectedProduct
  )
  $signature = Get-AuthenticodeSignature -LiteralPath $Path
  $subject = if ($signature.SignerCertificate) { $signature.SignerCertificate.Subject } else { "" }
  if ($signature.Status -ne "Valid") {
    throw "Authenticode validation failed for $Path`: $($signature.Status) $($signature.StatusMessage)"
  }
  if ($subject -notmatch "(^|,\s*)O=Microsoft Corporation(,|$)") {
    throw "Unexpected Authenticode signer for $Path`: $subject"
  }
  $product = (Get-Item -LiteralPath $Path).VersionInfo.ProductName
  if ([string]::IsNullOrWhiteSpace($product) -or $product -notmatch $ExpectedProduct) {
    throw "Unexpected product metadata for $Path`: $product"
  }
}

function Download-MicrosoftSigned {
  param(
    [Parameter(Mandatory = $true)][hashtable]$Source,
    [Parameter(Mandatory = $true)][string]$Destination,
    [Parameter(Mandatory = $true)][string]$ExpectedProduct
  )
  & curl.exe -L --fail --silent --show-error --retry 3 --retry-all-errors `
    --connect-timeout 30 --max-time 1800 `
    --output $Destination $Source.Url
  if ($LASTEXITCODE -ne 0) {
    throw "Download failed: $($Source.Url)"
  }
  if (-not (Test-Path -LiteralPath $Destination -PathType Leaf) -or (Get-Item $Destination).Length -eq 0) {
    throw "Downloaded Microsoft prerequisite is missing or empty: $Destination"
  }
  Assert-MicrosoftAuthenticode -Path $Destination -ExpectedProduct $ExpectedProduct
}

function Get-VcRuntimeFiles {
  $programFilesX86 = [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFilesX86)
  $vswhere = Join-Path $programFilesX86 "Microsoft Visual Studio/Installer/vswhere.exe"
  if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
    throw "vswhere.exe was not found: $vswhere"
  }

  $installations = @(& $vswhere -latest -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath)
  $installation = $installations | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -First 1
  if ([string]::IsNullOrWhiteSpace($installation)) {
    throw "Visual Studio with the x64 VC toolchain was not found"
  }

  $redistRoot = Join-Path $installation "VC/Redist/MSVC"
  $versionDirs = @(
    Get-ChildItem -LiteralPath $redistRoot -Directory |
      Where-Object { $_.Name -match '^\d+(\.\d+)+$' } |
      Sort-Object { [version]$_.Name } -Descending
  )
  if ($versionDirs.Count -eq 0) {
    throw "No versioned VC redistributable directory found under $redistRoot"
  }
  $x64Root = Join-Path $versionDirs[0].FullName "x64"
  $crtDir = Get-ChildItem -LiteralPath $x64Root -Directory |
    Where-Object { $_.Name -like "Microsoft.VC*.CRT" } |
    Sort-Object Name -Descending |
    Select-Object -First 1
  $openMpDir = Get-ChildItem -LiteralPath $x64Root -Directory |
    Where-Object { $_.Name -like "Microsoft.VC*.OpenMP" } |
    Sort-Object Name -Descending |
    Select-Object -First 1
  if (-not $crtDir -or -not $openMpDir) {
    throw "VC x64 CRT/OpenMP redistributable directories were not found under $x64Root"
  }

  $filesByName = @{}
  foreach ($file in @(
    Get-ChildItem -LiteralPath $crtDir.FullName -File -Filter "*.dll"
    Get-ChildItem -LiteralPath $openMpDir.FullName -File -Filter "*.dll"
  )) {
    $filesByName[$file.Name.ToLowerInvariant()] = $file
  }
  foreach ($required in $requiredVcRuntimeFiles) {
    if (-not $filesByName.ContainsKey($required.ToLowerInvariant())) {
      throw "VC runtime is missing required app-local DLL: $required"
    }
  }
  return @($filesByName.Values | Sort-Object Name)
}

function Copy-AppLocalRuntime {
  param(
    [Parameter(Mandatory = $true)][System.IO.FileInfo[]]$Files,
    [Parameter(Mandatory = $true)][string[]]$Destinations
  )
  foreach ($destination in $Destinations) {
    New-Item -ItemType Directory -Force -Path $destination | Out-Null
    foreach ($file in $Files) {
      Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $destination $file.Name) -Force
    }
  }
}

function Copy-RequiredFiles {
  param(
    [Parameter(Mandatory = $true)][string]$SourceDir,
    [Parameter(Mandatory = $true)][string]$DestinationDir,
    [Parameter(Mandatory = $true)][string[]]$Files
  )
  New-Item -ItemType Directory -Force -Path $DestinationDir | Out-Null
  foreach ($file in $Files) {
    $sourcePath = Join-Path $SourceDir $file
    if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
      throw "Verified archive is missing required file: $sourcePath"
    }
    Copy-Item -LiteralPath $sourcePath -Destination (Join-Path $DestinationDir $file) -Force
  }
}

$tempBase = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$tempRoot = Join-Path $tempBase ("voxflow-runtime-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null

try {
  $cpuZip = Join-Path $tempRoot $cpuSource.File
  $cudaZip = Join-Path $tempRoot $cudaSource.File
  $vadModel = Join-Path $tempRoot $vadSource.File
  $webViewBootstrapper = Join-Path $tempRoot $webViewSource.File
  $vcRuntimeFiles = @(Get-VcRuntimeFiles)

  Download-Verified -Source $cpuSource -Destination $cpuZip
  Download-Verified -Source $cudaSource -Destination $cudaZip
  Download-Verified -Source $vadSource -Destination $vadModel
  # The official Evergreen bootstrapper is currently versioned as
  # "Microsoft Edge Update" even though the downloaded payload installs the
  # WebView2 Runtime. Accept that Microsoft product name as well as older/newer
  # builds that expose WebView2 directly; Authenticode signer validation above
  # remains the trust boundary.
  Download-MicrosoftSigned -Source $webViewSource -Destination $webViewBootstrapper -ExpectedProduct "WebView2|^Microsoft Edge Update$"

  $cpuExtract = Join-Path $tempRoot "cpu"
  $cudaExtract = Join-Path $tempRoot "cuda"
  Expand-Archive -LiteralPath $cpuZip -DestinationPath $cpuExtract -Force
  Expand-Archive -LiteralPath $cudaZip -DestinationPath $cudaExtract -Force

  $cpuDest = Join-Path $RuntimeRoot "whisper/Release"
  $cudaDest = Join-Path $RuntimeRoot "whisper-cuda/Release"
  $vadDest = Join-Path $RuntimeRoot "vad"
  $redistDest = Join-Path $RuntimeRoot "windows-redist"
  $prereqDest = Join-Path $RuntimeRoot "windows-prerequisites"
  foreach ($destination in @($cpuDest, $cudaDest, $vadDest, $redistDest, $prereqDest)) {
    Remove-Item -LiteralPath $destination -Recurse -Force -ErrorAction SilentlyContinue
  }
  Copy-RequiredFiles -SourceDir (Join-Path $cpuExtract "Release") -DestinationDir $cpuDest -Files $cpuFiles
  Copy-RequiredFiles -SourceDir (Join-Path $cudaExtract "Release") -DestinationDir $cudaDest -Files $cudaFiles
  # App-local deployment avoids an elevated VC_redist install. Keep one source
  # directory for {app}, and duplicate the DLLs beside both whisper sidecars so
  # Windows' normal DLL search resolves their CRT/OpenMP dependencies reliably.
  Copy-AppLocalRuntime -Files $vcRuntimeFiles -Destinations @($redistDest, $cpuDest, $cudaDest)
  New-Item -ItemType Directory -Force -Path $vadDest | Out-Null
  Copy-Item -LiteralPath $vadModel -Destination (Join-Path $vadDest "silero_vad.onnx") -Force
  Assert-Sha256 -Path (Join-Path $vadDest "silero_vad.onnx") -Expected $vadSource.Sha256
  New-Item -ItemType Directory -Force -Path $prereqDest | Out-Null
  $webViewInstalledPath = Join-Path $prereqDest $webViewSource.File
  Copy-Item -LiteralPath $webViewBootstrapper -Destination $webViewInstalledPath -Force
  Assert-MicrosoftAuthenticode -Path $webViewInstalledPath -ExpectedProduct "WebView2|^Microsoft Edge Update$"

  foreach ($required in @(
    (Join-Path $cpuDest "whisper-cli.exe"),
    (Join-Path $cpuDest "whisper-server.exe"),
    (Join-Path $cudaDest "whisper-cli.exe"),
    (Join-Path $cudaDest "whisper-server.exe"),
    (Join-Path $vadDest "silero_vad.onnx"),
    $webViewInstalledPath
  )) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf) -or (Get-Item $required).Length -eq 0) {
      throw "Runtime resource is missing or empty: $required"
    }
  }
  foreach ($destination in @($redistDest, $cpuDest, $cudaDest)) {
    foreach ($required in $requiredVcRuntimeFiles) {
      $path = Join-Path $destination $required
      if (-not (Test-Path -LiteralPath $path -PathType Leaf) -or (Get-Item $path).Length -eq 0) {
        throw "App-local VC runtime is missing or empty: $path"
      }
    }
  }

  Write-Host "Verified Windows runtime prepared in $RuntimeRoot ($($vcRuntimeFiles.Count) app-local VC DLLs)"
}
finally {
  Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
}
