# Controllo di UN SINGOLO microservizio Nexus su Windows (start/stop/restart).
# Punto unico Windows (regola L) invocato da mcp-core (crate system_services)
# per l'endpoint /api/system/services/<name>/<action> e dal services_watchdog.
# E' il gemello per-servizio di dev-start.ps1/dev-stop.ps1 (batch) e copre i due
# modelli di esecuzione documentati:
#   1) WinSW: se esiste un servizio Windows con questo nome -> Start/Stop/Restart-Service (SCM);
#   2) processi (canonico in dev, WinSW disinstallato): PID in nexus-dev.pids.json,
#      eseguibile/working-dir/env/args dal manifest WinSW usato come fonte unica.
#
# NON tocca i database (postgresql-x64-17, nexus-pg-*): non sono controllabili
# dal catalogo (readonly). NON richiede admin nel modello a processi.
#
# Esce 0 in caso di successo (o no-op idempotente), 1 in caso di errore; il
# messaggio d'errore va su stderr (mcp-core lo restituisce come `stderr`).
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][ValidateSet('start', 'stop', 'restart')][string]$Action,
  [Parameter(Mandatory = $true)][string]$Service
)

$ErrorActionPreference = 'Stop'
$RUNTIME = 'D:\IDEAI-runtime'
$WINSW   = Join-Path $RUNTIME 'winsw'
$LOGDIR  = Join-Path $RUNTIME 'dev-logs'
$PIDFILE = Join-Path $RUNTIME 'nexus-dev.pids.json'

# Kill + verifica del fatto: punto unico condiviso con dev-stop.ps1/deploy-local.ps1.
. (Join-Path $PSScriptRoot 'lib\nexus-process.ps1')
# Lettura dei manifest: punto unico condiviso con dev-start.ps1/nexus-publish.ps1.
. (Join-Path $PSScriptRoot 'lib\nexus-manifest.ps1')
# FORMA del pidfile: campi, lettura, scrittura, costruzione di una voce e
# completamento delle prove mancanti. Punto unico condiviso con dev-start/dev-stop.
. (Join-Path $PSScriptRoot 'lib\nexus-pidfile.ps1')

function Write-Info([string]$msg) { Write-Output $msg }
function Fail([string]$msg) { [Console]::Error.WriteLine($msg); exit 1 }

# ── Pidfile: VOCI INTERE, mai una vista ridotta ───────────────────────────────
#
# QUI STAVA IL DIFETTO (misurato il 09/08/2026). Questo script leggeva il pidfile
# in una hashtable `id -> pid` e lo RISCRIVEVA tutto da quella: ogni azione su un
# SOLO servizio — e mcp-core ne innesca a comando (endpoint
# /api/system/services/<id>/<action>) e da solo (services_watchdog) — cancellava
# `start` ed `exe` da TUTTE e nove le voci. Da li' in poi nessun pid era piu'
# identificabile, quindi nessuno era dichiarabile morto, quindi dev-stop.ps1
# usciva 1 e deploy-local.ps1 si fermava con gli eseguibili lockati.
#
# Il rimedio non e' ricordarsi di ricopiare i campi: e' non avere piu' una forma
# ridotta da cui riscrivere. Si legge un array di voci intere, si sostituisce la
# sola voce toccata (Set-NexusPidEntry) e si riscrive l'array — e comunque
# Write-NexusPidFile proietta sui campi canonici, cosi' una voce impoverita non
# puo' piu' arrivare su disco da nessun percorso.
#
# `$script:voci` e' lo stato condiviso invece di un valore di ritorno: queste
# funzioni scrivono anche su stdout (Write-Info, che mcp-core legge), e in
# PowerShell un `return` si mescolerebbe a quell'output.
$script:voci = @()

function Read-Voci {
  if (-not (Test-Path $PIDFILE)) { $script:voci = @(); return }
  try { $lette = Read-NexusPidFile -Path $PIDFILE }
  catch {
    # Un pidfile illeggibile non e' «nessun processo in giro»: proseguire
    # spawnerebbe un secondo servizio sopra uno vivo. Stessa condotta della
    # guardia di dev-start.
    Fail "pidfile illeggibile ($($_.Exception.Message)): non so cosa sia in esecuzione. Eseguire .\deploy\dev-stop.ps1 e riprovare."
  }
  $script:voci = Resolve-NexusPidEntries -Voci @($lette) -WinswRoot $WINSW
}

# Il verdetto sulla voce di un servizio, o $null se non e' registrato. Delega al
# criterio unico: `Test-NexusProcessAlive` risponde alla domanda RISTRETTA «esiste
# un pid?», che la sua stessa libreria dichiara sbagliata per un pid letto da un
# registro — un pid riciclato passerebbe per il servizio.
function Get-Verdetto([string]$id) {
  $voce = Get-NexusPidEntry -Voci $script:voci -Id $id
  if (-not $voce) { return $null }
  return (Get-NexusStackLiveness -Voci @($voce))[0]
}

# ── Rotazione log (3 generazioni), come dev-start.ps1 ─────────────────────────
function Invoke-DevLogRotation([string]$path) {
  if (-not (Test-Path $path)) { return }
  if ((Get-Item $path).Length -eq 0) { return }
  try {
    for ($i = 2; $i -ge 1; $i--) {
      $src = "$path.$i"
      if (Test-Path $src) { Move-Item -Force $src "$path.$($i + 1)" }
    }
    Move-Item -Force $path "$path.1"
  }
  catch { Write-Warning "rotazione log fallita per ${path} (handle aperto? verra' troncato)" }
}

# ── Avvio dal manifest WinSW (fonte unica exe/cwd/env/args), come dev-start ────
function Start-FromManifest([string]$id) {
  $xmlPath = Join-Path $WINSW "$id\$id.xml"
  if (-not (Test-Path $xmlPath)) { Fail "manifest WinSW mancante: $xmlPath (servizio '$id' non installato ne' avviabile come processo)" }

  $m = Read-NexusServiceManifest -Path $xmlPath
  $exe = $m.Executable
  $cwd = $m.WorkingDirectory
  $argLine = $m.Arguments

  # Env dal manifest solo per il processo che stiamo per lanciare, poi ripristino.
  # Il tag e' opzionale: per i binari Rust l'elenco e' vuoto per costruzione.
  $saved = @{}
  foreach ($e in @($m.Env)) {
    $saved[$e.Name] = [Environment]::GetEnvironmentVariable($e.Name, 'Process')
    Set-Item -Path "env:$($e.Name)" -Value $e.Value
  }
  if (-not (Test-Path env:RUST_LOG)) {
    $saved['RUST_LOG'] = $null
    Set-Item -Path env:RUST_LOG -Value 'info'
  }

  New-Item -ItemType Directory -Force -Path $LOGDIR | Out-Null
  $out = Join-Path $LOGDIR "$id.out.log"
  $err = Join-Path $LOGDIR "$id.err.log"
  Invoke-DevLogRotation $out
  Invoke-DevLogRotation $err

  $sp = @{
    FilePath               = $exe
    WorkingDirectory       = $cwd
    RedirectStandardOutput = $out
    RedirectStandardError  = $err
    WindowStyle            = 'Hidden'
    PassThru               = $true
  }
  if ($argLine.Trim()) { $sp['ArgumentList'] = $argLine }
  $proc = Start-Process @sp

  foreach ($k in @($saved.Keys)) {
    if ($null -eq $saved[$k]) { Remove-Item -Path "env:$k" -ErrorAction SilentlyContinue }
    else { Set-Item -Path "env:$k" -Value $saved[$k] }
  }
  return $proc.Id
}

# ── Kill del processo del servizio (dal pidfile) ──────────────────────────────
# mcp-core (nexus-mcp-core) viene terminato SENZA /T: nel self-restart questo
# script e' figlio detached di mcp-core e /T (che uccide l'intero albero)
# abbatterebbe anche questo processo prima del rilancio. Gli altri servizi usano
# /T per non lasciare orfani (es. dev-server figli).
#
# Il kill e la VERIFICA che il processo sia davvero morto stanno in
# Stop-NexusProcessTree (lib\nexus-process.ps1). Qui si decide solo cosa fare
# dell'esito, e un kill fallito e' FATALE: exit 1 con il motivo su stderr, che
# mcp-core restituisce come `stderr` dell'endpoint (system_services.rs).
# Il PID resta nel pidfile se il processo e' vivo: rimuoverlo lo renderebbe un
# orfano non piu' rintracciabile, e il pidfile mentirebbe come mentiva l'output.
function Stop-ProcessModel([string]$id) {
  $v = Get-Verdetto $id
  if (-not $v) {
    Write-Info "$id gia' fermo (nessun pid registrato)"
  }
  elseif ($v.Vivo) {
    $res = Stop-NexusProcessTree -ProcessId $v.ProcessId -Label $id -KillTree:($id -ne 'nexus-mcp-core')
    if (-not $res.Stopped) { Fail $res.Message }
    Write-Info $res.Message
  }
  elseif ($v.AutorizzaDichiararloMorto) {
    Write-Info "$id gia' fermo (pid $($v.ProcessId) non e' piu' il nostro processo: $($v.Causa))"
  }
  else {
    # Non si uccide l'albero di un pid che non si e' potuto identificare: e'
    # l'errore nella direzione in cui fa danno invece di bloccare.
    Fail "$id pid $($v.ProcessId): non tocco questo pid, $($v.Dettaglio)"
  }
  $script:voci = Remove-NexusPidEntry -Voci $script:voci -Id $id
  Write-NexusPidFile -Path $PIDFILE -Voci $script:voci
}

function Start-ProcessModel([string]$id) {
  $v = Get-Verdetto $id
  if ($v -and $v.Vivo) {
    Write-Info "$id gia' in esecuzione (pid $($v.ProcessId))"
    return
  }
  if ($v -and -not $v.AutorizzaDichiararloMorto) {
    Fail "$id pid $($v.ProcessId): non avvio un secondo processo, $($v.Dettaglio)"
  }
  $newPid = Start-FromManifest $id
  # La voce nasce dal costruttore unico, che MISURA le prove d'identita' sul
  # processo appena nato. Le altre voci passano intatte.
  $script:voci = Set-NexusPidEntry -Voci $script:voci -Voce (New-NexusPidEntry -Id $id -ProcessId $newPid)
  Write-NexusPidFile -Path $PIDFILE -Voci $script:voci
  Write-Info "avviato $id (pid $newPid)"
}

# ── Servizio Windows (WinSW/SCM) presente? ────────────────────────────────────
$winService = Get-Service -Name $Service -ErrorAction SilentlyContinue

try {
  if ($winService) {
    # Modello WinSW: delega allo SCM.
    switch ($Action) {
      'start' { Start-Service -Name $Service; Write-Info "avviato $Service (Windows Service)" }
      'stop' { Stop-Service -Name $Service -Force; Write-Info "fermato $Service (Windows Service)" }
      'restart' {
        $svc = Get-Service -Name $Service
        if ($svc.Status -eq 'Running') { Restart-Service -Name $Service -Force }
        else { Start-Service -Name $Service }
        Write-Info "riavviato $Service (Windows Service)"
      }
    }
    exit 0
  }

  # Modello a processi (canonico in dev).
  Read-Voci
  switch ($Action) {
    'stop' {
      Stop-ProcessModel $Service
    }
    'start' {
      Start-ProcessModel $Service
    }
    'restart' {
      Stop-ProcessModel $Service
      Start-Sleep -Milliseconds 500
      # Rilettura: fra lo stop e lo start un altro attore puo' aver toccato il
      # file (il watchdog, un altro servizio). Si riparte dai fatti su disco.
      Read-Voci
      Start-ProcessModel $Service
    }
  }
  exit 0
}
catch {
  Fail "azione '$Action' su '$Service' fallita: $($_.Exception.Message)"
}
