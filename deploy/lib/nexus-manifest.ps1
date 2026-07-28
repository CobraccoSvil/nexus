# Punto unico (regola L) per LEGGERE un manifest di servizio WinSW. Dot-source da
# dev-start.ps1 / dev-service.ps1 / lib\nexus-publish.ps1:
#   . (Join-Path $PSScriptRoot 'lib\nexus-manifest.ps1')
#
# Perche' esiste. La lettura viveva in TRE copie divergenti — dev-start.ps1,
# dev-service.ps1 e Get-NexusServiceManifests — piu' una QUARTA in Rust
# (`parse_winsw`, xtask/src/service_manifests/winsw.rs) che i test usavano come
# controfigura del consumatore. Quattro implementazioni della stessa domanda:
# esattamente il caso che la regola L vieta, e la divergenza e' arrivata.
#
# L'INCIDENTE che chiude (2026-07-28, stack a processi giu' dopo deploy-local):
# `<arguments>` ed `<env>` sono OPZIONALI nello schema WinSW, e il generatore li
# omette quando sono vuoti — deliberatamente e con un test che lo certifica
# (`un_servizio_senza_argomenti_non_emette_il_tag`). I lettori PowerShell invece
# facevano `$s.arguments` / `$s.env`, cioe' li trattavano da obbligatori: assunto
# ereditato dal vecchio generatore fuori repo, che li emetteva sempre vuoti.
# Finche' nessuno attivava StrictMode l'accesso rendeva $null e il difetto era
# invisibile; dal momento in cui `deploy-local.ps1` ha dot-sourced una libreria
# che imposta StrictMode (scope DINAMICO: si propaga a valle, vedi nexus-publish),
# lo stesso accesso e' diventato un'eccezione e 7 servizi su 8 non sono partiti.
#
# Da qui due scelte di progetto:
#
# 1. SI LEGGE CON XPath, NON con l'adapter a proprieta'.
#    `SelectSingleNode`/`SelectNodes`/`GetAttribute` sono metodi .NET: rendono
#    $null o stringa vuota su cio' che manca, in ogni modalita' di esecuzione. La
#    lettura non dipende piu' da chi ci ha chiamati. `$s.arguments` invece cambia
#    comportamento con StrictMode, cioe' col percorso di invocazione: e' il modo
#    peggiore di rompersi, perche' lo stesso script funziona lanciato a mano e
#    fallisce dentro il deploy.
#
# 2. L'ASSENZA DI UN TAG OPZIONALE E' UN VALORE PREVISTO, NON UN ERRORE.
#    Un servizio senza argomenti rende Arguments = '' perche' lo schema lo
#    ammette, non perche' stiamo tollerando un manifest malformato. La riga di
#    demarcazione sta piu' in alto: un file illeggibile o privo di <service> e'
#    un errore esplicito, non una riga che sparisce. `<executable>` vuoto invece
#    si RENDE al chiamante senza giudicarlo, perche' i due consumatori ne fanno
#    cose opposte — chi avvia deve fallire, mentre chi pubblica salta di
#    proposito i manifest orfani dei servizi rimossi (vedi Get-NexusPublishDir).

# Legge un manifest e ne rende i fatti che i consumatori usano davvero:
# eseguibile, working directory, riga argomenti, variabili d'ambiente.
#
# StrictMode e' impostato DENTRO la funzione, non a livello di file: cosi' vale
# per questo codice e per i suoi discendenti senza essere imposto al chiamante
# che ci dot-sourcia. E' precisamente l'errore che ha prodotto l'incidente.
function Read-NexusServiceManifest {
  param([Parameter(Mandatory = $true)][string]$Path)
  Set-StrictMode -Version Latest

  if (-not (Test-Path $Path)) { throw "manifest non trovato: $Path" }
  try { [xml]$xml = Get-Content $Path -Raw } catch {
    throw "manifest illeggibile ${Path}: $($_.Exception.Message)"
  }

  $svc = $xml.SelectSingleNode('/service')
  if (-not $svc) { throw "manifest senza elemento <service>: $Path" }

  # Tag opzionali: SelectSingleNode rende $null se assenti, mai eccezione.
  $testoDi = {
    param($tag)
    $n = $svc.SelectSingleNode($tag)
    if ($n) { $n.InnerText.Trim() } else { '' }
  }

  # <env name="X" value="Y"/>: si leggono gli ATTRIBUTI con GetAttribute, non con
  # $e.name — su un XmlElement `Name` e' anche una proprieta' .NET (il nome del
  # tag), quindi l'accesso a proprieta' e' ambiguo per costruzione.
  $env = @()
  foreach ($n in $svc.SelectNodes('env')) {
    $nome = $n.GetAttribute('name')
    if (-not $nome) { continue }
    $env += [pscustomobject]@{ Name = $nome; Value = $n.GetAttribute('value') }
  }

  return [pscustomobject]@{
    Id               = & $testoDi 'id'
    Executable       = & $testoDi 'executable'
    WorkingDirectory = & $testoDi 'workingdirectory'
    # Riga grezza, NON splittata: il generatore la compone con un join su spazio e
    # Start-Process la rivuole intera. Splittare e ricomporre romperebbe i path
    # con spazi, in silenzio.
    Arguments        = & $testoDi 'arguments'
    Env              = $env
    Path             = $Path
  }
}

# Tutti i manifest sotto $WinswDir, uno per directory <id>\<id>.xml.
# Un manifest illeggibile e' un errore esplicito: e' la differenza fra "non ci
# sono servizi" e "non ho potuto leggerli".
function Get-NexusServiceManifestList {
  param([Parameter(Mandatory = $true)][string]$WinswDir)
  Set-StrictMode -Version Latest

  if (-not (Test-Path $WinswDir)) {
    throw "manifest dei servizi non trovati in $WinswDir. Generarli con: .\deploy\gen-service-manifests.ps1 -Write"
  }
  $out = @()
  foreach ($dir in (Get-ChildItem $WinswDir -Directory -ErrorAction SilentlyContinue)) {
    $xmlPath = Join-Path $dir.FullName "$($dir.Name).xml"
    if (-not (Test-Path $xmlPath)) { continue }
    $m = Read-NexusServiceManifest -Path $xmlPath
    # La directory e' l'identita' con cui i consumatori indirizzano il servizio
    # (pidfile, dev-service -Service): prevale sul tag <id>, che deve comunque
    # combaciare per costruzione.
    $out += ($m | Add-Member -NotePropertyName Dir -NotePropertyValue $dir.Name -PassThru -Force)
  }
  return $out
}
