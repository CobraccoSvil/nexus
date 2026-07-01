# Ferma lo stack Nexus avviato da dev-start.ps1 (processi, non servizi Windows).
# Legge i PID salvati e termina ogni processo con il suo albero (/T): i figli
# eventualmente spawnati da mcp-core (dev-server di progetto) non restano orfani a
# tenere le porte. NON tocca i database (servizi Windows separati).
$ErrorActionPreference = 'Stop'
$RUNTIME = 'D:\IDEAI-runtime'
$PIDFILE = Join-Path $RUNTIME 'nexus-dev.pids.json'

if (Test-Path $PIDFILE) {
  $procs = Get-Content $PIDFILE -Raw | ConvertFrom-Json
  foreach ($p in @($procs)) {
    if (Get-Process -Id $p.pid -ErrorAction SilentlyContinue) {
      # taskkill via cmd /c: in PS 5.1 il redirect di stderr di un exe nativo genera
      # un NativeCommandError; dentro cmd lo stderr non risale a PowerShell.
      cmd /c "taskkill /PID $($p.pid) /T /F >nul 2>nul"
      Write-Host ("fermato {0,-16} pid {1}" -f $p.id, $p.pid) -ForegroundColor Yellow
    }
    else {
      Write-Host ("{0,-16} pid {1} gia' assente" -f $p.id, $p.pid) -ForegroundColor DarkGray
    }
  }
  Remove-Item $PIDFILE -Force
  Write-Host 'Stack fermato.' -ForegroundColor Cyan
}
else {
  Write-Warning "Nessun $PIDFILE. Fallback: termino i processi noti per nome eseguibile."
  # web-ide (node.exe) volutamente ESCLUSO dal fallback: killare per nome ucciderebbe
  # anche altri processi Node non correlati. Fermalo con precisione tramite il pidfile.
  $names = 'mcp-core', 'nexus-gateway', 'admin-service', 'billing-service',
  'doc-service', 'plugin-service', 'chat-service', 'qdrant', 'GarnetServer'
  foreach ($n in $names) {
    Get-Process -Name $n -ErrorAction SilentlyContinue | ForEach-Object {
      cmd /c "taskkill /PID $($_.Id) /T /F >nul 2>nul"
      Write-Host "fermato $n (pid $($_.Id))" -ForegroundColor Yellow
    }
  }
  Write-Warning 'web-ide (node.exe) non incluso nel fallback per nome (rischio di uccidere altri Node).'
}
