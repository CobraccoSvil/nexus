# scripts/integration-full-local.ps1
#
# Esegue in locale la stessa lista di test del job CI `integration-full`, con
# REQUIRE_INTEGRATION_TESTS=1: una precondizione mancante diventa un fallimento
# che la nomina, invece di uno skip.
#
# PERCHE' ESISTE. Il job in cloud richiede minuti Actions, e su un account senza
# piano a pagamento quei minuti finiscono: da inizio luglio 2026 ogni run viene
# respinto prima di partire. Ma l'ambiente che il job allestisce (due cluster
# Postgres, Redis, mcp-core in ascolto) sulla macchina di sviluppo c'e' gia': qui
# la stessa verifica costa zero. Questo script e' l'esecuzione manuale del
# 2026-07-26 resa strumento, cosi' non va rifatta a memoria ogni volta.
#
# COSA NON E'. Non sostituisce il job: quello gira su Linux, con DB usa-e-getta e
# un artefatto compilato da zero. Qui i DB sono QUELLI DI SVILUPPO — lo script
# semina e rimuove le proprie righe, ma non e' un ambiente vergine, e la
# differenza va tenuta presente quando un test passa qui e fallisce la'.
#
#   pwsh -File scripts/integration-full-local.ps1
#   pwsh -File scripts/integration-full-local.ps1 -SaltaRelease   # niente agent_tools_safety

[CmdletBinding()]
param(
    # URL del meta-DB. Default: quello del .env di sviluppo.
    [string]$DatabaseUrl = "postgres://nexus:nexus@localhost:5433/nexus",
    # Cluster app (5434), oggetto di postgres_app_isolation.
    [string]$AppAdminUrl = "postgres://nexus_admin:nexus_admin_secret@localhost:5434/postgres",
    # mcp-core in ascolto. 127.0.0.1 e non localhost: su Windows la risoluzione
    # prova prima ::1 e paga un timeout per richiesta quando il core ascolta IPv4.
    [string]$McpCoreUrl = "http://127.0.0.1:4000",
    # agent_tools_safety pretende il binario release: saltalo se non l'hai compilato.
    [switch]$SaltaRelease
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

$Psql = "C:\Program Files\PostgreSQL\17\bin\psql.exe"
if (-not (Test-Path $Psql)) {
    $cmd = Get-Command psql -ErrorAction SilentlyContinue
    if (-not $cmd) { throw "psql non trovato: serve il client PostgreSQL" }
    $Psql = $cmd.Source
}

# `cargo` puo' non essere nel PATH: rustup lo installa in `~/.cargo/bin`, che
# finisce nel PATH del PROFILO UTENTE. Un servizio Windows non eredita quel
# profilo, quindi lanciando lo script da un runner self-hosted il comando non si
# trova -- misurato il 2026-07-27: il job falliva con "The term 'cargo' is not
# recognized" DOPO aver superato precondizioni e credenziale, il che rende il
# messaggio piu' confuso di quanto la causa meriti.
#
# Lo si cerca dove rustup lo mette, e se non c'e' si dice CHE COSA manca e a chi:
# un "comando non trovato" a meta' script non fa capire che il problema e'
# l'ambiente di chi lo esegue, non il codice.
$Cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $Cargo) {
    $candidati = @(
        (Join-Path $env:CARGO_HOME 'bin\cargo.exe'),
        (Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe')
    ) | Where-Object { $_ -and (Test-Path $_) }
    $Cargo = $candidati | Select-Object -First 1
}
if (-not $Cargo) {
    throw ("cargo non trovato ne' nel PATH ne' in ~/.cargo/bin. " +
           "Se questo script gira da un servizio Windows, quel servizio non eredita " +
           "il profilo dell'utente che ha installato rustup: eseguilo con l'account " +
           "di quell'utente, oppure installa la toolchain a livello di macchina.")
}

function Invoke-Sql([string]$Url, [string]$Sql) {
    $u = [uri]$Url
    $userInfo = $u.UserInfo.Split(':')
    $env:PGPASSWORD = $userInfo[1]
    $out = & $Psql -h $u.Host -p $u.Port -U $userInfo[0] -d $u.AbsolutePath.TrimStart('/') -tAc $Sql 2>&1
    if ($LASTEXITCODE -ne 0) { throw "psql fallito: $out" }
    return ("$out").Trim()
}

# ---------------------------------------------------------------------------
# 1. Precondizioni. Si verificano PRIMA di seminare qualunque cosa: fallire a
#    meta' lascerebbe righe nel DB di sviluppo.
# ---------------------------------------------------------------------------
Write-Host "== precondizioni ==" -ForegroundColor Cyan

$health = try { (Invoke-WebRequest -Uri "$McpCoreUrl/health" -TimeoutSec 5 -UseBasicParsing).StatusCode } catch { 0 }
if ($health -ne 200) {
    throw "mcp-core non risponde su $McpCoreUrl (health=$health). Avvialo prima: e' la precondizione che il job allestisce da se'."
}
Write-Host "  mcp-core in ascolto su $McpCoreUrl"
Write-Host "  meta-DB: $(Invoke-Sql $DatabaseUrl 'SELECT current_database()')"
Write-Host "  cluster app: $(Invoke-Sql $AppAdminUrl 'SELECT current_database()')"

# ---------------------------------------------------------------------------
# 2. Credenziale, per la stessa strada del job: il token lo firma il backend
#    (`/internal/dev-login-token`), la riga in `sessions` la scrive chi fa il
#    login -- qui nessuno, quindi la si apre a mano. Senza, `validate_token`
#    rifiuta con 401 una firma perfettamente valida.
# ---------------------------------------------------------------------------
Write-Host "== credenziale ==" -ForegroundColor Cyan
$token = (Invoke-RestMethod -Uri "$McpCoreUrl/internal/dev-login-token" -Method Post -TimeoutSec 15).token
if (-not $token) { throw "dev-login-token non ha restituito un token (setting auth.dev_login_enabled a false?)" }

$sha = [System.Security.Cryptography.SHA256]::Create()
$tokenHash = ($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($token)) | ForEach-Object { $_.ToString("x2") }) -join ''

# `sub` dal payload: il JWT e' base64url senza padding, che [Convert] rifiuta.
$payload = $token.Split('.')[1].Replace('-', '+').Replace('_', '/')
while ($payload.Length % 4) { $payload += '=' }
$sub = ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($payload)) | ConvertFrom-Json).sub
Write-Host "  utente del token: $sub"

Invoke-Sql $DatabaseUrl "INSERT INTO sessions (user_id, token_hash, expires_at) VALUES ('$sub','$tokenHash', NOW() + INTERVAL '2 hours');" | Out-Null
Write-Host "  sessione aperta (2 ore, rimossa in chiusura)"

# I test del dominio chat/run interrogano un progetto: gli handler chiamano
# `ensure_project_access`, che pretende owner o membro. L'utente del dev-login
# non lo e' di nessuno, quindi gli si da' accesso al primo progetto e glielo si
# toglie alla fine. La riga aggiunta si riconosce: e' l'unica di quell'utente.
$progetto = Invoke-Sql $DatabaseUrl "SELECT id FROM projects ORDER BY created_at LIMIT 1;"
$membershipCreata = $false
if ($progetto) {
    $gia = Invoke-Sql $DatabaseUrl "SELECT count(*) FROM project_members WHERE user_id='$sub' AND project_id='$progetto';"
    if ($gia -eq '0') {
        Invoke-Sql $DatabaseUrl "INSERT INTO project_members (project_id, user_id, role) VALUES ('$progetto','$sub','admin');" | Out-Null
        $membershipCreata = $true
        Write-Host "  accesso temporaneo al progetto $progetto"
    }
} else {
    Write-Host "  nessun progetto nel meta-DB: i test del dominio chat/run lo dichiareranno" -ForegroundColor Yellow
}

# ---------------------------------------------------------------------------
# 3. La stessa lista del job. Un obiettivo per riga: l'elenco E' la
#    dichiarazione di copertura, e un `--workspace` nasconderebbe cosa gira.
# ---------------------------------------------------------------------------
$esitoFinale = 0
try {
    $env:DATABASE_URL = $DatabaseUrl
    $env:NEXUS_APP_ADMIN_URL = $AppAdminUrl
    $env:MCP_CORE_URL = $McpCoreUrl
    $env:NEXUS_TEST_JWT = $token
    $env:REQUIRE_INTEGRATION_TESTS = "1"

    $testMcpCore = @(
        "precondizioni_integrazione",
        "orchestrator_db_schema",
        "project_db_config_contract",
        "postgres_app_isolation",
        "settings_update_contract",
        "agent_runs_endpoints",
        "chat_history_run_anchor",
        "m71_cost_breakdown"
    )
    if (-not $SaltaRelease) { $testMcpCore += "agent_tools_safety" }

    $argomenti = @("test", "-p", "mcp-core")
    foreach ($t in $testMcpCore) { $argomenti += @("--test", $t) }
    $argomenti += @("--", "--nocapture")

    Write-Host "== test di integrazione (ambiente preteso completo) ==" -ForegroundColor Cyan
    & $Cargo @argomenti
    if ($LASTEXITCODE -ne 0) { $esitoFinale = $LASTEXITCODE }

    & $Cargo test -p nexus-auth --test settings_write --test token_firmato_e_accettato -- --nocapture
    if ($LASTEXITCODE -ne 0) { $esitoFinale = $LASTEXITCODE }

    & $Cargo test -p nexus-project-pools --test pool_routing -- --nocapture
    if ($LASTEXITCODE -ne 0) { $esitoFinale = $LASTEXITCODE }
}
finally {
    # Pulizia SEMPRE, anche se i test falliscono o l'utente interrompe: le righe
    # seminate stanno nel DB di sviluppo, non in un container che sparisce.
    Write-Host "== pulizia ==" -ForegroundColor Cyan
    try {
        Invoke-Sql $DatabaseUrl "DELETE FROM sessions WHERE token_hash='$tokenHash';" | Out-Null
        if ($membershipCreata) {
            Invoke-Sql $DatabaseUrl "DELETE FROM project_members WHERE user_id='$sub' AND project_id='$progetto';" | Out-Null
        }
        Write-Host "  sessione e accesso temporaneo rimossi"
    } catch {
        Write-Host "  ATTENZIONE: pulizia fallita ($_). Righe da rimuovere a mano:" -ForegroundColor Red
        Write-Host "    DELETE FROM sessions WHERE token_hash='$tokenHash';" -ForegroundColor Red
        if ($membershipCreata) {
            Write-Host "    DELETE FROM project_members WHERE user_id='$sub' AND project_id='$progetto';" -ForegroundColor Red
        }
    }
    $env:NEXUS_TEST_JWT = $null
    $env:REQUIRE_INTEGRATION_TESTS = $null
}

if ($esitoFinale -ne 0) {
    Write-Host "ESITO: FALLITO (exit $esitoFinale)" -ForegroundColor Red
    exit $esitoFinale
}
Write-Host "ESITO: tutti i test dell'ambiente completo sono passati" -ForegroundColor Green
