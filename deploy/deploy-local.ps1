# Porting Windows di deploy/deploy-local.sh (porting WSL->Windows nativo).
#
# Workflow: BUILD Rust (a stack acceso) -> STOP -> BUILD web -> PUBBLICA -> MIGRA -> START.
# Il web-ide si costruisce DOPO lo stop: `next build` riscrive la dir da cui node
# sta servendo, e la cancella all'inizio (404 sui chunk per tutta la build).
# I servizi eseguono da $RUNTIME_BIN, non da target\: su Windows un .exe in
# esecuzione e' lockato, e finche' i manifest puntavano a target\debug ogni
# `cargo build` a stack vivo moriva con "Accesso negato (os error 5)" DOPO aver
# ricompilato tutto. Separate le due directory, la build non tocca mai un file
# lockato: si compila mentre lo stack lavora e i servizi restano giu' solo per la
# copia. Una build fallita non ferma piu' nulla.
# Default: debug. Uso:
#   .\deploy-local.ps1             build Rust + web-ide, con stop/start servizi
#   .\deploy-local.ps1 -Rust       solo Rust
#   .\deploy-local.ps1 -Web        solo web-ide
#   .\deploy-local.ps1 -NoRestart  build senza toccare i servizi (NON serve admin)
#
# Due ambienti, rilevati automaticamente (vedi $serviziInstallati piu' sotto):
# servizi WinSW installati -> stop/start via Stop-Service; nessun servizio
# applicativo -> stack a PROCESSI, si delega a dev-stop.ps1 / dev-start.ps1.
#
# MIGRAZIONI DB: questo script NON le applica. Le esegue mcp-core all'avvio, per
# questo il riavvio non e' opzionale quando il commit ne porta di nuove: con
# -NoRestart i binari sono aggiornati ma lo schema resta indietro.
param([switch]$Rust, [switch]$Web, [switch]$NoRestart)
$ErrorActionPreference = 'Stop'
$ROOT = 'D:\IDEAI'

# Kill + verifica del fatto: punto unico condiviso con dev-service.ps1/dev-stop.ps1.
. (Join-Path $PSScriptRoot 'lib\nexus-process.ps1')
# Pubblicazione degli artefatti: punto unico condiviso con dev-build.ps1. La
# directory di destinazione NON si ricompone qui — si DERIVA dai manifest, che
# sono gia' la fonte unica di dev-start/dev-stop. Una seconda copia della regola
# in PowerShell, accanto a quella in Rust, divergerebbe in silenzio.
. (Join-Path $PSScriptRoot 'lib\nexus-publish.ps1')

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
  # Kill + VERIFICA via Stop-NexusProcessTree: un processo sopravvissuto tiene
  # lockato il proprio .exe sotto $RUNTIME_BIN, e la COPIA piu' sotto fallirebbe
  # con un opaco `os error 5` (la build non e' piu' in gioco: gira prima, su
  # target\, che nessun servizio tocca). Ritorna i sopravvissuti al chiamante,
  # che decide (qui: pubblicazione annullata).
  $alive = @()
  if ($procId -ne 0) {
    $res = Stop-NexusProcessTree -ProcessId $procId -Label $name -KillTree
    if (-not $res.Stopped) { $alive += $res.Message }
  }
  foreach ($k in $kids) {
    if ($k) {
      $res = Stop-NexusProcessTree -ProcessId ([int]$k) -Label "$name (figlio)" -KillTree
      if (-not $res.Stopped) { $alive += $res.Message }
    }
  }
  return $alive
}

$doRust = $Rust -or (-not $Rust -and -not $Web)
$doWeb  = $Web  -or (-not $Rust -and -not $Web)
$rustSvc = 'nexus-mcp-core','nexus-gateway','nexus-admin','nexus-doc','nexus-plugin'

# MODALITA' DELL'AMBIENTE: servizi WinSW installati, oppure stack a PROCESSI?
# Non e' un dettaglio cosmetico: `Stop-ServiceTree` risolve il PID SOLO via
# Win32_Service, quindi senza servizi installati $procId resta 0, non ferma
# nulla, e i 5 "Impossibile trovare un servizio" sembrano innocui mentre sono
# LA CAUSA del `cargo build ... os error 5` due schermate piu' sotto (i binari
# restano lockati dai processi vivi). Incidente 2026-07-19.
# In modalita' processi si DELEGA a dev-stop.ps1/dev-start.ps1 invece di
# duplicare qui la loro logica (regola L): sanno gia' leggere i manifest WinSW
# come fonte unica di eseguibile/env, ruotano i log e tengono il file dei PID.
$serviziInstallati = @(Get-Service -Name $rustSvc -ErrorAction SilentlyContinue).Count -gt 0
if (-not $NoRestart -and -not $serviziInstallati) {
  Write-Host '== ambiente a PROCESSI (nessun servizio WinSW installato): delego a dev-stop/dev-start ==' -ForegroundColor Yellow
  Write-Host '   nota: dev-stop/dev-start governano lo stack INTERO (anche qdrant/garnet/web-ide),' -ForegroundColor DarkYellow
  Write-Host '   non solo i binari selezionati da -Rust/-Web.' -ForegroundColor DarkYellow
}

# 1. STOP (solo se gestiamo i servizi) — kill-tree per non lasciare orfani.
# Se qualcosa sopravvive si INTERROMPE qui: proseguire vorrebbe dire compilare
# contro eseguibili lockati (os error 5) e poi riavviare servizi mai fermati.
# 1. BUILD — A STACK ACCESO.
#
# I servizi eseguono da $RUNTIME_BIN, non da target\: nessun artefatto che cargo
# sta per riscrivere e' lockato, quindi si compila mentre lo stack lavora. Prima
# l'ordine era STOP -> BUILD -> START e il downtime durava quanto la build (per
# mcp-core: minuti); ora dura quanto una copia di file.
# Vale anche come rete: una build che fallisce lascia lo stack in piedi sulla
# versione precedente, invece di averlo gia' fermato per niente.
if ($doRust) {
  Write-Host '== build Rust (MSVC, stack ancora attivo) ==' -ForegroundColor Cyan
  Initialize-Msvc
  Set-Location $ROOT
  cargo build --workspace
  if ($LASTEXITCODE -ne 0) { throw 'cargo build fallito' }
}
# NB: la build del web-ide NON sta qui. Compilare a stack acceso e' sicuro solo
# per i binari Rust, che eseguono da una copia: `next build` invece riscrive
# `apps/web-ide/.next`, cioe' la directory da cui il processo node sta servendo
# (build non-standalone). Peggio: next cancella `.next` all'INIZIO della build
# (cleanDistDir, default true), quindi il server vivo risponderebbe 404 sui chunk
# per TUTTA la durata della compilazione, non solo durante il riavvio. Sta dopo lo
# stop, al passo 2b.

# 2. STOP — solo ora, e solo per il tempo della copia.
if (-not $NoRestart) {
  if (-not $serviziInstallati) {
    # dev-stop.ps1 esce 1 se qualcosa sopravvive al kill (tipicamente: processi
    # elevati e shell non elevata). Proseguire vorrebbe dire copiare sopra
    # eseguibili lockati: si interrompe qui, come nel ramo servizi.
    & (Join-Path $PSScriptRoot 'dev-stop.ps1')
    if ($LASTEXITCODE -ne 0) {
      throw 'dev-stop.ps1 non ha fermato tutto (vedi sopra): copia annullata, gli eseguibili sono lockati.'
    }
  } else {
    $survivors = @()
    if ($doRust) { foreach ($s in $rustSvc) { $survivors += Stop-ServiceTree $s } }
    if ($doWeb)  { $survivors += Stop-ServiceTree 'nexus-web-ide' }
    if ($survivors.Count -gt 0) {
      $survivors | ForEach-Object { Write-Host $_ -ForegroundColor Red }
      throw "$($survivors.Count) processo/i non terminato/i (vedi sopra): copia annullata."
    }
  }
  Start-Sleep -Seconds 2
}

# 2a. BUILD del web-ide, ORA che il suo processo e' fermo (vedi la nota al passo 1).
if ($doWeb -and -not $NoRestart) {
  Write-Host '== build web-ide (next, a servizio fermo) ==' -ForegroundColor Cyan
  Set-Location "$ROOT\apps\web-ide"
  # NODE_ENV vale per QUESTA build e viene ripristinato subito: piu' avanti la
  # stessa shell avvia lo stack via dev-start.ps1, e quei processi ereditano il
  # suo ambiente. Lasciandolo impostato, mcp-core ereditava NODE_ENV=production e
  # lo passava ai comandi degli agenti: `npm install` nei progetti saltava le
  # devDependencies (niente Vite, niente Tailwind) senza che nulla fallisse.
  # Misurato il 29/07/2026: 33 pacchetti installati invece di 112.
  $nodeEnvPrec = [Environment]::GetEnvironmentVariable('NODE_ENV', 'Process')
  $env:NODE_ENV = 'production'
  try {
    pnpm exec next build
    if ($LASTEXITCODE -ne 0) { throw 'next build fallito' }
  } finally {
    if ($null -eq $nodeEnvPrec) { Remove-Item -Path env:NODE_ENV -ErrorAction SilentlyContinue }
    else { $env:NODE_ENV = $nodeEnvPrec }
  }
  Set-Location $ROOT
} elseif ($doWeb) {
  # Con -NoRestart non si e' fermato nulla: ricostruire .next sotto il node vivo
  # gli farebbe servire 404 sui chunk. Meglio non farlo e dirlo.
  Write-Host '== web-ide SALTATO: -NoRestart non ferma il processo, e next build ==' -ForegroundColor Yellow
  Write-Host '   riscrive la dir da cui sta servendo. Rilanciare senza -NoRestart.' -ForegroundColor Yellow
}

# 2b. PUBBLICAZIONE — gli artefatti passano da target\ alla dir da cui si esegue.
#
# E' il passo che rende vero tutto il resto: finche' non si copia, i servizi
# ripartono sulla versione precedente. Delega al punto unico condiviso con
# dev-build.ps1 (lib\nexus-publish.ps1), che deriva la destinazione DAI MANIFEST e
# pubblica i binari che i manifest dichiarano — non un glob su target\debug, che
# copierebbe anche binari il cui servizio nessuno ha fermato.
if ($doRust -and -not $NoRestart) {
  Write-Host '== pubblicazione artefatti ==' -ForegroundColor Cyan
  $esito = Publish-NexusArtifacts -BuildDir "$ROOT\target\debug"
  if ($esito.Pubblicati.Count -eq 0) {
    throw "nessun artefatto pubblicato in $($esito.PublishDir): i servizi ripartirebbero sulla versione precedente."
  }
} elseif ($doRust) {
  # -NoRestart: i binari restano in target\, i servizi eseguono dalla copia. Detto
  # esplicitamente, perche' prima della separazione compila/esegue la build finiva
  # direttamente nella dir di esecuzione e questo flag bastava.
  Write-Host '== -NoRestart: artefatti NON pubblicati ==' -ForegroundColor Yellow
  Write-Host '   i binari nuovi sono in target\debug; i servizi restano sulla versione' -ForegroundColor Yellow
  Write-Host '   precedente finche non si ripete senza -NoRestart.' -ForegroundColor Yellow
}

# 2b. MIGRAZIONI, fra la build e l'avvio.
#
# Perche' QUI e non altrove: e' il solo punto in cui i binari sono aggiornati e
# i servizi sono ancora fermi. Applicare lo schema mentre mcp-core gira
# significherebbe eseguire DDL sotto un processo in esecuzione; applicarlo prima
# della build userebbe un xtask vecchio.
#
# Perche' ESISTE questo passo, dato che mcp-core migra da solo all'avvio: senza,
# il comando `xtask migrate` resterebbe esercitato solo a mano su Windows, cioe'
# sull'unico sistema su cui questo progetto gira — e uno strumento che nessuno
# invoca non e' uno strumento, e' una nota. Con questo passo lo schema e'
# aggiornato PRIMA che il servizio parta, e un fallimento delle migrazioni si
# vede qui invece di comparire come un avvio che non riesce.
#
# NON e' ridondante rispetto a mcp-core: applicare un set gia' applicato non fa
# nulla (il registro `_sqlx_migrations` lo sa), quindi il costo e' una lettura.
if ($doRust -and -not $NoRestart) {
  Write-Host '== migrazioni schema META ==' -ForegroundColor Cyan
  Set-Location $ROOT
  cargo run --quiet -p xtask -- migrate --set meta --apply
  if ($LASTEXITCODE -ne 0) { throw "migrazioni fallite (exit $LASTEXITCODE): lo stack non viene riavviato su uno schema che non e' quello del codice" }
}

# 3. START
if (-not $NoRestart) {
  if (-not $serviziInstallati) {
    # Senza questo ramo il deploy finiva con lo stack GIU': Start-Service falliva
    # sui servizi inesistenti e nessuno riavviava i processi (incidente
    # 2026-07-19). dev-start.ps1 rispetta l'ordine (mcp-core prima, +5s) e ruota
    # i log; NON richiede admin.
    & (Join-Path $PSScriptRoot 'dev-start.ps1')
    if ($LASTEXITCODE -ne 0) {
      Write-Host 'dev-start.ps1 non ha avviato lo stack: i binari sono aggiornati ma i servizi sono GIU.' -ForegroundColor Red
    } else {
      Write-Host '== stack riavviato (processi) ==' -ForegroundColor Green
    }
  } else {
    if ($doRust) { Start-Service nexus-mcp-core -ErrorAction Continue; Start-Sleep -Seconds 5; foreach ($s in $rustSvc | Where-Object { $_ -ne 'nexus-mcp-core' }) { Start-Service $s -ErrorAction Continue } }
    if ($doWeb)  { Start-Service nexus-web-ide -ErrorAction Continue }
    Write-Host '== servizi riavviati ==' -ForegroundColor Green
    Get-Service nexus-* | Format-Table Name, Status -AutoSize
  }
}
Set-Location $ROOT
