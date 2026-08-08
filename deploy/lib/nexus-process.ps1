# Punto unico (regola L) per TERMINARE un processo Nexus su Windows e VERIFICARE
# il fatto. Dot-source da dev-service.ps1 / dev-stop.ps1 / deploy-local.ps1:
#   . (Join-Path $PSScriptRoot 'lib\nexus-process.ps1')
#
# Perche' esiste (incidente 2026-07-17, regola H sulla causa): i chiamanti facevano
#   cmd /c "taskkill /PID $pid /T /F >nul 2>nul"
#   Write-Host "fermato $id (pid $pid)"
# cioe' sopprimevano l'errore, non leggevano $LASTEXITCODE e dichiaravano l'esito
# senza guardare il processo. Con i servizi avviati da una shell ELEVATA il
# taskkill di una shell non elevata fallisce con "Accesso negato": lo script
# stampava "fermato nexus-mcp-core (pid 39988)" mentre il processo continuava a
# rispondere HTTP 200 su :4000. L'esito riportato era l'OPPOSTO del fatto, e chi
# poi lanciava `cargo build` si prendeva un `os error 5` (.exe lockato) senza
# capire perche'.
#
# Regola M (stato tecnico dai segnali strutturati, mai dedotto):
#   - VERDETTO  = il fatto oggettivo: Get-Process sul PID dopo il tentativo.
#                 Mai l'assunzione che taskkill abbia funzionato, e nemmeno il suo
#                 solo exit code: con /T taskkill esce !=0 se un FIGLIO non e'
#                 terminabile, anche quando il padre e' morto davvero.
#   - DIAGNOSI  = exit code di taskkill (segnale strutturato) + elevazione di
#                 QUESTA shell (token del processo corrente, segnale strutturato).
#   - Il testo di taskkill viene riportato VERBATIM come diagnostica per
#     l'operatore: non ci si decide sopra (niente match su "Access is denied").

# Elevazione di QUESTA shell: letta dal token del processo corrente, non dedotta.
function Test-NexusShellElevated {
  return ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)
}

# PREMESSA VINCOLANTE: risponde alla domanda RISTRETTA «esiste un processo con
# questo pid?», ed e' legittima SOLO qui — su un pid appena tentato da un kill,
# nell'arco di secondi, per vedere se e' sparito. Per un pid letto da un
# REGISTRO (il pidfile) non e' la domanda giusta: quel numero puo' essere stato
# riciclato, e questa funzione risponderebbe $true per un estraneo. Quel caso ha
# il suo punto unico, lib\nexus-liveness.ps1 (Get-NexusProcessLiveness), a cui
# dev-stop.ps1 delega PRIMA di arrivare qui.
function Test-NexusProcessAlive {
  param([AllowNull()][object]$ProcessId)
  if (-not $ProcessId) { return $false }
  return [bool](Get-Process -Id ([int]$ProcessId) -ErrorAction SilentlyContinue)
}

# Termina l'albero del processo e ATTENDE la prova che sia morto.
# Ritorna SEMPRE un esito strutturato (mai un throw): decide il chiamante se e'
# fatale (dev-service: exit 1) o accumulabile (dev-stop: continua e poi esce !=0).
#
#   Stopped        [bool]   il FATTO: il processo non esiste piu'
#   AlreadyStopped [bool]   non era vivo nemmeno prima del tentativo
#   ExitCode       [int]    exit code di taskkill (diagnostica, non verdetto)
#   Output         [string] testo di taskkill, verbatim (solo display)
#   Elevated       [bool]   questa shell e' elevata
#   Message        [string] messaggio pronto per l'operatore (motivo reale + azione)
function Stop-NexusProcessTree {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)][int]$ProcessId,
    [string]$Label = '',
    # /T termina anche i figli (dev-server spawnati dall'agente): senza, restano
    # orfani a tenere le porte. Va OMESSO quando il chiamante e' esso stesso un
    # discendente del target (self-restart di mcp-core): /T abbatterebbe anche lui.
    [switch]$KillTree,
    # Generoso di proposito: taskkill /F e' una TerminateProcess (morte pressoche'
    # immediata), ma un processo appeso in I/O kernel puo' impiegarci. Il loop esce
    # appena il processo sparisce, quindi il costo nel caso normale e' ~0.
    [int]$TimeoutSeconds = 30
  )

  $what = if ($Label) { "$Label (pid $ProcessId)" } else { "pid $ProcessId" }
  $elevated = Test-NexusShellElevated

  if (-not (Test-NexusProcessAlive $ProcessId)) {
    return [pscustomobject]@{
      Stopped = $true; AlreadyStopped = $true; ExitCode = 0; Output = ''
      Elevated = $elevated; Target = $what; Message = "$what gia' fermo"
    }
  }

  $treeFlag = if ($KillTree) { '/T' } else { '' }
  # Redirect INTERNO a cmd (2>&1 dentro la stringa): in PS 5.1 il redirect dello
  # stderr di un exe nativo da PowerShell genera un NativeCommandError. Dentro cmd
  # lo stderr non risale a PS e possiamo CATTURARLO invece di buttarlo in >nul.
  # taskkill manda a capo a meta' frase (e con /T emette una riga per figlio): le
  # righe vengono ricucite in una sola, senza toccarne il contenuto.
  $raw = (cmd /c "taskkill /PID $ProcessId $treeFlag /F 2>&1") | Out-String
  $exitCode = $LASTEXITCODE
  $output = (($raw -split "`r?`n" | ForEach-Object { $_.Trim() } | Where-Object { $_ }) -join ' ')

  # Quanto aspettare: se taskkill ha accettato il comando (exit 0) la morte e'
  # imminente e concediamo l'intero timeout. Se ha rifiutato (exit !=0) il kill non
  # e' nemmeno partito: aspettare 30s e' tempo perso, ma un istante serve comunque
  # perche' il processo puo' essere morto per conto suo nel frattempo (o taskkill
  # puo' aver fallito solo su un figlio). Il verdetto resta Get-Process.
  $waitMs = if ($exitCode -eq 0) { $TimeoutSeconds * 1000 } else { 1000 }
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $alive = Test-NexusProcessAlive $ProcessId
  while ($alive -and $sw.ElapsedMilliseconds -lt $waitMs) {
    Start-Sleep -Milliseconds 200
    $alive = Test-NexusProcessAlive $ProcessId
  }
  $sw.Stop()

  if (-not $alive) {
    return [pscustomobject]@{
      Stopped = $true; AlreadyStopped = $false; ExitCode = $exitCode; Output = $output
      Elevated = $elevated; Target = $what; Message = "fermato $what"
    }
  }

  # Ancora vivo: esito NEGATIVO. Il messaggio dice il motivo reale, non "fermato".
  $secs = [int][Math]::Round($sw.ElapsedMilliseconds / 1000)
  $lines = @("$what E' ANCORA VIVO dopo ${secs}s dal tentativo di kill (taskkill exit=$exitCode).")
  if ($output) { $lines += "taskkill: $output" }
  if (-not $elevated) {
    $lines += "Questa shell NON e' elevata: il servizio e' stato con ogni probabilita' avviato con privilegi elevati (Accesso negato). Rilancia questo script da un terminale PowerShell come amministratore."
  }
  else {
    $lines += "Questa shell E' gia' elevata: il processo non risponde nemmeno al kill forzato (bloccato in I/O kernel?). Ispezionare il PID prima di riprovare."
  }
  $lines += "Finche' resta vivo l'eseguibile e' lockato: un 'cargo build' fallira' con 'os error 5' (accesso negato al file in uso)."

  return [pscustomobject]@{
    Stopped = $false; AlreadyStopped = $false; ExitCode = $exitCode; Output = $output
    Elevated = $elevated; Target = $what; Message = ($lines -join [Environment]::NewLine)
  }
}
