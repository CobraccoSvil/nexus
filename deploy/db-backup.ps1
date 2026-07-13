<#
.SYNOPSIS
  Backup dei cluster PostgreSQL di Nexus (ambiente Windows nativo).

.DESCRIPTION
  Punto unico di backup del database (regola L). Sostituisce i tre script bash
  legacy tarati sull'ambiente WSL + Docker (container ideai-postgres-nexus-1):
  deploy/backup-db-to-d.sh, scripts/backup-db.sh, scripts/db-backup.sh.

  Backuppa ENTRAMBI i cluster della separazione DB per-progetto:
    - meta :5433  -> DB 'nexus' (config, prompt, settings, routing, ...)
    - app  :5434  -> DB per-progetto <slug>_nexus (chat/run) e <slug>_app
  I database non vengono elencati a mano: si enumerano a runtime (esclusi i
  template e 'postgres'), cosi' un nuovo progetto entra nel backup senza
  toccare lo script.

  Per ciascun cluster produce, in una sottocartella <BackupDir>\<timestamp>\:
    - <cluster>-globals.sql   (ruoli/permessi, pg_dumpall --globals-only)
    - <cluster>-<db>.dump     (pg_dump -Fc --no-owner --no-acl per ogni DB)
  Ogni .dump viene validato con pg_restore -l. Retention per-set (cartelle).
  Copia off-site opzionale su Google Drive via rclone (opt-in, -Gdrive).

.PARAMETER BackupDir
  Directory radice dei backup (default D:\Backups\Nexus). Ogni esecuzione crea
  una sottocartella con timestamp.

.PARAMETER KeepLast
  Numero di set (sottocartelle) da conservare. I piu' vecchi vengono rimossi.
  Default 14. 0 = nessuna rotation.

.PARAMETER SyncOnly
  Salta il dump: esegue solo rotation (ed eventuale off-site) sui set esistenti.

.PARAMETER Gdrive
  Abilita la copia off-site via rclone (default: solo disco locale). Se rclone
  non e' installato o il remote non e' configurato lo step viene saltato con
  avviso, senza far fallire il backup locale.

.PARAMETER RcloneRemote
  Nome del remote rclone (default 'gdrive').

.PARAMETER GdrivePath
  Percorso di destinazione sul remote (default 'Nexus/Backups').

.NOTES
  Credenziali: default dev noti, override via variabili d'ambiente
  NEXUS_META_PGPASSWORD (:5433) e NEXUS_APP_PGPASSWORD (:5434) per la produzione.
  Le password sono passate a pg_dump via PGPASSWORD (env di processo), mai in
  argomenti visibili.

.EXAMPLE
  .\deploy\db-backup.ps1
  .\deploy\db-backup.ps1 -KeepLast 30 -Gdrive
  .\deploy\db-backup.ps1 -SyncOnly
#>
[CmdletBinding()]
param(
  [string]$BackupDir = 'D:\Backups\Nexus',
  [int]$KeepLast = 14,
  [switch]$SyncOnly,
  [switch]$Gdrive,
  [string]$RcloneRemote = 'gdrive',
  [string]$GdrivePath = 'Nexus/Backups'
)

$ErrorActionPreference = 'Stop'

# ── Individua i binari PostgreSQL (versione piu' recente installata) ──────────
function Get-PgBin {
  $root = 'C:\Program Files\PostgreSQL'
  if (-not (Test-Path $root)) { throw "PostgreSQL non trovato in $root" }
  $ver = Get-ChildItem $root -Directory |
    Where-Object { $_.Name -match '^\d+$' } |
    Sort-Object { [int]$_.Name } -Descending |
    Select-Object -First 1
  if (-not $ver) { throw "Nessuna versione PostgreSQL sotto $root" }
  $bin = Join-Path $ver.FullName 'bin'
  foreach ($exe in 'pg_dump.exe', 'pg_dumpall.exe', 'pg_restore.exe', 'psql.exe') {
    if (-not (Test-Path (Join-Path $bin $exe))) { throw "$exe mancante in $bin" }
  }
  return $bin
}

$pgbin = Get-PgBin
$PgDump = Join-Path $pgbin 'pg_dump.exe'
$PgDumpall = Join-Path $pgbin 'pg_dumpall.exe'
$PgRestore = Join-Path $pgbin 'pg_restore.exe'
$Psql = Join-Path $pgbin 'psql.exe'

# ── Cluster da backuppare (separazione DB per-progetto) ──────────────────────
# La password si prende dall'env indicato in PwEnv se valorizzato, altrimenti
# dal default dev. In produzione: impostare le due variabili d'ambiente.
$clusters = @(
  [pscustomobject]@{
    Label = 'meta-5433'; PgHost = '127.0.0.1'; Port = 5433; User = 'nexus'
    PwEnv = 'NEXUS_META_PGPASSWORD'; PwDefault = 'nexus'
  },
  [pscustomobject]@{
    Label = 'app-5434'; PgHost = '127.0.0.1'; Port = 5434; User = 'nexus_admin'
    PwEnv = 'NEXUS_APP_PGPASSWORD'; PwDefault = 'nexus_admin_secret'
  }
)

function Resolve-Pw($cluster) {
  $fromEnv = [Environment]::GetEnvironmentVariable($cluster.PwEnv)
  if ($fromEnv) { return $fromEnv }
  return $cluster.PwDefault
}

# Elenca i DB reali di un cluster (esclusi template e 'postgres').
function Get-Databases($cluster) {
  $env:PGPASSWORD = Resolve-Pw $cluster
  $sql = "SELECT datname FROM pg_database WHERE datistemplate=false AND datname <> 'postgres' ORDER BY datname"
  $out = & $Psql -h $cluster.PgHost -p $cluster.Port -U $cluster.User -d postgres -tAqc $sql
  if ($LASTEXITCODE -ne 0) { throw "psql fallito su $($cluster.Label) (:$($cluster.Port))" }
  return @($out | ForEach-Object { $_.Trim() } | Where-Object { $_ })
}

$failures = @()

# ── Step 1: dump ─────────────────────────────────────────────────────────────
if (-not $SyncOnly) {
  $ts = Get-Date -Format 'yyyyMMdd-HHmmss'
  $setDir = Join-Path $BackupDir $ts
  New-Item -ItemType Directory -Force -Path $setDir | Out-Null
  Write-Host "Set di backup: $setDir`n"

  foreach ($c in $clusters) {
    Write-Host "== cluster $($c.Label) =="
    $env:PGPASSWORD = Resolve-Pw $c

    # Globals (ruoli/permessi)
    $globalsFile = Join-Path $setDir "$($c.Label)-globals.sql"
    & $PgDumpall -h $c.PgHost -p $c.Port -U $c.User --globals-only -f $globalsFile
    if ($LASTEXITCODE -ne 0) {
      Write-Host "  globals: FALLITO"; $failures += "$($c.Label)/globals"
    } else {
      Write-Host ("  globals: {0:N1} KB" -f ((Get-Item $globalsFile).Length / 1KB))
    }

    # Un dump per database
    $dbs = Get-Databases $c
    if ($dbs.Count -eq 0) { Write-Host "  (nessun database utente)"; continue }
    foreach ($db in $dbs) {
      $outFile = Join-Path $setDir "$($c.Label)-$db.dump"
      $env:PGPASSWORD = Resolve-Pw $c
      & $PgDump -h $c.PgHost -p $c.Port -U $c.User -d $db -Fc --no-owner --no-acl -f $outFile
      if ($LASTEXITCODE -ne 0) {
        Write-Host "  $db : DUMP FALLITO"; $failures += "$($c.Label)/$db"; continue
      }
      # Verifica integrita': pg_restore -l deve elencare oggetti
      $objs = (& $PgRestore -l $outFile 2>$null | Where-Object { $_ -and ($_ -notmatch '^;') } | Measure-Object).Count
      $mb = [math]::Round((Get-Item $outFile).Length / 1MB, 2)
      if ($objs -lt 1) {
        Write-Host "  $db : dump SOSPETTO (0 oggetti nel manifest), rimuovo"
        Remove-Item $outFile -Force
        $failures += "$($c.Label)/$db (manifest vuoto)"
      } else {
        Write-Host "  $db : OK  $mb MB, $objs oggetti"
      }
    }
    Write-Host ''
  }
}

# ── Step 2: rotation (tieni gli ultimi KeepLast set) ─────────────────────────
if ($KeepLast -gt 0 -and (Test-Path $BackupDir)) {
  $sets = Get-ChildItem $BackupDir -Directory |
    Where-Object { $_.Name -match '^\d{8}-\d{6}$' } |
    Sort-Object Name -Descending
  if ($sets.Count -gt $KeepLast) {
    $old = $sets | Select-Object -Skip $KeepLast
    Write-Host "== rotation: rimuovo $($old.Count) set oltre gli ultimi $KeepLast =="
    foreach ($o in $old) {
      Remove-Item $o.FullName -Recurse -Force
      Write-Host "  rimosso: $($o.Name)"
    }
    Write-Host ''
  }
}

# ── Step 3: off-site opzionale (rclone) ──────────────────────────────────────
if ($Gdrive) {
  $rcloneCmd = Get-Command rclone -ErrorAction SilentlyContinue
  $rclone = if ($rcloneCmd) { $rcloneCmd.Source } else { $null }
  if (-not $rclone) {
    Write-Host "== off-site SALTATO: rclone non installato (configura con 'rclone config', tipo drive) =="
  } else {
    $remotes = & $rclone listremotes 2>$null
    if ($remotes -notcontains "${RcloneRemote}:") {
      Write-Host "== off-site SALTATO: remote '${RcloneRemote}:' non configurato =="
    } else {
      $dest = "${RcloneRemote}:$GdrivePath"
      Write-Host "== off-site: rclone copy $BackupDir -> $dest =="
      & $rclone copy $BackupDir $dest --transfers 4
      if ($LASTEXITCODE -eq 0) { Write-Host "  upload OK" }
      else { Write-Host "  upload FALLITO (exit $LASTEXITCODE)"; $failures += 'rclone-upload' }
    }
  }
  Write-Host ''
}

# ── Riepilogo ────────────────────────────────────────────────────────────────
$env:PGPASSWORD = 'x'
Write-Host "== stato $BackupDir =="
Get-ChildItem $BackupDir -Directory -ErrorAction SilentlyContinue |
  Sort-Object Name -Descending | Select-Object -First 5 |
  ForEach-Object {
    $sz = [math]::Round(((Get-ChildItem $_.FullName -File | Measure-Object Length -Sum).Sum) / 1MB, 1)
    Write-Host ("  {0}  {1} MB" -f $_.Name, $sz)
  }

if ($failures.Count -gt 0) {
  Write-Host "`nBACKUP INCOMPLETO — fallimenti: $($failures -join ', ')"
  exit 1
}
Write-Host "`nBackup completato senza errori."
exit 0
