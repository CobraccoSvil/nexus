# Punto unico (regola L) della FORMA del pidfile dello stack dev
# (D:\IDEAI-runtime\nexus-dev.pids.json): quali campi porta una voce, CHI li
# annota, come si serializza, come si rilegge e come si completa una voce
# scritta prima di questo contratto. Dot-source da dev-start.ps1 / dev-stop.ps1 /
# dev-service.ps1:
#   . (Join-Path $PSScriptRoot 'lib\nexus-pidfile.ps1')
#
# Il pidfile e' un CONTRATTO fra tre script che non si chiamano fra loro:
# dev-start.ps1 e dev-service.ps1 scrivono, dev-stop.ps1 e la guardia di
# dev-start leggono. Finche' la forma viveva nei chiamanti, i due lati potevano
# divergere in silenzio — e sono divergiti.
#
# L'INCIDENTE CHE LO FA NASCERE (misurato tre volte il 09/08/2026,
# deploy-local.ps1 -Rust fermo con «il pid esiste, ma nulla lo lega a questo
# servizio» x9 e gli eseguibili lockati). Il criterio di vitalita' faceva la cosa
# giusta: senza prove d'identita' un pid e' `non_interrogabile`, e su un ignoto
# non si agisce. Ma le prove non c'erano perche' erano state CANCELLATE a monte:
#
#   dev-start.ps1 annotava `start` (epoch d'avvio, prova FORTE) ed `exe` per ogni
#   processo che lanciava. Poi bastava UNA azione su UN SOLO servizio
#   (dev-service.ps1 -Action start|stop|restart, che mcp-core invoca
#   dall'endpoint /api/system/services/... e che il services_watchdog invoca da
#   solo) perche' `Read-PidMap` riducesse l'intero file a una hashtable
#   `id -> pid` e `Write-PidMap` lo RISCRIVESSE tutto da quella: nove voci, due
#   campi, zero prove. Non la voce toccata: TUTTE.
#
# Misurato sul file vero prima del fix: nove voci `{id, pid}` in ordine di chiave
# hashtable (non l'ordine d'avvio di dev-start), fra cui `nexus-browser-bridge`
# che dev-start non lancia affatto — cioe' la firma esatta di Write-PidMap.
#
# Da qui le tre scelte di progetto di questo modulo:
#
# 1. UNA VOCE NASCE IN UN SOLO POSTO, E NASCE MISURATA. `New-NexusPidEntry` legge
#    l'istante d'avvio e il nome dell'eseguibile DAL SISTEMA OPERATIVO subito
#    dopo lo spawn, quando il processo e' certamente il nostro. Non esiste un
#    altro modo di costruire una voce: chi registra un pid annota le prove nello
#    stesso momento, o non registra nulla.
#
# 2. LA SCRITTURA PROIETTA SUI CAMPI CANONICI. `Write-NexusPidFile` emette sempre
#    e solo `{id, pid, start, exe}`, per ogni voce, qualunque cosa il chiamante
#    le abbia appeso. Un consumatore che tenga in mano una forma ridotta non puo'
#    piu' scriverla: la riduzione muore nel chiamante invece di finire su disco.
#
# 3. LA CHIAVE ASSENTE E LA CHIAVE NULLA SONO CASI DIVERSI (regola Q). `start`
#    presente e nullo significa «l'ho misurato e il SO non ha risposto»; `start`
#    ASSENTE significa «l'ha scritta qualcuno che non conosce questo contratto»,
#    cioe' un file antecedente — oppure una regressione di questa classe. I due
#    non vanno confusi, perche' il ripiego dal manifest (punto 4) assorbe il
#    secondo caso in silenzio: era gia' successo, ed e' il motivo per cui il
#    difetto bloccava dev-stop mentre dev-start passava senza dire nulla.
#
# 4. UN FILE ANTECEDENTE NON BLOCCA PER SEMPRE. `Resolve-NexusPidEntries`
#    recupera la prova DEBOLE che possediamo comunque: il manifest WinSW di
#    quell'id dichiara quale eseguibile quel servizio esegue. E' meno dell'istante
#    d'avvio — identifica un PROGRAMMA, non UNA esecuzione — ma esclude il caso
#    che conta (il pid riassegnato a un estraneo), e il verdetto lo dichiara per
#    iscritto. Un file senza prove NON e' la stessa cosa di un processo non
#    osservabile: le prove, li', sono recuperabili da una fonte che e' nostra.
#
# Il criterio di vitalita' NON sta qui e non e' toccato: sta in nexus-liveness.ps1
# (gemello di crates/mcp-core/src/process_liveness.rs, e per restare gemello non
# deve conoscere il pidfile, che e' solo degli script). Qui sta tutto cio' che
# conosce i NOMI DEI CAMPI — lettura, scrittura, costruzione, completamento e
# l'applicazione del criterio a un intero file — cosi' che una rinomina non possa
# spostare i due lati in tempi diversi.

# Il criterio su UN processo (Get-NexusProcessStartUnix, Get-NexusProcessLiveness).
. (Join-Path $PSScriptRoot 'nexus-liveness.ps1')
# La lettura di un manifest WinSW: fonte della prova debole per i file antecedenti.
. (Join-Path $PSScriptRoot 'nexus-manifest.ps1')

# Valore di una proprieta' che puo' non esserci affatto, distinguendo l'assenza
# della CHIAVE dal valore nullo: sono due fatti diversi (vedi punto 3 in testa).
function Get-NexusPidCampo {
  param(
    [Parameter(Mandatory = $true)][object]$Voce,
    [Parameter(Mandatory = $true)][string]$Nome
  )
  if (-not $Voce.PSObject.Properties[$Nome]) { return $null }
  $v = $Voce.$Nome
  if ($null -eq $v) { return $null }
  if ($v -is [string] -and -not $v.Trim()) { return $null }
  return $v
}

function Test-NexusPidCampoDichiarato {
  param(
    [Parameter(Mandatory = $true)][object]$Voce,
    [Parameter(Mandatory = $true)][string]$Nome
  )
  return [bool]$Voce.PSObject.Properties[$Nome]
}

# L'UNICO costruttore di una voce. Le prove d'identita' si misurano QUI, sul
# processo appena nato: e' il solo istante in cui quel pid e' certamente nostro.
#
# `start` (epoch unix dell'avvio reale) e' il discriminante forte; se il SO non lo
# dichiara resta $null — la chiave c'e' comunque, e chi legge sa che e' stata
# misurata e non omessa. `exe` si MISURA anch'esso e non si copia dal manifest: se
# l'eseguibile lanciato non fosse quello dichiarato, un nome preso dal manifest
# confermerebbe per sempre un'identita' mai verificata.
function New-NexusPidEntry {
  param(
    [Parameter(Mandatory = $true)][string]$Id,
    [Parameter(Mandatory = $true)][int]$ProcessId
  )
  $proc = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
  return [pscustomobject]@{
    id    = $Id
    pid   = $ProcessId
    start = (Get-NexusProcessStartUnix -ProcessId $ProcessId)
    exe   = $(if ($proc) { $proc.ProcessName } else { $null })
  }
}

# Legge il pidfile e ne restituisce SEMPRE le voci come array piatto.
#
# Esiste perche' PS 5.1 ha due trappole opposte su questo file, e ognuna e' gia'
# costata orfani:
#
#  - in SCRITTURA `$x | ConvertTo-Json` con UN solo elemento produce un oggetto,
#    non un array (Write-NexusPidFile lo compensa forzando le parentesi quadre);
#  - in LETTURA `@(Get-Content ... | ConvertFrom-Json)` NON enumera l'array: lo
#    incapsula, e si ottiene UN elemento che contiene i nove. `$v.pid` diventa
#    allora un `Object[]` e ogni conversione a valle fallisce. Misurato sul
#    pidfile vero: nove voci lette come una.
#
# La differenza fra le due forme e' invisibile a rileggere il codice, quindi la
# lettura si fa in UN posto: l'assegnazione a variabile PRIMA di `@()` e' cio'
# che enumera davvero.
function Read-NexusPidFile {
  param([Parameter(Mandatory = $true)][string]$Path)
  if (-not (Test-Path $Path)) { return , @() }
  $raw = Get-Content $Path -Raw
  if (-not $raw -or -not $raw.Trim()) { return , @() }
  $dati = $raw | ConvertFrom-Json
  return , @($dati)
}

# L'UNICA scrittura del pidfile. Proietta ogni voce sui campi canonici: cio' che
# il chiamante ha in mano puo' essere piu' ricco (diagnostica di
# Resolve-NexusPidEntries) o piu' povero (una voce letta da un file antecedente),
# ma su disco la forma e' una sola.
#
# `start` ed `exe` vengono emessi ANCHE quando sono nulli: la chiave presente col
# valore nullo dice «misurato, il SO non ha risposto», e la sua assenza resta
# riservata ai file scritti prima di questo contratto.
#
# Per questo una voce ANTECEDENTE si riscrive antecedente, senza le due chiavi.
# La prova recuperata da Resolve-NexusPidEntries viene dal manifest, non da
# un'osservazione: persisterla la renderebbe indistinguibile da una misura, e
# alla lettura successiva quel file non si dichiarerebbe piu' per quello che e'.
# Basterebbe UNA azione di dev-service.ps1 su un file vecchio per cancellare il
# solo segnale che permette di accorgersi di una regressione di questa classe —
# cioe' lo stesso modo in cui il difetto del 09/08 e' rimasto invisibile.
function Write-NexusPidFile {
  param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][AllowNull()][AllowEmptyCollection()][object[]]$Voci
  )
  $canoniche = @()
  foreach ($v in @($Voci)) {
    if (-not $v) { continue }
    if ($v.PSObject.Properties['Antecedente'] -and $v.Antecedente) {
      $canoniche += [pscustomobject]@{ id = [string]$v.id; pid = [int]$v.pid }
      continue
    }
    $rawStart = Get-NexusPidCampo -Voce $v -Nome 'start'
    $rawExe = Get-NexusPidCampo -Voce $v -Nome 'exe'
    $canoniche += [pscustomobject]@{
      id    = [string]$v.id
      pid   = [int]$v.pid
      start = $(if ($null -ne $rawStart) { [int64]$rawStart } else { $null })
      exe   = $(if ($null -ne $rawExe) { [string]$rawExe } else { $null })
    }
  }
  # PS 5.1: `ConvertTo-Json` con UN solo elemento produce un OGGETTO, non un
  # array -> il lettore itererebbe le PROPRIETA' della voce e lascerebbe orfani.
  $json = $canoniche | ConvertTo-Json -Depth 3
  if ($json -and $json -notmatch '^\s*\[') { $json = "[`n$json`n]" }
  if (-not $json) { $json = '[]' }
  Set-Content -Path $Path -Value $json -Encoding utf8
}

# La voce di un id, o $null. L'identita' di una voce e' l'`id` del servizio, mai
# il pid: il pid e' cio' che il servizio USA in questa esecuzione.
function Get-NexusPidEntry {
  param(
    [AllowNull()][AllowEmptyCollection()][object[]]$Voci,
    [Parameter(Mandatory = $true)][string]$Id
  )
  foreach ($v in @($Voci)) {
    if ($v -and [string]$v.id -eq $Id) { return $v }
  }
  return $null
}

# Sostituisce la voce con lo stesso id, o la aggiunge in coda. Le ALTRE voci
# passano intatte: e' il punto in cui il difetto misurato non e' piu'
# rappresentabile, perche' chi tocca un servizio non ricostruisce il file dalla
# propria vista ridotta.
function Set-NexusPidEntry {
  param(
    [AllowNull()][AllowEmptyCollection()][object[]]$Voci,
    [Parameter(Mandatory = $true)][object]$Voce
  )
  $out = @()
  $sostituita = $false
  foreach ($v in @($Voci)) {
    if (-not $v) { continue }
    if ([string]$v.id -eq [string]$Voce.id) { $out += $Voce; $sostituita = $true }
    else { $out += $v }
  }
  if (-not $sostituita) { $out += $Voce }
  return , $out
}

function Remove-NexusPidEntry {
  param(
    [AllowNull()][AllowEmptyCollection()][object[]]$Voci,
    [Parameter(Mandatory = $true)][string]$Id
  )
  $out = @()
  foreach ($v in @($Voci)) {
    if ($v -and [string]$v.id -ne $Id) { $out += $v }
  }
  return , $out
}

# Completa le voci a cui mancano le prove d'identita', per i pidfile scritti
# prima di questo contratto. NON rilassa il criterio: aggiunge una prova che
# possediamo gia', il manifest WinSW dell'id, che dichiara quale eseguibile quel
# servizio lancia. La prova resta DEBOLE e il verdetto lo dice.
#
# Ogni voce esce con `ProveIdentita` (PascalCase: e' diagnostica in memoria, e
# Write-NexusPidFile la scarta per costruzione):
#   registrate     -> `start` c'era: prova forte, niente da completare
#   dal_manifest   -> mancavano, e il manifest ha dato l'eseguibile atteso
#   assenti        -> nessuna prova recuperabile (manifest mancante o illeggibile)
#
# `antecedente` dice che le CHIAVI mancavano del tutto, cioe' che quel file non
# e' stato scritto da Write-NexusPidFile: e' l'unico modo per accorgersi che il
# ripiego sta assorbendo una regressione invece di un file vecchio.
function Resolve-NexusPidEntries {
  param(
    [Parameter(Mandatory = $true)][AllowNull()][AllowEmptyCollection()][object[]]$Voci,
    [Parameter(Mandatory = $true)][string]$WinswRoot
  )
  $out = @()
  foreach ($v in @($Voci)) {
    if (-not $v -or -not $v.pid) { continue }
    $start = Get-NexusPidCampo -Voce $v -Nome 'start'
    $exe = Get-NexusPidCampo -Voce $v -Nome 'exe'
    $antecedente = -not ((Test-NexusPidCampoDichiarato -Voce $v -Nome 'start') -or
      (Test-NexusPidCampoDichiarato -Voce $v -Nome 'exe'))

    $prove = 'registrate'
    if ($null -eq $start) {
      if ($null -eq $exe) {
        $xml = Join-Path $WinswRoot "$($v.id)\$($v.id).xml"
        if (Test-Path $xml) {
          try { $exe = [IO.Path]::GetFileNameWithoutExtension((Read-NexusServiceManifest -Path $xml).Executable) }
          catch { $exe = $null }
        }
        if ($exe) { $prove = 'dal_manifest' } else { $prove = 'assenti' }
      }
      else { $prove = 'dal_manifest' }
    }

    $out += [pscustomobject]@{
      id            = [string]$v.id
      pid           = [int]$v.pid
      start         = $(if ($null -ne $start) { [int64]$start } else { $null })
      exe           = $(if ($exe) { [string]$exe } else { $null })
      ProveIdentita = $prove
      Antecedente   = $antecedente
    }
  }
  return , $out
}

# Verdetto per ogni voce di un pidfile, con la voce di origine accanto.
# `$Voci` sono le voci deserializzate (o completate da Resolve-NexusPidEntries).
# Sta qui e non nel criterio perche' e' l'unico punto che traduce i CAMPI del
# registro nei termini del criterio: `start` -> istante atteso, `exe` -> nome
# atteso. Con quella traduzione altrove, una rinomina dei campi spegnerebbe il
# discriminante senza che nulla fallisse.
function Get-NexusStackLiveness {
  param([Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$Voci)
  $out = @()
  foreach ($v in @($Voci)) {
    if (-not $v -or -not $v.pid) { continue }
    $rawStart = Get-NexusPidCampo -Voce $v -Nome 'start'
    $atteso = if ($null -ne $rawStart) { [int64]$rawStart } else { $null }
    $exe = Get-NexusPidCampo -Voce $v -Nome 'exe'
    $verdetto = Get-NexusProcessLiveness -ProcessId ([int]$v.pid) -ExpectedStartUnix $atteso `
      -ExpectedName ([string]$exe)
    $out += [pscustomobject]@{
      Id                        = [string]$v.id
      ProcessId                 = [int]$v.pid
      Stato                     = $verdetto.Stato
      Causa                     = $verdetto.Causa
      Dettaglio                 = $verdetto.Dettaglio
      Vivo                      = $verdetto.Vivo
      AutorizzaDichiararloMorto = $verdetto.AutorizzaDichiararloMorto
    }
  }
  return , $out
}
