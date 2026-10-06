# Download the latest shinchiro libmpv build (libmpv-2.dll) for Windows x86_64.
# Usage: pwsh scripts/fetch-libmpv-windows.ps1 [-Dest target/release]
param([string]$Dest = "target/release")
$ErrorActionPreference = "Stop"
$rel = Invoke-RestMethod "https://api.github.com/repos/shinchiro/mpv-winbuild-cmake/releases/latest" `
    -Headers @{ "User-Agent" = "fluxplay-build" }
$asset = $rel.assets | Where-Object { $_.name -match '^mpv-dev-x86_64-\d{8}-git-.*\.7z$' } | Select-Object -First 1
if (-not $asset) { throw "mpv-dev-x86_64 asset not found in $($rel.tag_name)" }
$tmp = Join-Path ([IO.Path]::GetTempPath()) "fluxplay-libmpv"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$archive = Join-Path $tmp $asset.name
Invoke-WebRequest $asset.browser_download_url -OutFile $archive
# BCJ2-compressed 7z: needs real 7-Zip (preinstalled on GitHub runners; else fetch 7zr).
$7z = (Get-Command 7z -ErrorAction SilentlyContinue).Source
if (-not $7z) {
    $7z = Join-Path $tmp "7zr.exe"
    Invoke-WebRequest "https://www.7-zip.org/a/7zr.exe" -OutFile $7z
}
& $7z e -y "-o$tmp" $archive "libmpv-2.dll" | Out-Null
New-Item -ItemType Directory -Force -Path $Dest | Out-Null
Copy-Item (Join-Path $tmp "libmpv-2.dll") $Dest -Force
Write-Host "libmpv-2.dll ($($asset.name)) -> $Dest"
