# Ferma lo stack Nexus avviato da dev-start.ps1 (processi, non servizi Windows).
# Legge i PID salvati e termina ogni processo con il suo albero (/T): i figli
# eventualmente spawnati da mcp-core (dev-server di progetto) non restano orfani a
# tenere le porte. NON tocca i database (servizi Windows separati).
$ErrorActionPreference = 'Stop'
$RUNTIME = 'D:\IDEAI-runtime'
$PIDFILE = Join-Path $RUNTIME 'nexus-dev.pids.json'

# 1) Ferma i PID registrati nel pidfile (albero /T), se presente. `@($procs)` tollera
# sia un array sia un oggetto singolo (pidfile serializzato a un elemento).
if (Test-Path $PIDFILE) {
  try {
    $procs = Get-Content $PIDFILE -Raw | ConvertFrom-Json
    foreach ($p in @($procs)) {
      if ($p.pid -and (Get-Process -Id $p.pid -ErrorAction SilentlyContinue)) {
        # taskkill via cmd /c: in PS 5.1 il redirect di stderr di un exe nativo genera
        # un NativeCommandError; dentro cmd lo stderr non risale a PowerShell.
        cmd /c "taskkill /PID $($p.pid) /T /F >nul 2>nul"
        Write-Host ("fermato {0,-16} pid {1}" -f $p.id, $p.pid) -ForegroundColor Yellow
      }
    }
  }
  catch { Write-Warning "pidfile illeggibile ($($_.Exception.Message)); procedo col fallback per nome." }
  Remove-Item $PIDFILE -Force -ErrorAction SilentlyContinue
}

# 2) SEMPRE fallback per nome dei processi noti ancora vivi. Il pidfile puo' essere
# incompleto/corrotto (serializzazione a elemento singolo -> restano orfani) o stale:
# senza questo pass gli orfani lockano i .exe al successivo `cargo build`. web-ide
# (node.exe) resta ESCLUSO: killare per nome ucciderebbe altri Node non correlati
# (viene fermato sopra tramite il pidfile).
$names = 'mcp-core', 'nexus-gateway', 'admin-service', 'billing-service',
'doc-service', 'plugin-service', 'chat-service', 'qdrant', 'GarnetServer'
foreach ($n in $names) {
  Get-Process -Name $n -ErrorAction SilentlyContinue | ForEach-Object {
    cmd /c "taskkill /PID $($_.Id) /T /F >nul 2>nul"
    Write-Host ("fermato (per nome) {0,-16} pid {1}" -f $n, $_.Id) -ForegroundColor Yellow
  }
}

# 3) web-ide (node) per PROPRIETARIO DELLA PORTA (fonte di verita' reale, regola H).
# Il PIDFILE drifta: un `node server.js` orfano (spesso elevato) puo' restare
# proprietario di :3000 servendo un build vecchio (400/ChunkLoadError sui chunk). Non
# possiamo killare node per nome (ucciderebbe altri Node non correlati), quindi
# risolviamo l'owner della porta, univoco. Porta override via NEXUS_WEBIDE_PORT.
$webPort = if ($env:NEXUS_WEBIDE_PORT) { [int]$env:NEXUS_WEBIDE_PORT } else { 3000 }
Get-NetTCPConnection -LocalPort $webPort -State Listen -ErrorAction SilentlyContinue |
Select-Object -ExpandProperty OwningProcess -Unique | ForEach-Object {
  $op = Get-Process -Id $_ -ErrorAction SilentlyContinue
  if ($op -and $op.ProcessName -eq 'node') {
    cmd /c "taskkill /PID $($op.Id) /T /F >nul 2>nul"
    Write-Host ("fermato (per porta :{0}) web-ide pid {1}" -f $webPort, $op.Id) -ForegroundColor Yellow
  }
}
Write-Host 'Stack fermato.' -ForegroundColor Cyan
