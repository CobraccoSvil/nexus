# scripts/cache-sweep.ps1 — tetto alla cache incrementale di Cargo.
#
# PERCHE' ESISTE
#   `scripts/gate-env.sh` tiene CARGO_INCREMENTAL=1 in locale (e 0 in CI): in
#   locale quella cache viene riusata davvero, ed e' l'unica cosa che eviti di
#   ricompilare `mcp-core` intero per una riga cambiata — misurato il
#   2026-08-05: 81s senza incrementale contro 23s con.
#
#   Ma la cache non ha un tetto nativo, e senza una potatura ricresce. Misure
#   sulla stessa macchina:
#     2026-07-26  target-verify 98,0 GB, di cui 80,3 (82%) di incremental
#                 target        186,1 GB, di cui 148,0 (80%)
#     2026-08-05  8 directory target*, 250,78 GB in tutto, 154,05 di incremental
#
#   Riaccendere l'incrementale senza dire come si pota sarebbe stato risolvere
#   un problema creandone un altro piu' lento da vedere.
#
# IL CRITERIO E' L'ETA', NON LA DIMENSIONE
#   Gli artefatti freschi sono esattamente quelli che servono: potare "i piu'
#   grossi" toglierebbe per primo il target su cui si sta lavorando. Si pota
#   cio' che non viene toccato da giorni, che per definizione non sta
#   accelerando nessun ciclo.
#
# COSA NON TOCCA
#   Solo `debug/incremental` e `release/incremental`. Mai `debug/deps`, che sono
#   gli artefatti veri: cancellarli costringe a un rebuild completo, cioe'
#   l'opposto dello scopo. Rimuovere l'incrementale costa al massimo una
#   ricompilazione piu' lenta del crate toccato, e la cache si ricostruisce da
#   se' al primo build.
#
# USO
#   powershell -File scripts/cache-sweep.ps1 -Report
#   powershell -File scripts/cache-sweep.ps1 -Sweep [-GiorniMax 7]
#
#   Da registrare come attivita' periodica (settimanale): un intervento manuale
#   che diventa abitudine e' la toppa che la regola H vieta.

[CmdletBinding()]
param(
  [switch]$Report,
  [switch]$Sweep,
  [int]$GiorniMax = 7
)

$ErrorActionPreference = 'Stop'
$radice = Split-Path -Parent $PSScriptRoot

function Get-Incrementali {
  # Ogni `target*` della radice, piu' i target dentro eventuali worktree.
  $candidati = @()
  Get-ChildItem -LiteralPath $radice -Directory -Filter 'target*' -ErrorAction SilentlyContinue |
    ForEach-Object { $candidati += $_.FullName }
  $wt = Join-Path $radice '.claude\worktrees'
  if (Test-Path -LiteralPath $wt) {
    Get-ChildItem -LiteralPath $wt -Directory -ErrorAction SilentlyContinue | ForEach-Object {
      Get-ChildItem -LiteralPath $_.FullName -Directory -Filter 'target*' -ErrorAction SilentlyContinue |
        ForEach-Object { $candidati += $_.FullName }
    }
  }

  $esiti = @()
  foreach ($t in $candidati) {
    foreach ($profilo in @('debug', 'release')) {
      $inc = Join-Path $t "$profilo\incremental"
      if (-not (Test-Path -LiteralPath $inc)) { continue }
      $file = Get-ChildItem -LiteralPath $inc -Recurse -File -Force -ErrorAction SilentlyContinue
      # Cargo CREA la directory anche con CARGO_INCREMENTAL=0: il segnale e' il
      # CONTENUTO, non l'esistenza (gotcha misurato il 2026-07-26).
      if (-not $file) { continue }
      $ultimo = ($file | Measure-Object -Property LastWriteTime -Maximum).Maximum
      $esiti += [PSCustomObject]@{
        Percorso  = $inc
        GB        = [math]::Round((($file | Measure-Object -Property Length -Sum).Sum) / 1GB, 2)
        File      = $file.Count
        GiorniFa  = [math]::Round(((Get-Date) - $ultimo).TotalDays, 1)
      }
    }
  }
  return $esiti
}

$trovati = Get-Incrementali

if (-not $trovati) {
  Write-Output 'cache-sweep: nessuna cache incrementale con contenuto.'
  exit 0
}

# La premessa accanto ai numeri (regola O): da dove si sta guardando.
Write-Output "cache-sweep: radice $radice, soglia $GiorniMax giorni"
Write-Output ''
$trovati | Sort-Object GB -Descending | Format-Table -AutoSize |
  Out-String -Width 160 | Write-Output

$totale = [math]::Round(($trovati | Measure-Object -Property GB -Sum).Sum, 2)
$potabili = @($trovati | Where-Object { $_.GiorniFa -ge $GiorniMax })
$recuperabile = if ($potabili) { [math]::Round(($potabili | Measure-Object -Property GB -Sum).Sum, 2) } else { 0 }

Write-Output "totale incrementale: $totale GB — potabile (fermo da >= $GiorniMax giorni): $recuperabile GB"

if (-not $Sweep) {
  if (-not $Report) {
    Write-Output ''
    Write-Output 'Nessuna azione: usa -Report per il solo censimento o -Sweep per potare.'
  }
  exit 0
}

if (-not $potabili) {
  Write-Output 'Niente da potare: nessuna cache oltre la soglia.'
  exit 0
}

Write-Output ''
foreach ($p in $potabili) {
  # Il retry non e' superstizione: su alberi con decine di migliaia di file
  # Remove-Item puo' fallire con "la directory non e' vuota" e riuscire al giro
  # dopo (misurato il 2026-07-26 su 3107 file residui).
  $fatto = $false
  foreach ($tentativo in 1..3) {
    try { Remove-Item -LiteralPath $p.Percorso -Recurse -Force -ErrorAction Stop } catch { }
    if (-not (Test-Path -LiteralPath $p.Percorso)) { $fatto = $true; break }
  }
  if ($fatto) {
    Write-Output ("  potato {0,6} GB  ({1} giorni)  {2}" -f $p.GB, $p.GiorniFa, $p.Percorso)
  } else {
    Write-Output ("  RESIDUO {0,6} GB  {1}" -f $p.GB, $p.Percorso)
  }
}
Write-Output ''
Write-Output "cache-sweep: recuperati fino a $recuperabile GB. La cache si ricostruisce al primo build."
