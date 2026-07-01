# Ciclo di test locale SENZA servizi Windows: FERMA lo stack -> BUILD -> RIAVVIA.
# I .exe in esecuzione sono lockati su Windows, quindi vanno fermati prima di
# ricompilarli. Sostituisce deploy-local.ps1 nel flusso "processi" (che invece
# gestisce i servizi WinSW). NON richiede admin: nessun servizio Windows coinvolto.
#
# Uso:
#   .\deploy\dev-build.ps1            build completa (Rust + web-ide) e riavvio
#   .\deploy\dev-build.ps1 -Rust      solo Rust
#   .\deploy\dev-build.ps1 -Web       solo web-ide
#   .\deploy\dev-build.ps1 -NoStart   build senza riavviare lo stack
param([switch]$Rust, [switch]$Web, [switch]$NoStart)
$ErrorActionPreference = 'Stop'
$ROOT = 'D:\IDEAI'

function Initialize-Msvc {
  $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
  $vsPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
  $vcvars = Join-Path $vsPath 'VC\Auxiliary\Build\vcvars64.bat'
  cmd /c "`"$vcvars`" && set" | ForEach-Object { if ($_ -match '^([^=]+)=(.*)$') { Set-Item -Path "env:$($matches[1])" -Value $matches[2] } }
}

$doRust = $Rust -or (-not $Rust -and -not $Web)
$doWeb = $Web -or (-not $Rust -and -not $Web)

# 1. STOP (i binari lockati non sono ricompilabili mentre girano)
& "$PSScriptRoot\dev-stop.ps1"
Start-Sleep -Seconds 2

# 2. BUILD
if ($doRust) {
  Write-Host '== build Rust (MSVC) ==' -ForegroundColor Cyan
  Initialize-Msvc
  Set-Location $ROOT
  cargo build --workspace
  if ($LASTEXITCODE -ne 0) { throw 'cargo build fallito' }
}
if ($doWeb) {
  Write-Host '== build web-ide (next) ==' -ForegroundColor Cyan
  Set-Location "$ROOT\apps\web-ide"
  $env:NODE_ENV = 'production'
  pnpm exec next build
  if ($LASTEXITCODE -ne 0) { throw 'next build fallito' }
}
Set-Location $ROOT

# 3. START
if (-not $NoStart) {
  & "$PSScriptRoot\dev-start.ps1"
}
