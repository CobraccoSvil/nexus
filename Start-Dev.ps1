# Apre WSL e avvia lo stack IDEAI completo.
# Doppio click da Esplora risorse, oppure: pwsh -File Start-Dev.ps1

$wslPath = "/home/administrator/ideai"

Write-Host "Avvio stack IDEAI in WSL..." -ForegroundColor Cyan

# Apre un terminale Windows Terminal (se disponibile) oppure wt, altrimenti cmd
if (Get-Command wt -ErrorAction SilentlyContinue) {
    wt -d . wsl -d Ubuntu -- bash -l -c "cd $wslPath && bash scripts/dev-wsl.sh; exec bash"
} else {
    Start-Process "wsl" -ArgumentList "-d Ubuntu -- bash -l -c `"cd $wslPath && bash scripts/dev-wsl.sh; exec bash`""
}
