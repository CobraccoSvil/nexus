# Punto unico (regola L) PowerShell della domanda «questo processo REGISTRATO e'
# ancora vivo?». Dot-source da dev-start.ps1 / dev-stop.ps1 / nexus-process.ps1:
#   . (Join-Path $PSScriptRoot 'lib\nexus-liveness.ps1')
#
# Gemello di crates/mcp-core/src/process_liveness.rs: stesso criterio, stesso
# vocabolario. Sono due implementazioni perche' i due lati parlano linguaggi
# diversi e non possono chiamarsi; NON sono due criteri. Chi ne cambia uno
# cambia l'altro, o il registro Rust e il pidfile degli script torneranno a dare
# risposte diverse sullo stesso processo — che e' il difetto da cui nascono
# entrambi.
#
# PERCHE' NON BASTA `Get-Process -Id` (incidente 2026-08-08). dev-start.ps1 si
# rifiutava di partire con «Trovato nexus-dev.pids.json: uno stack potrebbe
# essere gia' attivo» mentre TUTTI E NOVE i processi elencati nel file erano
# morti: nessuno aveva mai chiesto al sistema operativo se fossero ancora li'.
# Bastava l'ESISTENZA DEL FILE. Il rimedio manuale era eseguire dev-stop.ps1,
# cioe' fermare uno stack che non c'era.
#
# E non basta nemmeno l'esistenza del pid: i pid si riciclano. Il criterio e'
# «questo pid esiste ancora, ED E' il processo che credo?», e la seconda meta'
# vuole un discriminante — qui l'istante di AVVIO, che dev-start.ps1 annota nel
# pidfile subito dopo lo spawn (campo `start`, epoch unix).
#
# TRE ESITI, NON DUE (regola Q). `non_interrogabile` esiste perche' «non ho
# potuto guardare» non degradi ne' a vivo ne' a morto: `Get-Process` non vede i
# processi di altri utenti, e leggere lo `StartTime` di un processo elevato da
# una shell non elevata solleva «Accesso negato». Da li' nasce la direzione
# opposta del difetto — dichiarare morto cio' che sta girando — e i due predicati
# che ne derivano NON sono l'uno la negazione dell'altro:
#
#   .Vivo                       -> posso trattarlo come vivo?  (solo 'vivo')
#   .AutorizzaDichiararloMorto  -> posso AGIRE come se fosse morto, cioe'
#                                  riavviare lo stack o cancellare il pidfile?
#                                  (solo 'morto')

# Tolleranza (secondi) fra avvio reale e avvio atteso. Stessa costante di
# TOLLERANZA_AVVIO_S in process_liveness.rs. Qui l'attesa e' lo StartTime letto
# subito dopo lo spawn, quindi lo scarto legittimo e' nullo; il margine serve a
# non far dipendere il verdetto dall'arrotondamento al secondo.
$script:NexusTolleranzaAvvioS = 10

# Istante di avvio del processo come epoch unix, oppure $null se il SO non lo
# dichiara. Il $null NON e' un'assenza di processo: e' un'assenza di risposta, e
# i chiamanti la distinguono.
function Get-NexusProcessStartUnix {
  param([Parameter(Mandatory = $true)][int]$ProcessId)
  $proc = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
  if (-not $proc) { return $null }
  try {
    # .StartTime solleva Win32Exception (accesso negato) sui processi di altri
    # utenti o piu' privilegiati, e InvalidOperationException se il processo e'
    # uscito fra la Get-Process e questa riga.
    return [int64]([DateTimeOffset]::new($proc.StartTime.ToUniversalTime(), [TimeSpan]::Zero).ToUnixTimeSeconds())
  }
  catch { return $null }
}

# Il CRITERIO, puro: dati i fatti, qual e' il verdetto? Separato da chi
# interroga il SO perche' si possa provare senza dover produrre a comando un pid
# riciclato o un accesso negato (regola O).
#
#   $Esiste       [bool]         il SO ha visto un processo con quel pid
#   $AvvioReale   [int64|$null]  istante d'avvio letto dal SO
#   $AvvioAtteso  [int64|$null]  istante d'avvio annotato nel registro
#   $NomeReale    [string|$null] nome dell'eseguibile letto dal SO
#   $NomeAtteso   [string|$null] nome dell'eseguibile che il servizio dovrebbe avere
#
# DUE DISCRIMINANTI, DI FORZA DIVERSA, E IL VERDETTO LO DICHIARA. L'istante
# d'avvio identifica UNA esecuzione: se combacia, quel pid e' esattamente il
# processo che abbiamo avviato noi. Il nome dell'eseguibile identifica solo un
# PROGRAMMA — due istanze dello stesso servizio hanno lo stesso nome — ma
# esclude il caso che conta, cioe' il pid riassegnato a un estraneo qualsiasi.
# Il secondo si usa solo quando il primo non c'e': serve ai pidfile scritti
# prima che il campo `start` esistesse, dove l'alternativa non e' una prova piu'
# forte, e' nessuna prova.
function Get-NexusLivenessVerdict {
  param(
    [Parameter(Mandatory = $true)][bool]$Esiste,
    [AllowNull()][object]$AvvioReale,
    [AllowNull()][object]$AvvioAtteso,
    [AllowNull()][string]$NomeReale,
    [AllowNull()][string]$NomeAtteso
  )

  $esito = {
    param($stato, $causa, $dettaglio)
    [pscustomobject]@{
      Stato                     = $stato
      Causa                     = $causa
      Dettaglio                 = $dettaglio
      Vivo                      = ($stato -eq 'vivo')
      AutorizzaDichiararloMorto = ($stato -eq 'morto')
    }
  }

  # La morte accertata viene PRIMA dell'identita': un pid che non esiste e'
  # morto anche se il registro non ha annotato nulla. Invertendo l'ordine, un
  # pidfile vecchio renderebbe ogni processo «non interrogabile» per sempre —
  # cioe' lo stack bloccato esattamente come prima.
  if (-not $Esiste) {
    return & $esito 'morto' 'pid_assente' 'nessun processo con questo pid'
  }

  # Prova forte: l'istante d'avvio.
  if ($null -ne $AvvioAtteso) {
    if ($null -eq $AvvioReale) {
      return & $esito 'non_interrogabile' 'avvio_reale_non_leggibile' `
        'il SO non dichiara l''istante d''avvio (processo di un altro utente o piu'' privilegiato?)'
    }
    $scarto = [Math]::Abs([int64]$AvvioReale - [int64]$AvvioAtteso)
    if ($scarto -le $script:NexusTolleranzaAvvioS) {
      return & $esito 'vivo' '' 'pid esistente, identita'' confermata dall''istante d''avvio'
    }
    return & $esito 'morto' 'pid_riciclato' `
      "pid riciclato su un processo estraneo (avvio reale $AvvioReale, atteso $AvvioAtteso)"
  }

  # Prova debole: l'eseguibile. Dice che il pid non e' finito a un estraneo, non
  # che sia questa esecuzione — e il dettaglio lo mette per iscritto, cosi' chi
  # legge il log sa su che cosa si e' deciso.
  if ($NomeAtteso -and $NomeReale) {
    if ($NomeReale -eq $NomeAtteso) {
      return & $esito 'vivo' '' "pid esistente, eseguibile atteso ($NomeReale): identita' probabile, non certa (il pidfile non porta l'istante d'avvio)"
    }
    return & $esito 'morto' 'pid_riciclato' `
      "pid riciclato: esegue '$NomeReale', atteso '$NomeAtteso'"
  }

  return & $esito 'non_interrogabile' 'avvio_atteso_non_registrato' `
    'il registro non ha annotato l''istante d''avvio ne'' l''eseguibile: il pid esiste, ma nulla lo lega a questo servizio'
}

# La domanda completa su un pid letto da un registro: interroga il SO e giudica.
function Get-NexusProcessLiveness {
  param(
    [Parameter(Mandatory = $true)][int]$ProcessId,
    # Epoch unix annotato al momento dello spawn. $null = non registrato.
    [AllowNull()][object]$ExpectedStartUnix,
    # Nome dell'eseguibile atteso (senza estensione, come lo da' Get-Process):
    # discriminante di ripiego per i pidfile senza istante d'avvio.
    [AllowNull()][string]$ExpectedName
  )
  if ($ProcessId -le 0) {
    return Get-NexusLivenessVerdict -Esiste $false -AvvioReale $null -AvvioAtteso $null
  }
  $proc = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
  $esiste = [bool]$proc
  $reale = if ($esiste) { Get-NexusProcessStartUnix -ProcessId $ProcessId } else { $null }
  # `.ProcessName` e' leggibile anche sui processi di cui `.StartTime` e'
  # negato: e' la ragione per cui puo' fare da ripiego proprio nel caso in cui
  # la prova forte manca.
  $nome = if ($esiste) { $proc.ProcessName } else { $null }
  return Get-NexusLivenessVerdict -Esiste $esiste -AvvioReale $reale -AvvioAtteso $ExpectedStartUnix `
    -NomeReale $nome -NomeAtteso $ExpectedName
}

# IL PIDFILE NON STA QUI. La sua forma — quali campi porta una voce, chi li
# annota, come si serializza e come si rilegge — e' lib\nexus-pidfile.ps1, che
# dot-sourcia questo file e vi aggiunge Get-NexusStackLiveness (il criterio
# applicato a un intero registro) e Read-NexusPidFile (la lettura).
#
# La separazione e' voluta e non e' estetica: questo modulo e' il gemello di
# crates/mcp-core/src/process_liveness.rs, e per restare gemello non deve
# conoscere un formato che esiste solo negli script di deploy: il gemello Rust
# non ha nessun pidfile a cui corrispondere. Di la' invece stanno accanto TUTTI
# i punti che conoscono i nomi dei campi: chi li scrive, chi li legge e chi li
# traduce nei termini di questo criterio. Con la traduzione da una parte e la
# scrittura dall'altra, una rinomina spegnerebbe il discriminante senza far
# fallire nulla.
