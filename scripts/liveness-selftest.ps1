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
# Test di mutazione (da rieseguire dopo ogni modifica alla libreria):
#   1) togliere il confronto sull'avvio, cioe' ritornare 'vivo' appena il pid
#      esiste           -> deve ROSSEGGIARE su [riciclo] e [riciclo per exe]
#   2) far degradare 'non_interrogabile' a 'morto'
#                       -> deve ROSSEGGIARE su [ignoto]
#   3) mettere il ramo dell'identita' PRIMA di quello dell'assenza
#                       -> deve ROSSEGGIARE su [assente senza attesa], che e'
#                          esattamente il caso dell'incidente
# Misurato: senza la mutazione tutti i casi passano.
#
# Uso: powershell -NoProfile -File scripts\liveness-selftest.ps1
# Exit 0 = tutti i casi superati.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\deploy\lib\nexus-liveness.ps1')

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

Write-Host ''
if ($falliti -gt 0) {
  [Console]::Error.WriteLine("liveness-selftest: $falliti caso/i FALLITO/I.")
  exit 1
}
Write-Host 'liveness-selftest: tutti i casi superati.' -ForegroundColor Cyan
exit 0
