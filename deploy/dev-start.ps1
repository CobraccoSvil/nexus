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

# Evita doppioni se uno stack e' gia' attivo.
if (Test-Path $PIDFILE) {
  Write-Warning "Trovato ${PIDFILE}: uno stack potrebbe essere gia' attivo. Esegui prima .\deploy\dev-stop.ps1. Interrompo."
  return
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

    $started += [pscustomobject]@{ id = $id; pid = $proc.Id }
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
