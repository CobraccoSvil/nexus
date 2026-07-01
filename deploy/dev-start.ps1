# Avvia lo stack Nexus come PROCESSI (non servizi Windows), per il test locale.
# Legge i manifest WinSW in D:\IDEAI-runtime\winsw\<id>\<id>.xml come FONTE UNICA di
# eseguibile / working dir / env: niente duplicazione di comandi o segreti nel repo.
# Log per-servizio in D:\IDEAI-runtime\dev-logs\. I PID vengono salvati per dev-stop.ps1.
# NON richiede admin: lanciare processi non e' un'operazione privilegiata.
#
# I database restano servizi Windows separati (postgresql-x64-17, nexus-pg-nexus,
# nexus-pg-app) e NON sono gestiti qui: i dati devono persistere fra un test e l'altro.
$ErrorActionPreference = 'Stop'
$RUNTIME = 'D:\IDEAI-runtime'
$WINSW   = Join-Path $RUNTIME 'winsw'
$LOGDIR  = Join-Path $RUNTIME 'dev-logs'
$PIDFILE = Join-Path $RUNTIME 'nexus-dev.pids.json'

# Ordine di avvio: infra dati -> mcp-core (attende 5s) -> altri Rust -> web-ide.
$order = @(
  'nexus-qdrant', 'nexus-garnet',
  'nexus-mcp-core',
  'nexus-gateway', 'nexus-admin', 'nexus-billing', 'nexus-doc', 'nexus-plugin', 'nexus-chat',
  'nexus-web-ide'
)

# Prerequisito: i database restano servizi Windows. Avvisa (non li avvia questo script).
foreach ($pg in 'postgresql-x64-17', 'nexus-pg-nexus', 'nexus-pg-app') {
  $svc = Get-Service $pg -ErrorAction SilentlyContinue
  if ($svc -and $svc.Status -ne 'Running') {
    Write-Warning "$pg non e' in esecuzione: gli applicativi potrebbero non partire. Avvia con: Start-Service $pg"
  }
}

# Evita doppioni se uno stack e' gia' attivo.
if (Test-Path $PIDFILE) {
  Write-Warning "Trovato ${PIDFILE}: uno stack potrebbe essere gia' attivo. Esegui prima .\deploy\dev-stop.ps1. Interrompo."
  return
}

New-Item -ItemType Directory -Force -Path $LOGDIR | Out-Null
$started = @()

foreach ($id in $order) {
  $xmlPath = Join-Path $WINSW "$id\$id.xml"
  if (-not (Test-Path $xmlPath)) { Write-Warning "${id}: manifest mancante ($xmlPath), salto."; continue }
  try {
    [xml]$x = Get-Content $xmlPath -Raw
    $s = $x.service
    $exe = $s.executable
    $cwd = $s.workingdirectory
    $argLine = if ($s.arguments) { [string]$s.arguments } else { '' }

    # Env dal manifest: valorizzata per web-ide, vuota per i binari Rust (che leggono
    # il .env dalla working dir). Impostata solo per il processo che stiamo per lanciare
    # e ripristinata subito dopo, per non inquinare i servizi successivi.
    $svcEnv = @($s.env | Where-Object { $_ })
    $saved = @{}
    foreach ($e in $svcEnv) {
      $saved[$e.name] = [Environment]::GetEnvironmentVariable($e.name, 'Process')
      Set-Item -Path "env:$($e.name)" -Value $e.value
    }

    $out = Join-Path $LOGDIR "$id.out.log"
    $err = Join-Path $LOGDIR "$id.err.log"
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

    $started += [pscustomobject]@{ id = $id; pid = $proc.Id }
    Write-Host ("avviato {0,-16} pid {1}" -f $id, $proc.Id) -ForegroundColor Green

    if ($id -eq 'nexus-mcp-core') { Start-Sleep -Seconds 5 } else { Start-Sleep -Milliseconds 600 }
  }
  catch {
    Write-Warning "${id}: avvio fallito - $($_.Exception.Message)"
  }
}

$started | ConvertTo-Json | Set-Content -Path $PIDFILE -Encoding utf8
Write-Host ''
Write-Host "Stack avviato ($($started.Count) processi). PID: $PIDFILE" -ForegroundColor Cyan
Write-Host "Log: $LOGDIR   |   Stop: .\deploy\dev-stop.ps1" -ForegroundColor Cyan
