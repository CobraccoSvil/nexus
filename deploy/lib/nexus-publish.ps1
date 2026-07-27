# Punto unico (regola L) per PUBBLICARE gli artefatti compilati nella directory da
# cui i servizi ESEGUONO. Dot-source da deploy-local.ps1 / dev-build.ps1:
#   . (Join-Path $PSScriptRoot 'lib\nexus-publish.ps1')
#
# Perche' esiste. Su Windows un .exe in esecuzione e' lockato in scrittura: finche'
# i manifest hanno puntato a `target\debug`, ogni `cargo build` a stack vivo moriva
# con os error 5 DOPO aver ricompilato tutto. La cura e' separare dove si COMPILA
# da dove si ESEGUE — ma introduce un passo nuovo, la copia, e un passo nuovo che
# vive in UN solo script e' un passo che gli altri flussi dimenticano.
#
# E' esattamente quello che era successo: la pubblicazione era stata insegnata solo
# a deploy-local.ps1, mentre dev-build.ps1 (il ciclo canonico dello stack a
# PROCESSI: "Sostituisce deploy-local.ps1 nel flusso processi") continuava a fare
# STOP -> cargo build -> dev-start. Siccome dev-start lancia l'eseguibile
# DICHIARATO DAL MANIFEST, il risultato era: build verde, stack riavviato, modifica
# NON in esecuzione, zero messaggi. Il difetto peggiore possibile — silenzioso, e
# nel ciclo di lavoro quotidiano.
#
# Due scelte di progetto, entrambe per non ripetere l'errore:
#
# 1. LA DESTINAZIONE SI DERIVA DAI MANIFEST, non si ricompone.
#    I manifest sono gia' la fonte unica per dev-start/dev-stop/dev-service, che ne
#    leggono <executable>. Ricomporre qui `<runtime>/bin/<profilo>` avrebbe creato
#    una seconda copia della regola (in PowerShell) accanto a quella in Rust
#    (xtask service-manifests): due posti che possono divergere, e la divergenza
#    sarebbe SILENZIOSA — i servizi ripartirebbero sulla versione precedente senza
#    dirlo. Derivandola, la copia arriva dove i servizi guardano PER COSTRUZIONE.
#
# 2. SI PUBBLICA CIO' CHE I MANIFEST DICHIARANO, non un glob su target\debug.
#    Un glob copierebbe anche xtask.exe e nexus-sudo-runner.exe (che servizi non
#    sono), e soprattutto copierebbe binari il cui servizio nessuno ha fermato:
#    Copy-Item su un exe vivo fallisce, e fallirebbe a stack gia' fermo.

Set-StrictMode -Version Latest

# Radice di runtime: unico posto in PowerShell che la nomina. Deve combaciare con
# il default di NEXUS_RUNTIME_ROOT in xtask service-manifests; e' solo il punto di
# partenza per TROVARE i manifest, non la destinazione della copia (vedi sopra).
function Get-NexusRuntimeRoot {
  if ($env:NEXUS_RUNTIME_ROOT) { return $env:NEXUS_RUNTIME_ROOT }
  return 'D:\IDEAI-runtime'
}

# I servizi dichiarati dai manifest: id, eseguibile, directory di esecuzione.
# Un manifest illeggibile e' un errore esplicito, non una riga che sparisce.
function Get-NexusServiceManifests {
  param([string]$WinswDir)
  if (-not $WinswDir) { $WinswDir = Join-Path (Get-NexusRuntimeRoot) 'winsw' }
  if (-not (Test-Path $WinswDir)) {
    throw "manifest dei servizi non trovati in $WinswDir. Generarli con: .\deploy\gen-service-manifests.ps1 -Write"
  }
  $out = @()
  foreach ($dir in (Get-ChildItem $WinswDir -Directory -ErrorAction SilentlyContinue)) {
    $xmlPath = Join-Path $dir.FullName "$($dir.Name).xml"
    if (-not (Test-Path $xmlPath)) { continue }
    try { $x = [xml](Get-Content $xmlPath -Raw) } catch {
      throw "manifest illeggibile ${xmlPath}: $($_.Exception.Message)"
    }
    $exe = $x.service.executable
    if (-not $exe) { continue }
    $out += [pscustomobject]@{ Id = $dir.Name; Executable = $exe; Xml = $xmlPath }
  }
  return $out
}

# La directory da cui i binari del WORKSPACE eseguono, LETTA dai manifest.
#
# Fail-closed su due condizioni, perche' in entrambe una copia sarebbe inutile e
# l'errore comparirebbe piu' tardi, altrove, travestito da altro:
#   - nessun manifest punta a un .exe: non c'e' nulla da pubblicare;
#   - i manifest puntano ancora dentro l'albero di build (`\target\`): sono quelli
#     PRECEDENTI alla separazione compila/esegue. Pubblicare non servirebbe (i
#     servizi eseguono da target\) e la build a stack acceso fallirebbe col lock.
function Get-NexusPublishDir {
  param([string]$WinswDir)
  $manifests = Get-NexusServiceManifests -WinswDir $WinswDir
  $exeDirs = @()
  foreach ($m in $manifests) {
    if ($m.Executable -match '\.exe$') { $exeDirs += (Split-Path $m.Executable -Parent) }
  }
  $exeDirs = @($exeDirs | Where-Object { $_ } | Select-Object -Unique)
  if ($exeDirs.Count -eq 0) {
    throw 'nessun manifest dichiara un eseguibile: non c e nulla da pubblicare.'
  }
  # La dir dei binari del workspace e' quella che ne ospita di piu': gli scostamenti
  # (node, garnet, qdrant) stanno ognuno per conto proprio.
  #
  # Si sceglie PRIMA la dir dominante e solo DOPO si controlla dov'e'. Il contrario
  # — rifiutare se QUALCHE manifest punta in `target\` — sembra piu' prudente ed e'
  # invece fragile: in <runtime>/winsw sopravvivono i manifest ORFANI dei servizi
  # rimossi dal repo (billing-service, chat-service), mai rigenerati e quindi fermi
  # ai path vecchi. Farebbero fallire la pubblicazione di servizi vivi per colpa di
  # servizi che non esistono piu'. (`xtask service-manifests --check` li segnala
  # gia' come ORFANO: e' li' che vanno tolti, non qui.)
  $gruppi = $exeDirs | ForEach-Object {
    $d = $_
    [pscustomobject]@{ Dir = $d; N = @($manifests | Where-Object { (Split-Path $_.Executable -Parent) -eq $d }).Count }
  }
  $dominante = ($gruppi | Sort-Object -Property N -Descending | Select-Object -First 1).Dir
  if ($dominante -match '[\\/]target[\\/]') {
    throw ("i manifest puntano ancora dentro l albero di build ($dominante): sono " +
           'quelli precedenti alla separazione compila/esegue. Pubblicare non servirebbe ' +
           '(i servizi eseguono da li) e la build a stack acceso fallirebbe col lock. ' +
           'Rigenerarli con .\deploy\gen-service-manifests.ps1 -Write, poi ripetere.')
  }
  return $dominante
}

# Copia in $PublishDir i binari dichiarati dai manifest e i rispettivi simboli.
#
# NOTA sui .pdb: rustc nomina i simboli col nome del CRATE (underscore), mentre
# l'eseguibile porta il nome del BINARIO (trattino): `mcp-core.exe` ->
# `mcp_core.pdb`. Cercare il pdb con [IO.Path]::ChangeExtension sull'exe trova
# `mcp-core.pdb`, che non esiste: i simboli non venivano copiati e i backtrace in
# runtime restavano senza numeri di riga, in silenzio.
#
# Ritorna un oggetto con l'esito: chi ha pubblicato, chi mancava, dove.
function Publish-NexusArtifacts {
  param(
    [string]$BuildDir = 'D:\IDEAI\target\debug',
    [string]$WinswDir,
    [switch]$Quiet
  )
  $publishDir = Get-NexusPublishDir -WinswDir $WinswDir
  if (-not (Test-Path $BuildDir)) {
    throw "directory di build assente ($BuildDir): eseguire prima cargo build."
  }
  New-Item -ItemType Directory -Force -Path $publishDir | Out-Null

  $manifests = Get-NexusServiceManifests -WinswDir $WinswDir
  $attesi = @($manifests | Where-Object { (Split-Path $_.Executable -Parent) -eq $publishDir })

  $pubblicati = @()
  $mancanti = @()
  foreach ($m in $attesi) {
    $nome = Split-Path $m.Executable -Leaf
    $src = Join-Path $BuildDir $nome
    if (-not (Test-Path $src)) { $mancanti += $nome; continue }
    try {
      Copy-Item $src -Destination (Join-Path $publishDir $nome) -Force
    } catch [System.IO.IOException] {
      # Un .exe in esecuzione e' lockato in scrittura: la copia trova vivo il
      # servizio che avrebbe dovuto essere fermo. Dirlo con il nome del servizio,
      # invece di lasciare un IOException su un path.
      throw ("impossibile pubblicare ${nome}: il servizio '$($m.Id)' e' ancora in " +
             "esecuzione e tiene lockato $(Join-Path $publishDir $nome). " +
             'Fermare lo stack prima di pubblicare (dev-stop.ps1, oppure usare ' +
             'dev-build.ps1/deploy-local.ps1 che lo fanno gia).')
    }
    # simboli: stesso basename con gli underscore al posto dei trattini
    $pdbNome = ([IO.Path]::GetFileNameWithoutExtension($nome) -replace '-', '_') + '.pdb'
    $pdbSrc = Join-Path $BuildDir $pdbNome
    if (Test-Path $pdbSrc) { Copy-Item $pdbSrc -Destination (Join-Path $publishDir $pdbNome) -Force }
    $pubblicati += $nome
  }

  if (-not $Quiet) {
    Write-Host "   pubblicati $($pubblicati.Count) eseguibili in $publishDir" -ForegroundColor DarkGray
    if ($mancanti.Count -gt 0) {
      Write-Host "   ATTENZIONE: $($mancanti.Count) non trovati in ${BuildDir}: $($mancanti -join ', ')" -ForegroundColor Yellow
      Write-Host '   i servizi corrispondenti resteranno sulla versione precedente.' -ForegroundColor Yellow
    }
  }
  return [pscustomobject]@{
    PublishDir = $publishDir
    Pubblicati = $pubblicati
    Mancanti   = $mancanti
  }
}
