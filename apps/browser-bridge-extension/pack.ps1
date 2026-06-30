# Pacchettizza l'estensione Nexus Browser Bridge da Windows.
#
# Produce in dist/:
#   browser-bridge-extension-<ver>.zip
#   browser-bridge-extension-<ver>.crx
#   key.pem            (chiave privata persistente, NON committare)
#
# Il calcolo dell'extension ID, di update.xml e degli script di installazione
# (install-windows.ps1 / install-linux.sh) e` delegato al daemon
# browser-bridge-mcp che li genera a runtime via gli endpoint /extension/*.
#
# Uso: powershell -ExecutionPolicy Bypass -File .\pack.ps1

[CmdletBinding()]
param()
$ErrorActionPreference = "Stop"

$Src  = $PSScriptRoot
$Dist = Join-Path $Src "dist"
New-Item -ItemType Directory -Path $Dist -Force | Out-Null

$manifest = Get-Content (Join-Path $Src "manifest.json") -Raw | ConvertFrom-Json
$Version  = $manifest.version
$BaseName = "browser-bridge-extension-$Version"
Write-Host "==> versione $Version"

$ChromePaths = @(
    "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
    "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe",
    "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe"
)
$Chrome = $ChromePaths | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $Chrome) { throw "Chrome non trovato" }
Write-Host "==> Chrome: $Chrome"

# ZIP
$ZipPath = Join-Path $Dist "$BaseName.zip"
if (Test-Path $ZipPath) { Remove-Item $ZipPath -Force }
Compress-Archive -Path `
    (Join-Path $Src "manifest.json"),(Join-Path $Src "background.js"),(Join-Path $Src "popup.html"),(Join-Path $Src "popup.js"),(Join-Path $Src "icon128.png") `
    -DestinationPath $ZipPath -Force
Write-Host "==> $ZipPath"

# CRX
$PackDir = Join-Path $Dist "_pack-$Version"
if (Test-Path $PackDir) { Remove-Item -Recurse -Force $PackDir }
New-Item -ItemType Directory -Path $PackDir | Out-Null
Copy-Item (Join-Path $Src "manifest.json"),(Join-Path $Src "background.js"),(Join-Path $Src "popup.html"),(Join-Path $Src "popup.js"),(Join-Path $Src "icon128.png") -Destination $PackDir

$Key = Join-Path $Dist "key.pem"
if (Test-Path $Key) {
    & $Chrome "--pack-extension=$PackDir" "--pack-extension-key=$Key" 2>&1 | Out-Null
} else {
    & $Chrome "--pack-extension=$PackDir" 2>&1 | Out-Null
    $Generated = Join-Path $Dist "_pack-$Version.pem"
    if (Test-Path $Generated) { Move-Item $Generated $Key }
}
$GeneratedCrx = Join-Path $Dist "_pack-$Version.crx"
$CrxPath = Join-Path $Dist "$BaseName.crx"
if (Test-Path $GeneratedCrx) {
    Move-Item -Force $GeneratedCrx $CrxPath
    Remove-Item -Recurse -Force $PackDir
    Write-Host "==> $CrxPath"
} else {
    throw "Chrome non ha prodotto .crx"
}

Write-Host ""
Write-Host "Avvia il daemon (cargo run -p browser-bridge-mcp) e poi:"
Write-Host "  - GET http://127.0.0.1:4055/extension/info"
Write-Host "  - GET http://127.0.0.1:4055/extension/install.ps1   (per Windows)"
Write-Host "  - GET http://127.0.0.1:4055/extension/install.sh    (per Linux)"
