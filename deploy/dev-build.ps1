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
  # IDEMPOTENTE (fix definitivo, regola H): vcvars64.bat APPENDE a PATH. Importare
  # il suo ambiente nella sessione PowerShell a ogni run compone il PATH a ogni
  # build; nella stessa finestra dopo qualche giro il `set PATH=%PATH%;...` interno
  # di vcvars supera il limite di cmd (~8191 char) -> "Linea in ingresso troppo
  # lunga" (build che PRIMA funzionava). Se l'ambiente MSVC e' gia' applicato in
  # questa sessione (VSCMD_VER settato da un vcvars precedente), NON ri-eseguirlo:
  # cargo usa l'env gia' presente e il PATH non cresce piu'.
  if ($env:VSCMD_VER) {
    Write-Host '   MSVC gia inizializzato in questa sessione (skip vcvars).' -ForegroundColor DarkGray
    return
  }
  # Deduplica il PATH ereditato PRIMA di vcvars: il `set PATH=%PATH%;...` interno
  # di vcvars gira in cmd (limite ~8191) e un PATH gia' gonfio (duplicati) lo fa
  # esplodere. Rimuovere i duplicati da' margine anche al primo run di una sessione.
  $env:PATH = (($env:PATH -split ';' | Where-Object { $_ } | Select-Object -Unique) -join ';')
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
