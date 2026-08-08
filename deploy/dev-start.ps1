# Avvia lo stack Nexus come PROCESSI (non servizi Windows), per il test locale.
# Legge i manifest WinSW in D:\IDEAI-runtime\winsw\<id>\<id>.xml come FONTE UNICA di
# eseguibile / working dir / env: niente duplicazione di comandi o segreti nel repo.
# Log per-servizio in D:\IDEAI-runtime\dev-logs\. I PID vengono salvati per dev-stop.ps1.
# NON richiede admin: lanciare processi non e' un'operazione privilegiata.
#
# Log: Start-Process TRONCA i file di redirect a ogni avvio, quindi PRIMA di
# lanciare ogni servizio i log della sessione precedente vengono ruotati in
# <nome>.log.1 / .2 / .3 (3 generazioni). Senza rotazione un riavvio cancellava
# la sessione precedente e l'errore da diagnosticare con essa (incidente
# 2026-07-06: PROVIDER_ERROR delle 16:30 perso dal restart delle 18:09).
#
# NB dimensione "0 KB": finche' un processo tiene aperto l'handle del proprio
# log, la directory entry NTFS non viene aggiornata ed Explorer/Get-ChildItem
# mostrano 0 byte anche se il file si sta riempiendo. Il contenuto reale si
# legge con: Get-Content <file> -Tail 50 [-Wait].
#
# I database restano servizi Windows separati (postgresql-x64-17, nexus-pg-nexus,
# nexus-pg-app) e NON sono gestiti qui: i dati devono persistere fra un test e l'altro.
$ErrorActionPreference = 'Stop'
$RUNTIME = 'D:\IDEAI-runtime'
$WINSW   = Join-Path $RUNTIME 'winsw'
$LOGDIR  = Join-Path $RUNTIME 'dev-logs'
$PIDFILE = Join-Path $RUNTIME 'nexus-dev.pids.json'

# Lettura dei manifest: punto unico condiviso con dev-service.ps1/nexus-publish.ps1.
. (Join-Path $PSScriptRoot 'lib\nexus-manifest.ps1')
# «Questo processo registrato e' vivo?»: punto unico condiviso con dev-stop.ps1.
. (Join-Path $PSScriptRoot 'lib\nexus-liveness.ps1')

# Ordine di avvio: infra dati -> mcp-core (attende 5s) -> altri Rust -> web-ide.
$order = @(
  'nexus-qdrant', 'nexus-garnet',
  'nexus-mcp-core',
  'nexus-gateway', 'nexus-admin', 'nexus-doc', 'nexus-plugin',
  'nexus-web-ide'
)

# Prerequisito: i database restano servizi Windows. Avvisa (non li avvia questo script).
foreach ($pg in 'postgresql-x64-17', 'nexus-pg-nexus', 'nexus-pg-app') {
  $svc = Get-Service $pg -ErrorAction SilentlyContinue
  if ($svc -and $svc.Status -ne 'Running') {
    Write-Warning "$pg non e' in esecuzione: gli applicativi potrebbero non partire. Avvia con: Start-Service $pg"
  }
}

# Evita doppioni se uno stack e' gia' attivo — CHIEDENDOLO AL SISTEMA OPERATIVO,
# non al filesystem (incidente 2026-08-08).
#
# Prima qui bastava `Test-Path $PIDFILE`: l'esistenza di un file era trattata
# come la prova che nove processi stessero girando. L'08/08/2026 erano morti
# tutti e nove insieme — sono figli di una console, e quando quella termina se ne
# vanno insieme — e il file li elencava ancora tutti: lo stack non ripartiva, e
# l'unico rimedio era eseguire dev-stop.ps1 per fermare cio' che non c'era.
#
# Il pidfile resta la fonte di CHI cercare; l'esistenza la dice il SO. E i tre
# esiti vogliono tre condotte diverse: qualcuno vivo -> non si parte (era il caso
# per cui la guardia esiste); tutti morti -> il file e' un residuo, si dice e si
# procede; qualcuno non interrogabile -> non si decide da soli, perche' agire
# sull'ignoto e' il difetto nell'altra direzione.
if (Test-Path $PIDFILE) {
  $voci = @()
  try { $voci = Read-NexusPidFile -Path $PIDFILE }
  catch {
    Write-Warning "${PIDFILE} illeggibile ($($_.Exception.Message)): non posso sapere cosa c'e' in giro. Esegui .\deploy\dev-stop.ps1 e rilancia. Interrompo."
    return
  }
  # I pidfile scritti prima di questo criterio non portano ne' `start` ne' `exe`,
  # e senza almeno una prova ogni pid resterebbe «non interrogabile» — cioe' lo
  # stack bloccato come prima, che e' proprio il difetto da chiudere. L'exe
  # atteso pero' lo sappiamo comunque: e' quello che il manifest dichiara per
  # quell'id, e il manifest si legge dal suo punto unico.
  $voci = @($voci | ForEach-Object {
      $v = $_
      if ($v.PSObject.Properties['exe'] -and $v.exe) { return $v }
      $xml = Join-Path $WINSW "$($v.id)\$($v.id).xml"
      $atteso = $null
      if (Test-Path $xml) {
        try { $atteso = [IO.Path]::GetFileNameWithoutExtension((Read-NexusServiceManifest -Path $xml).Executable) }
        catch { $atteso = $null }
      }
      [pscustomobject]@{
        id    = $v.id
        pid   = $v.pid
        start = (& { if ($v.PSObject.Properties['start']) { $v.start } else { $null } })
        exe   = $atteso
      }
    })
  $stato = Get-NexusStackLiveness -Voci $voci
  $vivi = @($stato | Where-Object { $_.Vivo })
  $ignoti = @($stato | Where-Object { -not $_.Vivo -and -not $_.AutorizzaDichiararloMorto })

  if ($vivi.Count -gt 0) {
    Write-Warning "Stack gia' ATTIVO: $($vivi.Count) processo/i vivo/i ($(($vivi | ForEach-Object { "$($_.Id) pid $($_.ProcessId)" }) -join ', ')). Esegui prima .\deploy\dev-stop.ps1. Interrompo."
    return
  }
  if ($ignoti.Count -gt 0) {
    Write-Warning "Stato non accertabile per $($ignoti.Count) processo/i del pidfile:"
    $ignoti | ForEach-Object { Write-Warning "  $($_.Id) pid $($_.ProcessId): $($_.Dettaglio)" }
    Write-Warning "Non riavvio da solo: un doppio stack e' peggio di uno fermo. Verifica con Get-Process, poi esegui .\deploy\dev-stop.ps1 (da shell ELEVATA se i processi sono elevati). Interrompo."
    return
  }
  Write-Host "Pidfile residuo: tutti e $($stato.Count) i processi registrati sono morti, lo rimuovo e proseguo." -ForegroundColor DarkGray
  Remove-Item $PIDFILE -Force -ErrorAction SilentlyContinue
}

New-Item -ItemType Directory -Force -Path $LOGDIR | Out-Null

# Ruota i log della sessione precedente: <file>.log -> .log.1 -> .log.2 -> .log.3.
# I file vuoti non vengono ruotati (Start-Process li ricrea comunque). Se un
# handle e' ancora aperto (processo orfano) il Move-Item fallisce: warning e si
# prosegue col troncamento, senza bloccare l'avvio dello stack.
$LOG_GENERATIONS = 3
function Invoke-DevLogRotation([string]$path) {
  if (-not (Test-Path $path)) { return }
  if ((Get-Item $path).Length -eq 0) { return }
  # 2 tentativi con pausa: subito dopo dev-stop l'handle puo' essere ancora in
  # rilascio (o lo scanner AV sta leggendo il file appena chiuso).
  foreach ($attempt in 1, 2) {
    try {
      for ($i = $LOG_GENERATIONS - 1; $i -ge 1; $i--) {
        $src = "$path.$i"
        if (Test-Path $src) { Move-Item -Force $src "$path.$($i + 1)" }
      }
      Move-Item -Force $path "$path.1"
      return
    }
    catch {
      if ($attempt -eq 1) { Start-Sleep -Milliseconds 500; continue }
      Write-Warning "rotazione log fallita per ${path}: $($_.Exception.Message) (handle ancora aperto? il file verra' troncato)"
    }
  }
}

$started = @()

foreach ($id in $order) {
  $xmlPath = Join-Path $WINSW "$id\$id.xml"
  if (-not (Test-Path $xmlPath)) { Write-Warning "${id}: manifest mancante ($xmlPath), salto."; continue }
  try {
    $m = Read-NexusServiceManifest -Path $xmlPath
    $exe = $m.Executable
    $cwd = $m.WorkingDirectory
    $argLine = $m.Arguments

    # Env dal manifest: valorizzata per web-ide, ASSENTE per i binari Rust (che
    # leggono il .env dalla working dir) — il tag e' opzionale e il generatore lo
    # omette, quindi qui l'elenco vuoto e' il caso normale, non un manifest monco.
    # Impostata solo per il processo che stiamo per lanciare e ripristinata subito
    # dopo, per non inquinare i servizi successivi.
    $saved = @{}
    foreach ($e in @($m.Env)) {
      $saved[$e.Name] = [Environment]::GetEnvironmentVariable($e.Name, 'Process')
      Set-Item -Path "env:$($e.Name)" -Value $e.Value
    }

    # RUST_LOG per il tracing dei binari Rust: i processi ereditano l'ambiente
    # di questa shell e dotenvy NON sovrascrive variabili gia' presenti. Se ne'
    # l'operatore ne' il manifest l'hanno impostata, propaga il default 'info'
    # cosi' i log restano attivi anche se il .env della working dir ne e' privo.
    if (-not (Test-Path env:RUST_LOG)) {
      $saved['RUST_LOG'] = $null
      Set-Item -Path env:RUST_LOG -Value 'info'
    }

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

    # `start` (epoch unix dell'avvio REALE, letto dal SO) e' il discriminante
    # d'identita' del pid: senza, alla lettura successiva non c'e' modo di dire
    # se quel numero e' ancora questo processo o un estraneo che ne ha ereditato
    # il pid. Si legge qui, dove il processo e' appena nato e certamente il
    # nostro. Se il SO non lo dichiara resta assente, e chi legge lo trattera'
    # per quello che e': non accertabile, mai «vivo» d'ufficio.
    $started += [pscustomobject]@{
      id    = $id
      pid   = $proc.Id
      start = (Get-NexusProcessStartUnix -ProcessId $proc.Id)
      # Il nome si MISURA sul processo nato, non si deduce dal manifest: se
      # l'eseguibile lanciato non fosse quello dichiarato, un `exe` copiato dal
      # manifest confermerebbe per sempre un'identita' mai verificata.
      exe   = $proc.ProcessName
    }
    Write-Host ("avviato {0,-16} pid {1}" -f $id, $proc.Id) -ForegroundColor Green

    if ($id -eq 'nexus-mcp-core') { Start-Sleep -Seconds 5 } else { Start-Sleep -Milliseconds 600 }
  }
  catch {
    Write-Warning "${id}: avvio fallito - $($_.Exception.Message)"
  }
}

# Serializza SEMPRE come array JSON: in PS 5.1 `$started | ConvertTo-Json` con 1 solo
# elemento produce un OGGETTO singolo, non un array -> dev-stop iterava male e lasciava
# orfani. Forziamo le parentesi quadre se ConvertTo-Json le ha omesse.
$json = $started | ConvertTo-Json -Depth 3
if ($json -and $json -notmatch '^\s*\[') { $json = "[`n$json`n]" }
if (-not $json) { $json = '[]' }
Set-Content -Path $PIDFILE -Value $json -Encoding utf8
Write-Host ''
if ($started.Count -lt $order.Count) {
  Write-Warning "Avviati $($started.Count)/$($order.Count) processi: alcuni servizi non sono partiti (vedi warning sopra e i log in $LOGDIR)."
}
Write-Host "Stack avviato ($($started.Count) processi). PID: $PIDFILE" -ForegroundColor Cyan
Write-Host "Log: $LOGDIR   |   Stop: .\deploy\dev-stop.ps1" -ForegroundColor Cyan
Write-Host "NB: Explorer mostra 0 KB finche' il processo tiene aperto l'handle del log (size NTFS pigra)." -ForegroundColor DarkGray
Write-Host "    Leggere con: Get-Content $LOGDIR\<servizio>.out.log -Tail 50 [-Wait]" -ForegroundColor DarkGray

# ESITO DICHIARATO, non dedotto dal codice d'uscita.
#
# Questo script non chiama mai `exit`: i fallimenti per-servizio sono degradati a
# Write-Warning e il ramo "stack gia' attivo" fa `return`. In PowerShell, `&` su
# un .ps1 che non chiama `exit` NON aggiorna $LASTEXITCODE — il chiamante legge
# il valore dell'ULTIMO comando nativo precedente, cioe' un residuo di `cargo`.
# deploy-local.ps1 lo leggeva per decidere fra «stack riavviato» e «i servizi
# sono GIU'»: una diagnosi tirata a sorte, che su uno stack a pezzi diceva
# «riavviato» perche' l'ultima cargo era andata bene.
#
# Il canale giusto e' un VALORE: chi vuole sapere com'e' andata interroga i
# campi, come deploy-local.ps1 fa gia' con Publish-NexusArtifacts. Non si
# aggiunge un `exit`: un exit code e' un numero che il chiamante puo' ignorare
# senza che nulla lo fermi, e qui il chiamante lo ignorava gia' credendo di
# leggerlo.
[pscustomobject]@{
  Attesi  = $order.Count
  Avviati = $started.Count
  Completo = ($started.Count -ge $order.Count)
  PidFile = $PIDFILE
  LogDir  = $LOGDIR
}
