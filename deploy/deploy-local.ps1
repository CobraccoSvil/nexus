# Porting Windows di deploy/deploy-local.sh (porting WSL->Windows nativo).
# Workflow corretto: STOP servizi -> BUILD -> START (gli .exe in esecuzione sono
# lockati su Windows, quindi vanno fermati prima di ricompilarli).
# Default: debug (i servizi WinSW puntano a target\debug). Uso:
#   .\deploy-local.ps1             build Rust + web-ide, con stop/start servizi
#   .\deploy-local.ps1 -Rust       solo Rust
#   .\deploy-local.ps1 -Web        solo web-ide
#   .\deploy-local.ps1 -NoRestart  build senza toccare i servizi (NON serve admin)
param([switch]$Rust, [switch]$Web, [switch]$NoRestart)
$ErrorActionPreference = 'Stop'
$ROOT = 'D:\IDEAI'

# Auto-elevazione: stop/start dei servizi Windows richiede admin.
if (-not $NoRestart) {
  $isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)
  if (-not $isAdmin) {
    $argList = @('-NoProfile','-ExecutionPolicy','Bypass','-File',$PSCommandPath)
    if ($Rust) { $argList += '-Rust' }
    if ($Web)  { $argList += '-Web' }
    Start-Process powershell.exe -Verb RunAs -Wait -ArgumentList $argList
    return
  }
}

function Initialize-Msvc {
  $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
  $vsPath  = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
  $vcvars  = Join-Path $vsPath 'VC\Auxiliary\Build\vcvars64.bat'
  cmd /c "`"$vcvars`" && set" | ForEach-Object { if ($_ -match '^([^=]+)=(.*)$') { Set-Item -Path "env:$($matches[1])" -Value $matches[2] } }
}

function Stop-ServiceTree($name) {
  # Ferma il servizio e rimuove i figli orfani (dev server avviati dall'agente) che
  # altrimenti tengono le porte / i socket ereditati da mcp-core (es. :4000): al
  # restart il bind fallisce con WSAEADDRINUSE (os error 10048) -> crash loop WinSW.
  # ORDINE CRITICO: NON killare il padre per primo, perche' WinSW lo vedrebbe morto
  # inaspettatamente e lo rilancerebbe subito (locka l'exe -> build fallita). Quindi:
  # 1) cattura i figli mentre il padre e' vivo; 2) Stop-Service (stop deliberato,
  # WinSW NON rilancia); 3) force-kill il padre solo se appeso (servizio gia'
  # "stopped" -> niente restart); 4) killa i figli catturati. Regola E: solo PID
  # Nexus risolti via Win32_Service / loro figli, mai PID generici.
  $procId = 0
  $kids = @()
  try {
    $svc = Get-CimInstance Win32_Service -Filter "Name='$name'" -ErrorAction Stop
    if ($svc) { $procId = [int]$svc.ProcessId }
    if ($procId -ne 0) {
      $kids = @((Get-CimInstance Win32_Process -Filter "ParentProcessId=$procId" -ErrorAction SilentlyContinue).ProcessId)
    }
  } catch { }
  Stop-Service $name -Force -ErrorAction Continue
  Start-Sleep -Milliseconds 800
  # taskkill via cmd /c con redirect INTERNO: in PS 5.1 con ErrorActionPreference=Stop
  # un '2>$null' su exe nativo genera comunque un ErrorRecord (NativeCommandError) e
  # quindi un throw su "processo non trovato". Dentro cmd lo stderr non risale a PS.
  if ($procId -ne 0 -and (Get-Process -Id $procId -ErrorAction SilentlyContinue)) {
    cmd /c "taskkill /PID $procId /T /F >nul 2>nul"
  }
  foreach ($k in $kids) { if ($k) { cmd /c "taskkill /PID $k /T /F >nul 2>nul" } }
}

$doRust = $Rust -or (-not $Rust -and -not $Web)
$doWeb  = $Web  -or (-not $Rust -and -not $Web)
$rustSvc = 'nexus-mcp-core','nexus-gateway','nexus-admin','nexus-doc','nexus-plugin'

# 1. STOP (solo se gestiamo i servizi) — kill-tree per non lasciare orfani
if (-not $NoRestart) {
  if ($doRust) { foreach ($s in $rustSvc) { Stop-ServiceTree $s } }
  if ($doWeb)  { Stop-ServiceTree 'nexus-web-ide' }
  Start-Sleep -Seconds 2
}

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

# 3. START
if (-not $NoRestart) {
  if ($doRust) { Start-Service nexus-mcp-core -ErrorAction Continue; Start-Sleep -Seconds 5; foreach ($s in $rustSvc | Where-Object { $_ -ne 'nexus-mcp-core' }) { Start-Service $s -ErrorAction Continue } }
  if ($doWeb)  { Start-Service nexus-web-ide -ErrorAction Continue }
  Write-Host '== servizi riavviati ==' -ForegroundColor Green
  Get-Service nexus-* | Format-Table Name, Status -AutoSize
}
Set-Location $ROOT
