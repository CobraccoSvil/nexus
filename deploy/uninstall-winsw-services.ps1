# Rimuove DEFINITIVAMENTE i 10 servizi Windows (WinSW) applicativi e infra di Nexus,
# per passare all'esecuzione come processi (dev-start.ps1 / dev-build.ps1).
#
# NON tocca i database: postgresql-x64-17, nexus-pg-nexus, nexus-pg-app restano
# servizi Windows (i dati devono persistere).
#
# Reversibile: i manifest .xml e gli eseguibili WinSW restano su disco in
# D:\IDEAI-runtime\winsw\<id>\. Per re-installare un servizio:
#   & D:\IDEAI-runtime\winsw\<id>\<id>.exe install
#
# Richiede privilegi di amministratore: si auto-eleva (prompt UAC).
$ErrorActionPreference = 'Stop'

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)
if (-not $isAdmin) {
  Start-Process powershell.exe -Verb RunAs -Wait -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath)
  return
}

$WINSW = 'D:\IDEAI-runtime\winsw'
# Ordine inverso all'avvio: prima gli applicativi, poi l'infra dati.
$ids = @(
  'nexus-web-ide', 'nexus-chat', 'nexus-plugin', 'nexus-doc', 'nexus-billing',
  'nexus-admin', 'nexus-gateway', 'nexus-mcp-core', 'nexus-garnet', 'nexus-qdrant'
)

foreach ($id in $ids) {
  $svc = Get-Service $id -ErrorAction SilentlyContinue
  if (-not $svc) { Write-Host "${id}: gia' assente" -ForegroundColor DarkGray; continue }
  Write-Host "${id}: stop + rimozione..." -ForegroundColor Yellow
  Stop-Service $id -Force -ErrorAction Continue
  Start-Sleep -Milliseconds 500
  $exe = Join-Path $WINSW "$id\$id.exe"
  if (Test-Path $exe) {
    & $exe uninstall
    if ($LASTEXITCODE -ne 0) { sc.exe delete $id | Out-Null }
  }
  else {
    sc.exe delete $id | Out-Null
  }
  Write-Host "${id}: rimosso" -ForegroundColor Green
}

Write-Host ''
Write-Host 'Servizi residui (attesi: solo i database):' -ForegroundColor Cyan
Get-Service 'nexus-*', 'postgresql-x64-17' -ErrorAction SilentlyContinue | Format-Table Name, Status, StartType -AutoSize
Write-Host 'Ora avvia lo stack come processi con:  .\deploy\dev-start.ps1' -ForegroundColor Cyan
