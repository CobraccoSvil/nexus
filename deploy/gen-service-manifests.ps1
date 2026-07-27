# deploy/gen-service-manifests.ps1 — genera i manifest di servizio Windows.
#
# Sottile per definizione: NON contiene una lista di servizi. La lista viene dal
# catalogo DB (`system.services_catalog`), gli eseguibili dal workspace
# (`cargo metadata`), e i tre processi non-workspace da
# deploy/service-exec-overrides.toml. Tutto il lavoro sta in
# `cargo xtask service-manifests`, che e' codice versionato, testato e coperto
# dai gate.
#
# PERCHE' ESISTE QUESTO FILE. Il generatore precedente
# (D:\IDEAI-runtime\winsw\gen-winsw.ps1) viveva FUORI dal controllo di versione
# e teneva una lista di servizi scritta a mano. Quella lista non conteneva
# browser-bridge-mcp, che il catalogo dichiara sorvegliato: il servizio non
# aveva manifest, il watchdog lo ritentava a ogni ciclo e falliva sempre, per
# giorni, senza che nessun gate potesse accorgersene. Nella stessa lista
# sopravvivevano chat-service e billing-service, crate rimossi dal repo.
#
# Uso:
#   .\deploy\gen-service-manifests.ps1              # confronta piano e disco
#   .\deploy\gen-service-manifests.ps1 -Write       # scrive i manifest
#   .\deploy\gen-service-manifests.ps1 -DryRun      # solo il piano, nessun disco
#
# Il confronto viene sempre PRIMA della scrittura: un check eseguito dopo il
# write sarebbe verde per costruzione e non misurerebbe nulla.
#
# Richiede DATABASE_URL (dall'ambiente o dal .env del repo) perche' il catalogo
# vive nel DB e non ha un default: se manca, il comando fallisce dicendo cosa
# manca invece di generare manifest da una lista di ripiego.
param(
  [switch]$Write,
  [switch]$DryRun,
  [string]$OutDir,
  [ValidateSet('debug', 'release')][string]$Profile = 'debug'
)
$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutDir) {
  # La radice runtime e' della stessa natura di DATABASE_URL: si prende
  # dall'ambiente, con il default storico come ripiego dichiarato.
  $runtime = if ($env:NEXUS_RUNTIME_ROOT) { $env:NEXUS_RUNTIME_ROOT } else { 'D:\IDEAI-runtime' }
  $OutDir = Join-Path $runtime 'winsw'
}

$flag = if ($DryRun) { '--dry-run' } elseif ($Write) { '--write' } else { '--check' }
$argomenti = @('service-manifests', $flag, '--profile', $Profile)
if (-not $DryRun) { $argomenti += @('--out-dir', $OutDir) }

Push-Location $RepoRoot
try {
  # PRECONDIZIONE: il catalogo che stiamo per leggere e' esattamente cio' che le
  # migrazioni scrivono. Se il DB e' indietro, il piano verrebbe costruito su un
  # catalogo vecchio e i manifest sarebbero sbagliati in un modo che nessuno
  # noterebbe fino all'avvio. `migrate --check` esce 3 quando ci sono pendenti,
  # codice distinto da "non ho potuto guardare": qui la differenza conta.
  & cargo run --quiet -p xtask -- migrate --set meta --check
  if ($LASTEXITCODE -eq 3) {
    Write-Host ''
    Write-Host 'Il database e'' indietro rispetto al set di migrazioni.' -ForegroundColor Yellow
    Write-Host 'Il catalogo dei servizi si legge da li'', quindi i manifest sarebbero costruiti su dati vecchi.'
    Write-Host 'Applicale prima:  cargo xtask migrate --set meta --apply'
    exit 3
  }
  elseif ($LASTEXITCODE -ne 0) {
    Write-Host 'Impossibile verificare lo stato delle migrazioni: vedi l''errore sopra.' -ForegroundColor Yellow
    exit $LASTEXITCODE
  }

  # Il binario gia' compilato se c'e', altrimenti cargo: stesso criterio di
  # scripts/quality-scan.sh, che pero' passa SEMPRE da cargo per non riusare un
  # binario stantio. Qui vale la stessa preoccupazione, quindi cargo sempre.
  & cargo run --quiet -p xtask -- @argomenti
  $code = $LASTEXITCODE
}
finally {
  Pop-Location
}

if ($code -ne 0 -and -not $Write) {
  Write-Host ''
  Write-Host 'Il piano e il disco divergono. Per allinearli:' -ForegroundColor Yellow
  Write-Host '  .\deploy\gen-service-manifests.ps1 -Write'
}
exit $code
