<#
.SYNOPSIS
  Ripristino di un database Nexus da un dump prodotto da deploy/db-backup.ps1.

.DESCRIPTION
  Controparte nativa di db-backup.ps1 (ambiente Windows). Ripristina un singolo
  dump custom (-Fc) su un database di un cluster Nexus.

  OPERAZIONE DISTRUTTIVA: sovrascrive gli oggetti del database target. Ferma i
  servizi che usano il DB prima di procedere (deploy/dev-stop.ps1). Senza -Force
  lo script esegue solo un dry-run (mostra il piano senza toccare nulla).

.PARAMETER DumpFile
  Percorso del file .dump da ripristinare.

.PARAMETER Cluster
  Cluster target: 'meta' (:5433) o 'app' (:5434). Determina host/porta/utente.

.PARAMETER Database
  Nome del database target. Se omesso viene dedotto dal nome del file
  (<cluster>-<porta>-<db>.dump).

.PARAMETER Recreate
  DROP + CREATE del database prima del restore (ripristino pulito). Senza questo
  flag usa pg_restore --clean --if-exists sugli oggetti del DB esistente.

.PARAMETER Force
  Esegue davvero il restore. Senza -Force viene solo mostrato il piano.

.NOTES
  Credenziali: default dev, override via NEXUS_META_PGPASSWORD / NEXUS_APP_PGPASSWORD.

.EXAMPLE
  .\deploy\db-restore.ps1 -DumpFile 'D:\Backups\Nexus\20260712-201845\meta-5433-nexus.dump' -Cluster meta
  .\deploy\db-restore.ps1 -DumpFile '...\app-5434-beaty_book_nexus.dump' -Cluster app -Recreate -Force
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][string]$DumpFile,
  [Parameter(Mandatory = $true)][ValidateSet('meta', 'app')][string]$Cluster,
  [string]$Database,
  [switch]$Recreate,
  [switch]$Force
)

$ErrorActionPreference = 'Stop'

function Get-PgBin {
  $root = 'C:\Program Files\PostgreSQL'
  if (-not (Test-Path $root)) { throw "PostgreSQL non trovato in $root" }
  $ver = Get-ChildItem $root -Directory |
    Where-Object { $_.Name -match '^\d+$' } |
    Sort-Object { [int]$_.Name } -Descending | Select-Object -First 1
  if (-not $ver) { throw "Nessuna versione PostgreSQL sotto $root" }
  return (Join-Path $ver.FullName 'bin')
}

$pgbin = Get-PgBin
$PgRestore = Join-Path $pgbin 'pg_restore.exe'
$Psql = Join-Path $pgbin 'psql.exe'

if (-not (Test-Path $DumpFile)) { throw "Dump non trovato: $DumpFile" }

# Parametri del cluster
$cfg = switch ($Cluster) {
  'meta' { @{ PgHost = '127.0.0.1'; Port = 5433; User = 'nexus'; PwEnv = 'NEXUS_META_PGPASSWORD'; PwDefault = 'nexus' } }
  'app' { @{ PgHost = '127.0.0.1'; Port = 5434; User = 'nexus_admin'; PwEnv = 'NEXUS_APP_PGPASSWORD'; PwDefault = 'nexus_admin_secret' } }
}
$pw = [Environment]::GetEnvironmentVariable($cfg.PwEnv); if (-not $pw) { $pw = $cfg.PwDefault }

# Deduci il database dal nome file se non passato: <cluster>-<porta>-<db>.dump
if (-not $Database) {
  $base = [IO.Path]::GetFileNameWithoutExtension($DumpFile)
  if ($base -match '^[a-z]+-\d+-(.+)$') { $Database = $Matches[1] }
  else { throw "Impossibile dedurre il database dal nome '$base': passa -Database." }
}

# Validazione del dump (deve elencare oggetti)
$objs = (& $PgRestore -l $DumpFile 2>$null | Where-Object { $_ -and ($_ -notmatch '^;') } | Measure-Object).Count
if ($objs -lt 1) { throw "Il dump non e' un archivio custom valido (0 oggetti): $DumpFile" }

$action = if ($Recreate) { 'DROP + CREATE poi restore' } else { 'restore con --clean --if-exists' }
Write-Host "Piano di ripristino:"
Write-Host "  dump      : $DumpFile ($objs oggetti)"
Write-Host "  cluster   : $Cluster (:$($cfg.Port), utente $($cfg.User))"
Write-Host "  database  : $Database"
Write-Host "  azione    : $action"

if (-not $Force) {
  Write-Host "`nDry-run (nessuna modifica). Rilancia con -Force per eseguire il restore."
  Write-Host "ATTENZIONE: e' un'operazione distruttiva; ferma prima i servizi (deploy\dev-stop.ps1)."
  exit 0
}

$env:PGPASSWORD = $pw

if ($Recreate) {
  Write-Host "`n-> DROP + CREATE '$Database'"
  $sql = @"
SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname='$Database' AND pid <> pg_backend_pid();
DROP DATABASE IF EXISTS "$Database";
CREATE DATABASE "$Database" OWNER "$($cfg.User)";
"@
  $sql | & $Psql -h $cfg.PgHost -p $cfg.Port -U $cfg.User -d postgres -v ON_ERROR_STOP=1 -q
  if ($LASTEXITCODE -ne 0) { $env:PGPASSWORD = 'x'; throw "DROP/CREATE fallito." }
}

Write-Host "-> pg_restore su '$Database'"
$restoreArgs = @('-h', $cfg.PgHost, '-p', $cfg.Port, '-U', $cfg.User, '-d', $Database, '--no-owner', '--no-privileges')
if (-not $Recreate) { $restoreArgs += @('--clean', '--if-exists') }
$restoreArgs += $DumpFile
& $PgRestore @restoreArgs
$code = $LASTEXITCODE
$env:PGPASSWORD = 'x'

if ($code -ne 0) {
  Write-Host "`nRestore terminato con codice $code (pg_restore segnala warning con exit!=0 anche su restore riusciti parziali; verifica i messaggi sopra)."
  exit $code
}
Write-Host "`nRestore completato. Riavvia i servizi (deploy\dev-start.ps1)."
exit 0
