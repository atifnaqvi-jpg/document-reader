<#
.SYNOPSIS
  Downloads the PDFium shared library used for native PDF rendering.

.DESCRIPTION
  P1 onwards needs `pdfium.dll` at runtime (docs/P0-DESIGN.md §2). The app looks it
  up via the PDFIUM_LIBRARY_PATH environment variable, falling back to a copy next
  to the executable. Run this script once per dev machine.

  Pinning the version keeps rendering deterministic across machines. Bump
  $PdfiumVersion deliberately, then re-run the manual PDF smoke test.
#>

[CmdletBinding()]
param(
    # Verified against https://github.com/bblanchon/pdfium-binaries — bump
    # deliberately, then re-run the PDF smoke test before shipping a release.
    [string]$PdfiumVersion = "chromium/8066",
    [string]$Destination = "$PSScriptRoot\..\src-tauri\resources\pdfium.dll"
)

$ErrorActionPreference = "Stop"

$url = "https://github.com/bblanchon/pdfium-binaries/releases/download/$PdfiumVersion/pdfium-win-x64.tgz"
$tempArchive = Join-Path $env:TEMP "pdfium-win-x64.tgz"
$extractDir = Join-Path $env:TEMP "pdfium-extract"

Write-Host "Downloading PDFium $PdfiumVersion ..."
Invoke-WebRequest -Uri $url -OutFile $tempArchive

if (Test-Path $extractDir) { Remove-Item $extractDir -Recurse -Force }
New-Item -ItemType Directory -Path $extractDir | Out-Null

# tar is available on Windows 10+ and handles .tgz directly.
tar -xzf $tempArchive -C $extractDir

$binary = Get-ChildItem -Path $extractDir -Recurse -Filter "pdfium.dll" | Select-Object -First 1
if (-not $binary) { throw "pdfium.dll not found in the downloaded archive" }

# Guard against a truncated download or an HTML error page saved as .dll —
# a bad DLL would only surface later as a confusing load failure at runtime.
if ($binary.Length -lt 1MB) {
    throw "pdfium.dll looks wrong ($($binary.Length) bytes); expected several MB. Download may have failed."
}

$destinationDir = Split-Path $Destination -Parent
New-Item -ItemType Directory -Path $destinationDir -Force | Out-Null
Copy-Item $binary.FullName $Destination -Force

Write-Host "PDFium installed to $Destination ($([math]::Round($binary.Length / 1MB, 1)) MB)"
Write-Host "For development, also set: `$env:PDFIUM_LIBRARY_PATH = '$Destination'"
