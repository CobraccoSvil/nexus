# Ferma lo stack Nexus avviato da dev-start.ps1 (processi, non servizi Windows).
# Legge i PID salvati e termina ogni processo con il suo albero (/T): i figli
# eventualmente spawnati da mcp-core (dev-server di progetto) non restano orfani a
# tenere le porte. NON tocca i database (servizi Windows separati).
#
# Ogni kill passa da Stop-NexusProcessTree (lib\nexus-process.ps1), che VERIFICA
# che il processo sia davvero morto invece di dichiararlo. Un processo che
# sopravvive NON viene piu' annunciato come "fermato": l'errore e' esplicito e lo
# script esce !=0, cosi' i chiamanti (dev-build.ps1) non compilano contro .exe
# ancora lockati (os error 5). Vedi lib\nexus-process.ps1 per il razionale.
$ErrorActionPreference = 'Stop'
$RUNTIME = 'D:\IDEAI-runtime'
$PIDFILE = Join-Path $RUNTIME 'nexus-dev.pids.json'

. (Join-Path $PSScriptRoot 'lib\nexus-process.ps1')

# Processi sopravvissuti al kill: raccolti qui e riportati insieme in coda, cosi'
# un fallimento non impedisce di tentare gli altri servizi.
$survivors = @()

function Invoke-Kill([int]$processId, [string]$label, [string]$how) {
  $res = Stop-NexusProcessTree -ProcessId $processId -Label $label -KillTree
  if ($res.AlreadyStopped) { return }
  if ($res.Stopped) {
    Write-Host ("fermato{0} {1,-16} pid {2}" -f $how, $label, $processId) -ForegroundColor Yellow
  }
  else {
    Write-Host $res.Message -ForegroundColor Red
    $script:survivors += $res
  }
}

# 1) Ferma i PID registrati nel pidfile (albero /T), se presente. `@($procs)` tollera
# sia un array sia un oggetto singolo (pidfile serializzato a un elemento).
if (Test-Path $PIDFILE) {
  try {
    $procs = Get-Content $PIDFILE -Raw | ConvertFrom-Json
    foreach ($p in @($procs)) {
      if ($p.pid) { Invoke-Kill ([int]$p.pid) ([string]$p.id) '' }
    }
  }
  catch { Write-Warning "pidfile illeggibile ($($_.Exception.Message)); procedo col fallback per nome." }
  # Il pidfile va via solo se NESSUN processo e' sopravvissuto: cancellarlo mentre
  # qualcosa e' ancora vivo perde l'unica traccia del PID (orfano non rintracciabile)
  # e fa credere a dev-start.ps1 che lo stack sia giu'.
  if ($survivors.Count -eq 0) { Remove-Item $PIDFILE -Force -ErrorAction SilentlyContinue }
  else { Write-Warning "$PIDFILE NON rimosso: contiene processi ancora vivi." }
}

# 2) SEMPRE fallback per nome dei processi noti ancora vivi. Il pidfile puo' essere
# incompleto/corrotto (serializzazione a elemento singolo -> restano orfani) o stale:
# senza questo pass gli orfani lockano i .exe al successivo `cargo build`. web-ide
# (node.exe) resta ESCLUSO: killare per nome ucciderebbe altri Node non correlati
# (viene fermato sopra tramite il pidfile).
$names = 'mcp-core', 'nexus-gateway', 'admin-service',
'doc-service', 'plugin-service', 'qdrant', 'GarnetServer'
foreach ($n in $names) {
  Get-Process -Name $n -ErrorAction SilentlyContinue | ForEach-Object {
    Invoke-Kill $_.Id $n ' (per nome)'
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
    Invoke-Kill $op.Id 'web-ide' (" (per porta :{0})" -f $webPort)
  }
}

if ($survivors.Count -gt 0) {
  Write-Host ''
  [Console]::Error.WriteLine("Stack NON fermato: $($survivors.Count) processo/i sopravvissuto/i al kill (vedi sopra). Non compilare: gli eseguibili sono lockati.")
  exit 1
}
Write-Host 'Stack fermato.' -ForegroundColor Cyan
exit 0
