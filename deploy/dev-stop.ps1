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
$WINSW   = Join-Path $RUNTIME 'winsw'
$PIDFILE = Join-Path $RUNTIME 'nexus-dev.pids.json'

. (Join-Path $PSScriptRoot 'lib\nexus-process.ps1')
# FORMA del pidfile + criterio di vitalita' che si porta dietro.
. (Join-Path $PSScriptRoot 'lib\nexus-pidfile.ps1')

# Processi sopravvissuti al kill: raccolti qui e riportati insieme in coda, cosi'
# un fallimento non impedisce di tentare gli altri servizi.
$survivors = @()
# Voci del pidfile su cui NON si e' tentato nulla perche' non si e' potuto
# accertare di chi fosse il pid. Tengono vivo il pidfile ed escludono l'uscita 0
# esattamente come i sopravvissuti: in entrambi i casi non sappiamo se un .exe
# resta lockato, e dev-build.ps1 non deve compilarci contro.
$nonAccertati = @()

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
    $procs = Read-NexusPidFile -Path $PIDFILE
    # Le prove d'identita' mancanti si completano dal manifest PRIMA di giudicare,
    # come fa la guardia di dev-start: e' lo stesso file, e due letture con due
    # corredi di prove diversi davano due verdetti diversi sugli stessi nove pid.
    # Senza questa riga, un pidfile scritto da un produttore che non le annota
    # rendeva OGNI voce «non interrogabile», questo script usciva 1 e il deploy
    # si fermava con gli eseguibili lockati (misurato tre volte il 09/08/2026).
    $procs = Resolve-NexusPidEntries -Voci @($procs) -WinswRoot $WINSW
    $antecedenti = @($procs | Where-Object { $_.Antecedente })
    if ($antecedenti.Count -gt 0) {
      Write-Host "Pidfile senza prove d'identita' per $($antecedenti.Count) voce/i (file antecedente al contratto): completate dai manifest WinSW." -ForegroundColor DarkGray
    }
    # Il pid nel file e' un NUMERO, non un'identita': prima di `taskkill /T /F`
    # si verifica che sia ancora il nostro processo. Senza questa domanda un pid
    # riciclato dal SO fa abbattere l'albero di un estraneo — e' la stessa
    # ragione per cui dev-start non si fida piu' del solo file, vista dal lato
    # in cui l'errore fa danno invece di bloccare.
    foreach ($v in (Get-NexusStackLiveness -Voci @($procs))) {
      if ($v.Vivo) {
        Invoke-Kill $v.ProcessId $v.Id ''
        continue
      }
      if ($v.AutorizzaDichiararloMorto) {
        # Gia' fermo (o pid riciclato: il nostro processo non c'e' piu').
        continue
      }
      Write-Warning "$($v.Id) pid $($v.ProcessId): NON tocco questo pid, $($v.Dettaglio)"
      $nonAccertati += $v
    }
  }
  catch { Write-Warning "pidfile illeggibile ($($_.Exception.Message)); procedo col fallback per nome." }
  # Il pidfile va via solo se non resta nulla di aperto: cancellarlo mentre
  # qualcosa e' ancora vivo perde l'unica traccia del PID (orfano non rintracciabile)
  # e fa credere a dev-start.ps1 che lo stack sia giu'.
  if ($survivors.Count -eq 0 -and $nonAccertati.Count -eq 0) {
    Remove-Item $PIDFILE -Force -ErrorAction SilentlyContinue
  }
  elseif ($survivors.Count -gt 0) { Write-Warning "$PIDFILE NON rimosso: contiene processi ancora vivi." }
  else { Write-Warning "$PIDFILE NON rimosso: contiene pid di cui non si e' potuto accertare lo stato." }
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
if ($nonAccertati.Count -gt 0) {
  Write-Host ''
  [Console]::Error.WriteLine("Stack NON dichiarato fermo: $($nonAccertati.Count) pid del pidfile non erano accertabili e non sono stati toccati (vedi sopra). Non compilare: non sappiamo se quegli eseguibili sono ancora in uso.")
  exit 1
}
Write-Host 'Stack fermato.' -ForegroundColor Cyan
exit 0
