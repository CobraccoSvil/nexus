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

function Write-Info([string]$msg) { Write-Output $msg }
function Fail([string]$msg) { [Console]::Error.WriteLine($msg); exit 1 }

# ── Pidfile: array JSON di {id,pid}, stessa fonte di dev-start/dev-stop ────────
function Read-PidMap {
  $map = @{}
  if (Test-Path $PIDFILE) {
    try {
      $procs = Get-Content $PIDFILE -Raw | ConvertFrom-Json
      foreach ($p in @($procs)) { if ($p.id) { $map[[string]$p.id] = [int]$p.pid } }
    }
    # Write-Warning (stream 3), NON Write-Output: qui il valore di ritorno e'
    # $map e un messaggio su stdout lo trasformerebbe in un array.
    catch { Write-Warning "pidfile illeggibile ($($_.Exception.Message)); procedo senza." }
  }
  return $map
}

function Write-PidMap([hashtable]$map) {
  $arr = @()
  foreach ($k in $map.Keys) { $arr += [pscustomobject]@{ id = $k; pid = $map[$k] } }
  # PS 5.1: ConvertTo-Json con 1 solo elemento produce un OGGETTO, non un array.
  # Forziamo le parentesi quadre come fa dev-start.ps1 (altrimenti dev-stop itera male).
  $json = $arr | ConvertTo-Json -Depth 3
  if ($json -and $json -notmatch '^\s*\[') { $json = "[`n$json`n]" }
  if (-not $json) { $json = '[]' }
  Set-Content -Path $PIDFILE -Value $json -Encoding utf8
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
function Stop-ProcessModel([string]$id, [hashtable]$map) {
  $processId = $map[$id]
  if (-not $processId) {
    Write-Info "$id gia' fermo (nessun pid registrato)"
  }
  else {
    $res = Stop-NexusProcessTree -ProcessId $processId -Label $id -KillTree:($id -ne 'nexus-mcp-core')
    if (-not $res.Stopped) { Fail $res.Message }
    Write-Info $res.Message
  }
  $map.Remove($id) | Out-Null
  Write-PidMap $map
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
  $map = Read-PidMap
  switch ($Action) {
    'stop' {
      Stop-ProcessModel $Service $map
    }
    'start' {
      if (Test-NexusProcessAlive $map[$Service]) {
        Write-Info "$Service gia' in esecuzione (pid $($map[$Service]))"
      }
      else {
        $newPid = Start-FromManifest $Service
        $map[$Service] = $newPid
        Write-PidMap $map
        Write-Info "avviato $Service (pid $newPid)"
      }
    }
    'restart' {
      Stop-ProcessModel $Service $map
      Start-Sleep -Milliseconds 500
      $map = Read-PidMap
      $newPid = Start-FromManifest $Service
      $map[$Service] = $newPid
      Write-PidMap $map
      Write-Info "riavviato $Service (pid $newPid)"
    }
  }
  exit 0
}
catch {
  Fail "azione '$Action' su '$Service' fallita: $($_.Exception.Message)"
}
