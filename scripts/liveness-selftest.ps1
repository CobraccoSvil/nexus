# Test di deploy/lib/nexus-liveness.ps1, il criterio «questo processo registrato
# e' vivo?» che governa la guardia di dev-start.ps1 e i kill di dev-stop.ps1.
#
# Perche' esiste come test e non come verifica una-tantum: qui un errore non
# produce un messaggio sbagliato, produce un'AZIONE sbagliata — uno stack che non
# riparte (incidente 08/08/2026: nove pid morti nel pidfile e dev-start che si
# rifiutava di avviarsi) oppure un `taskkill /T /F` sull'albero di un estraneo che
# ha ereditato un pid.
#
# Attraversa le funzioni REALI dot-sourciando la libreria di produzione, e i casi
# usano processi VERI di questa macchina: il processo corrente come «vivo» e un
# pid appena terminato come «morto». Il solo caso non producibile a comando — il
# riciclo del pid — passa dal criterio puro, che e' separato apposta (regola O).
#
# Copre anche deploy\lib\nexus-pidfile.ps1, la FORMA del registro da cui quel
# criterio prende le proprie prove. Le due cose non si possono provare separate:
# il 09/08/2026 il criterio era giusto e le prove erano state cancellate a monte
# (dev-service.ps1 riscriveva l'intero pidfile da una vista `id -> pid`), e il
# risultato era uno stack che non si dichiarava fermo e un deploy bloccato tre
# volte. Un test del solo criterio sarebbe rimasto verde per tutto il tempo —
# ed e' esattamente quello che e' successo.
#
# Test di mutazione (da rieseguire dopo ogni modifica alle due librerie):
#   1) togliere il confronto sull'avvio, cioe' ritornare 'vivo' appena il pid
#      esiste           -> deve ROSSEGGIARE su [riciclo] e [riciclo per exe]
#   2) far degradare 'non_interrogabile' a 'morto'
#                       -> deve ROSSEGGIARE su [ignoto]
#   3) mettere il ramo dell'identita' PRIMA di quello dell'assenza
#                       -> deve ROSSEGGIARE su [assente senza attesa], che e'
#                          esattamente il caso dell'incidente dell'08/08
#   4) togliere l'annotazione di `start` da New-NexusPidEntry
#                       -> deve ROSSEGGIARE su [voce nasce con le prove], che
#                          pretende la prova FORTE e non si accontenta di quella
#                          debole sull'eseguibile
#   5) togliere anche `exe`, o far proiettare a Write-NexusPidFile i soli
#      {id,pid} (cioe' rimettere Write-PidMap)
#                       -> deve ROSSEGGIARE su [azione su un servizio non spoglia
#                          gli altri], che e' l'incidente del 09/08 in forma di test
#   6) togliere il completamento dal manifest in Resolve-NexusPidEntries
#                       -> deve ROSSEGGIARE su [voce antecedente completata dal
#                          manifest]: un pidfile vecchio tornerebbe a bloccare
#                          dev-stop per sempre
# Misurato: senza le mutazioni tutti i casi passano.
#
# Uso: powershell -NoProfile -File scripts\liveness-selftest.ps1
# Exit 0 = tutti i casi superati.
$ErrorActionPreference = 'Stop'
# nexus-pidfile.ps1 dot-sourcia il criterio: si carica il consumatore reale, non
# le due meta' montate a mano (regola O).
. (Join-Path $PSScriptRoot '..\deploy\lib\nexus-pidfile.ps1')

$falliti = 0
function Test-Caso([string]$nome, [scriptblock]$prova) {
  try {
    $esito = & $prova
    if ($esito -eq $true) { Write-Host "OK   [$nome]" -ForegroundColor Green }
    else {
      Write-Host "FAIL [$nome]: $esito" -ForegroundColor Red
      $script:falliti++
    }
  }
  catch {
    Write-Host "FAIL [$nome]: eccezione $($_.Exception.Message)" -ForegroundColor Red
    $script:falliti++
  }
}

# --- vivo: il processo corrente, con il proprio istante d'avvio come atteso ----
Test-Caso 'vivo' {
  $start = Get-NexusProcessStartUnix -ProcessId $PID
  if ($null -eq $start) { return "istante d'avvio del processo corrente non leggibile" }
  $v = Get-NexusProcessLiveness -ProcessId $PID -ExpectedStartUnix $start
  if ($v.Stato -ne 'vivo') { return "atteso vivo, ottenuto $($v.Stato)/$($v.Causa)" }
  if ($v.AutorizzaDichiararloMorto) { return 'un processo vivo autorizza a dichiararlo morto' }
  return $true
}

# --- riciclo: pid esistente, istante d'avvio lontano dall'atteso --------------
# E' il caso «morti dichiarati vivi»: senza il confronto, questo pid passerebbe
# per il nostro processo.
Test-Caso 'riciclo' {
  $start = Get-NexusProcessStartUnix -ProcessId $PID
  $v = Get-NexusProcessLiveness -ProcessId $PID -ExpectedStartUnix ($start - 3600)
  if ($v.Stato -ne 'morto' -or $v.Causa -ne 'pid_riciclato') {
    return "atteso morto/pid_riciclato, ottenuto $($v.Stato)/$($v.Causa)"
  }
  return $true
}

# --- riciclo per exe: nessun istante d'avvio, ma l'eseguibile non e' quello ----
Test-Caso 'riciclo per exe' {
  $v = Get-NexusProcessLiveness -ProcessId $PID -ExpectedStartUnix $null -ExpectedName 'un-eseguibile-che-non-esiste'
  if ($v.Stato -ne 'morto') { return "atteso morto, ottenuto $($v.Stato)/$($v.Causa)" }
  return $true
}

# --- vivo per exe: la prova debole regge quando manca quella forte ------------
# E' il caso dei pidfile scritti prima che il campo `start` esistesse.
Test-Caso 'vivo per exe' {
  $nome = (Get-Process -Id $PID).ProcessName
  $v = Get-NexusProcessLiveness -ProcessId $PID -ExpectedStartUnix $null -ExpectedName $nome
  if ($v.Stato -ne 'vivo') { return "atteso vivo, ottenuto $($v.Stato)/$($v.Causa)" }
  if ($v.Dettaglio -notmatch 'non certa') {
    return 'la prova debole non dichiara di essere tale: chi legge il log non puo'' saperlo'
  }
  return $true
}

# --- ignoto: pid esistente e NESSUNA prova -> non si decide -------------------
# Non deve degradare ne' a vivo (si avvierebbe un secondo stack) ne' a morto (si
# ucciderebbe un albero altrui).
Test-Caso 'ignoto' {
  $v = Get-NexusProcessLiveness -ProcessId $PID -ExpectedStartUnix $null
  if ($v.Stato -ne 'non_interrogabile') { return "atteso non_interrogabile, ottenuto $($v.Stato)" }
  if ($v.Vivo -or $v.AutorizzaDichiararloMorto) {
    return 'l''ignoto autorizza ad agire: e'' il difetto in una delle due direzioni'
  }
  return $true
}

# --- assente: un processo appena terminato e' morto DAVVERO -------------------
Test-Caso 'assente' {
  $p = Start-Process -FilePath 'cmd.exe' -ArgumentList '/c', 'exit 0' -WindowStyle Hidden -PassThru
  $p.WaitForExit()
  # Il pid resta non riassegnato per un istante: e' la finestra in cui il SO lo
  # dichiara inesistente.
  Start-Sleep -Milliseconds 300
  $v = Get-NexusProcessLiveness -ProcessId $p.Id -ExpectedStartUnix 1000000
  if ($v.Stato -ne 'morto') { return "atteso morto, ottenuto $($v.Stato)/$($v.Causa)" }
  return $true
}

# --- assente senza attesa: L'INCIDENTE DELL'08/08 ----------------------------
# Un pidfile vecchio non porta ne' `start` ne' `exe`. Se l'identita' venisse
# valutata PRIMA dell'esistenza, ogni pid morto diventerebbe «non interrogabile»
# e lo stack resterebbe bloccato esattamente come prima del fix.
Test-Caso 'assente senza attesa' {
  $v = Get-NexusLivenessVerdict -Esiste $false -AvvioReale $null -AvvioAtteso $null -NomeReale $null -NomeAtteso $null
  if (-not $v.AutorizzaDichiararloMorto) {
    return "un pid inesistente senza attesa non e' stato dichiarato morto: $($v.Stato)/$($v.Causa)"
  }
  return $true
}

# --- il pidfile si legge intero ----------------------------------------------
# PS 5.1 ha due trappole opposte sullo stesso file: `ConvertTo-Json` di UN
# elemento non produce un array, e `@(... | ConvertFrom-Json)` non enumera
# l'array che legge. Misurato sul pidfile vero: nove voci lette come una, e ogni
# `.pid` diventava un Object[].
Test-Caso 'pidfile letto intero' {
  $tmp = Join-Path $env:TEMP "nexus-liveness-selftest-$PID.json"
  try {
    @(
      [pscustomobject]@{ id = 'a'; pid = 1; start = 100 },
      [pscustomobject]@{ id = 'b'; pid = 2; start = 200 },
      [pscustomobject]@{ id = 'c'; pid = 3; start = 300 }
    ) | ConvertTo-Json | Set-Content -Path $tmp -Encoding utf8
    $voci = Read-NexusPidFile -Path $tmp
    if ($voci.Count -ne 3) { return "lette $($voci.Count) voci su 3" }
    if ($voci[0].pid -isnot [int]) { return "il pid non e' un intero: $($voci[0].pid.GetType().Name)" }

    # E un file con UNA sola voce, serializzato come oggetto singolo, resta una
    # voce sola e non zero.
    ([pscustomobject]@{ id = 'solo'; pid = 7; start = 700 }) | ConvertTo-Json | Set-Content -Path $tmp -Encoding utf8
    $una = Read-NexusPidFile -Path $tmp
    if ($una.Count -ne 1) { return "voce singola letta come $($una.Count) elementi" }
    return $true
  }
  finally { Remove-Item $tmp -Force -ErrorAction SilentlyContinue }
}

# --- lo stack morto non blocca il riavvio: L'INCIDENTE, in forma di elenco ----
Test-Caso 'stack tutto morto' {
  $finto = @(
    [pscustomobject]@{ id = 'nexus-mcp-core'; pid = 999001 },
    [pscustomobject]@{ id = 'nexus-gateway'; pid = 999002 },
    [pscustomobject]@{ id = 'nexus-web-ide'; pid = 999003 }
  )
  $stato = Get-NexusStackLiveness -Voci $finto
  if ($stato.Count -ne 3) { return "verdetti attesi 3, ottenuti $($stato.Count)" }
  $vivi = @($stato | Where-Object { $_.Vivo })
  $ignoti = @($stato | Where-Object { -not $_.Vivo -and -not $_.AutorizzaDichiararloMorto })
  if ($vivi.Count -ne 0 -or $ignoti.Count -ne 0) {
    return "pidfile di soli pid morti: vivi=$($vivi.Count) ignoti=$($ignoti.Count) -> dev-start si bloccherebbe ancora"
  }
  return $true
}

# ============================================================================
# FORMA DEL PIDFILE (deploy\lib\nexus-pidfile.ps1)
# ============================================================================
# I casi che seguono attraversano il PRODUTTORE reale: la voce la costruisce
# New-NexusPidEntry e il file lo scrive Write-NexusPidFile, cioe' le stesse
# funzioni che dev-start.ps1 e dev-service.ps1 chiamano. Un pidfile costruito a
# mano nel test proverebbe che il criterio sa leggere il JSON che il test sa
# scrivere, che e' precisamente cio' che non serve sapere.

# Un processo vero, nostro e vivo per la durata del test: e' l'unico modo di
# misurare l'annotazione delle prove nell'istante in cui la produzione la fa.
function New-ProcessoDiProva {
  return Start-Process -FilePath 'cmd.exe' -ArgumentList '/c', 'ping -n 60 127.0.0.1 >nul' `
    -WindowStyle Hidden -PassThru
}

# --- la voce nasce con le prove, e portano al verdetto FORTE ------------------
# E' il contratto che dev-start e dev-service devono rispettare: chi registra un
# pid annota le prove nello stesso momento. Non basta che il verdetto sia
# 'vivo' — ci si arriva anche con la sola prova debole — quindi si pretende il
# discriminante d'avvio, che e' l'unico che identifica UNA esecuzione.
Test-Caso 'voce nasce con le prove' {
  $p = New-ProcessoDiProva
  $tmp = Join-Path $env:TEMP "nexus-pidfile-selftest-$PID-a.json"
  try {
    $voce = New-NexusPidEntry -Id 'prova' -ProcessId $p.Id
    if ($null -eq $voce.start) { return 'New-NexusPidEntry non ha annotato l''istante d''avvio' }
    if (-not $voce.exe) { return 'New-NexusPidEntry non ha annotato l''eseguibile' }

    Write-NexusPidFile -Path $tmp -Voci @($voce)
    $rilette = Read-NexusPidFile -Path $tmp
    if ($rilette.Count -ne 1) { return "riletta $($rilette.Count) voce/i su 1" }

    $v = (Get-NexusStackLiveness -Voci $rilette)[0]
    if (-not $v.Vivo) { return "voce appena scritta non accertabile: $($v.Stato)/$($v.Causa)" }
    if ($v.Dettaglio -notmatch 'istante d''avvio') {
      return "il verdetto non si regge sulla prova forte ma su: $($v.Dettaglio)"
    }
    return $true
  }
  finally {
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    Remove-Item $tmp -Force -ErrorAction SilentlyContinue
  }
}

# --- L'INCIDENTE DEL 09/08, in forma di test ---------------------------------
# Un'azione su UN servizio non deve spogliare gli altri. Prima, dev-service.ps1
# leggeva il file in una hashtable `id -> pid` e lo riscriveva da quella: nove
# voci restavano senza prove, nessun pid era piu' identificabile, dev-stop.ps1
# usciva 1 e deploy-local.ps1 si fermava con gli eseguibili lockati.
Test-Caso 'azione su un servizio non spoglia gli altri' {
  $p = New-ProcessoDiProva
  $tmp = Join-Path $env:TEMP "nexus-pidfile-selftest-$PID-b.json"
  try {
    $voci = @(
      (New-NexusPidEntry -Id 'nexus-mcp-core' -ProcessId $PID),
      (New-NexusPidEntry -Id 'nexus-gateway' -ProcessId $PID),
      (New-NexusPidEntry -Id 'nexus-web-ide' -ProcessId $PID)
    )
    Write-NexusPidFile -Path $tmp -Voci $voci

    # Il giro esatto di dev-service.ps1: rileggi, sostituisci UNA voce, riscrivi.
    $lette = Read-NexusPidFile -Path $tmp
    $aggiornate = Set-NexusPidEntry -Voci $lette -Voce (New-NexusPidEntry -Id 'nexus-gateway' -ProcessId $p.Id)
    Write-NexusPidFile -Path $tmp -Voci $aggiornate

    $finali = Read-NexusPidFile -Path $tmp
    if ($finali.Count -ne 3) { return "dopo la sostituzione restano $($finali.Count) voci su 3" }
    foreach ($v in $finali) {
      if ($null -eq $v.start) { return "$($v.id): l'istante d'avvio e' stato perso nel giro di scrittura" }
      if (-not $v.exe) { return "$($v.id): l'eseguibile e' stato perso nel giro di scrittura" }
    }
    $gw = $finali | Where-Object { $_.id -eq 'nexus-gateway' }
    if ([int]$gw.pid -ne $p.Id) { return "la voce sostituita non porta il pid nuovo ($($gw.pid) invece di $($p.Id))" }

    # E ogni voce resta accertabile: e' la conseguenza che il 09/08 mancava.
    $ignoti = @(Get-NexusStackLiveness -Voci $finali | Where-Object { -not $_.Vivo -and -not $_.AutorizzaDichiararloMorto })
    if ($ignoti.Count -gt 0) { return "$($ignoti.Count) voce/i non accertabile/i dopo il giro di scrittura" }
    return $true
  }
  finally {
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    Remove-Item $tmp -Force -ErrorAction SilentlyContinue
  }
}

# --- il file su disco porta i campi canonici e nulla di piu' ------------------
# Resolve-NexusPidEntries appende diagnostica in memoria: se finisse su disco,
# il pidfile diventerebbe il posto dove ogni consumatore aggiunge il suo campo.
Test-Caso 'la scrittura proietta sui campi canonici' {
  $tmp = Join-Path $env:TEMP "nexus-pidfile-selftest-$PID-c.json"
  try {
    $voce = New-NexusPidEntry -Id 'prova' -ProcessId $PID |
    Add-Member -NotePropertyName 'ProveIdentita' -NotePropertyValue 'registrate' -PassThru -Force
    Write-NexusPidFile -Path $tmp -Voci @($voce)
    $riletta = (Read-NexusPidFile -Path $tmp)[0]
    $campi = @($riletta.PSObject.Properties.Name | Sort-Object)
    $attesi = @('exe', 'id', 'pid', 'start')
    if (($campi -join ',') -ne ($attesi -join ',')) {
      return "campi su disco: $($campi -join ',') (attesi: $($attesi -join ','))"
    }
    return $true
  }
  finally { Remove-Item $tmp -Force -ErrorAction SilentlyContinue }
}

# --- voce antecedente completata dal manifest --------------------------------
# Un pidfile scritto prima del contratto non porta ne' `start` ne' `exe`. Non e'
# la stessa cosa di un processo non osservabile: la prova debole (quale
# eseguibile quell'id lancia) e' recuperabile da una fonte nostra, il manifest.
# Senza il recupero ogni voce resterebbe «non interrogabile» e lo stack non si
# dichiarerebbe mai fermo.
#
# Il manifest e' una fixture, ma il LETTORE e' quello di produzione
# (Read-NexusServiceManifest) e lo schema ha gia' il proprio test dal lato del
# generatore (crates/xtask, service_manifests::winsw). In coda, se questa
# macchina ha i manifest reali, si verifica anche contro quelli.
Test-Caso 'voce antecedente completata dal manifest' {
  $p = New-ProcessoDiProva
  $radice = Join-Path $env:TEMP "nexus-pidfile-selftest-$PID-winsw"
  try {
    $dir = Join-Path $radice 'servizio-finto'
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Set-Content -Path (Join-Path $dir 'servizio-finto.xml') -Encoding utf8 -Value @'
<service>
  <id>servizio-finto</id>
  <executable>C:\Windows\System32\cmd.exe</executable>
  <workingdirectory>C:\Windows</workingdirectory>
</service>
'@

    # La forma misurata sul pidfile vero il 09/08: solo id e pid.
    $antecedenti = @(
      [pscustomobject]@{ id = 'servizio-finto'; pid = $p.Id },
      [pscustomobject]@{ id = 'senza-manifest'; pid = $p.Id }
    )
    $risolte = Resolve-NexusPidEntries -Voci $antecedenti -WinswRoot $radice

    $conManifest = $risolte | Where-Object { $_.id -eq 'servizio-finto' }
    if (-not $conManifest.Antecedente) { return 'una voce senza le chiavi non e'' stata riconosciuta come antecedente' }
    if ($conManifest.ProveIdentita -ne 'dal_manifest') { return "prove: $($conManifest.ProveIdentita) (attese dal_manifest)" }
    if ($conManifest.exe -ne 'cmd') { return "eseguibile atteso 'cmd', ottenuto '$($conManifest.exe)'" }

    $v = (Get-NexusStackLiveness -Voci @($conManifest))[0]
    if (-not $v.Vivo) { return "voce completata non accertabile: $($v.Stato)/$($v.Causa)" }
    if ($v.Dettaglio -notmatch 'non certa') {
      return 'la prova debole non si dichiara tale: chi legge il log crederebbe a un''identita'' certa'
    }

    # Riscritta, la voce antecedente resta antecedente: la prova viene dal
    # manifest e non da un'osservazione, e persisterla la renderebbe
    # indistinguibile da una misura — perdendo il solo segnale che permette di
    # accorgersi di un produttore che smette di annotare.
    $tmp = Join-Path $env:TEMP "nexus-pidfile-selftest-$PID-d.json"
    try {
      Write-NexusPidFile -Path $tmp -Voci @($conManifest)
      $riletta = (Read-NexusPidFile -Path $tmp)[0]
      $campi = @($riletta.PSObject.Properties.Name | Sort-Object)
      if (($campi -join ',') -ne 'id,pid') {
        return "voce antecedente riscritta con: $($campi -join ',') (la prova dal manifest e' stata persistita come misura)"
      }
      $ancora = (Resolve-NexusPidEntries -Voci @($riletta) -WinswRoot $radice)[0]
      if (-not $ancora.Antecedente) { return 'dopo la riscrittura il file non si dichiara piu'' antecedente' }
    }
    finally { Remove-Item $tmp -Force -ErrorAction SilentlyContinue }

    # Senza manifest non si inventa nulla: l'assenza di prove resta un'assenza.
    $senza = $risolte | Where-Object { $_.id -eq 'senza-manifest' }
    if ($senza.ProveIdentita -ne 'assenti') { return "prove: $($senza.ProveIdentita) (attese assenti)" }
    $vs = (Get-NexusStackLiveness -Voci @($senza))[0]
    if ($vs.Stato -ne 'non_interrogabile') { return "senza prove il verdetto e' $($vs.Stato), non un'astensione" }

    # Manifest REALI di questa macchina, quando ci sono: e' l'input che la
    # produzione legge davvero.
    $reali = 'D:\IDEAI-runtime\winsw'
    if (Test-Path (Join-Path $reali 'nexus-mcp-core\nexus-mcp-core.xml')) {
      # pid inesistente: nessun effetto su nulla, si guarda la sola risoluzione.
      $r = (Resolve-NexusPidEntries -Voci @([pscustomobject]@{ id = 'nexus-mcp-core'; pid = 999001 }) -WinswRoot $reali)[0]
      if ($r.ProveIdentita -ne 'dal_manifest' -or -not $r.exe) {
        return "manifest reale nexus-mcp-core: prove '$($r.ProveIdentita)', exe '$($r.exe)'"
      }
    }
    return $true
  }
  finally {
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    Remove-Item $radice -Recurse -Force -ErrorAction SilentlyContinue
  }
}

Write-Host ''
if ($falliti -gt 0) {
  [Console]::Error.WriteLine("liveness-selftest: $falliti caso/i FALLITO/I.")
  exit 1
}
Write-Host 'liveness-selftest: tutti i casi superati.' -ForegroundColor Cyan
exit 0
