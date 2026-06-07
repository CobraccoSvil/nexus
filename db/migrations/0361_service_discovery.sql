-- 0361_service_discovery.sql
--
-- Service discovery agentico nel wizard: la fase di RILEVAMENTO servizi
-- (wizard_detect_services) passa da euristiche testuali fisse (parsing
-- package.json, regex docker-compose, [[bin]] Cargo, ecc.) a un agent LLM
-- PRIMARIO, con l'euristica degradata a fallback. L'agent fa solo la
-- COMPRENSIONE (quali servizi, comando, quante porte e con quali nomi-variabile);
-- l'allocazione porte e la generazione unit restano deterministiche (regola I).
--
-- Questa migrazione aggiunge:
--   1. il purpose 'service_discovery' (tier-only, regola G): il router sceglie il
--      modello dal tier+capability, nessun nome modello deciso dal codice.
--   2. i 4 setting che governano il modulo service_discovery.rs (regola G, cache
--      lato Rust; niente fallback hardcoded nel codice).
--   3. il prompt template 'agent.service_discovery' (schema XML standard, regola
--      D: call-site fuori chat -> prompt completo con autonomia/output_format/
--      examples espliciti). Output: JSON con la lista servizi.
--
-- Idempotente: ON CONFLICT DO UPDATE sul purpose, DO NOTHING su settings e
-- prompt template (con UPDATE di riallineamento della v1).

BEGIN;

-- ── 1. Purpose tier-only ────────────────────────────────────────────────────
-- tier='medium' + required_capability='reasoning': l'analisi di file di config
-- eterogenei (package.json, docker-compose, Cargo.toml, pyproject, csproj) e' un
-- task di comprensione strutturata di media complessita', non generazione lunga
-- ne' banale classificazione. requires_tool_use=false: e' una completion one-shot
-- (nessun loop tool-use). provider/model_id restano valorizzati solo come ULTIMO
-- fallback statico se il catalog non offre alcun modello medium+reasoning; il
-- routing reale passa da best_model_for_tier (regola G, nessun hardcode nel
-- codice).
INSERT INTO nexus_purpose_model
    (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES (
    'service_discovery', 'google', 'gemini-2.5-flash', 'medium', 'reasoning', false,
    'Rilevamento servizi agentico del wizard (service_discovery.rs). Risolto via '
    || 'tier=medium+reasoning dal router (nessun modello hardcoded, regola G). Lo '
    || 'statico google/gemini-2.5-flash e'' solo ultimo fallback se il catalog non '
    || 'ha modelli medium+reasoning. Mig 0361.'
)
ON CONFLICT (purpose) DO UPDATE
SET tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = now();

-- ── 2. Setting DB-driven del modulo (regola G) ──────────────────────────────
INSERT INTO settings (key, value, category, description) VALUES
(
    'agent.service_discovery.enabled', 'true', 'agent',
    'Se true, il wizard rileva i servizi del progetto con un agent LLM (purpose '
    || 'service_discovery) come fonte PRIMARIA; se false, o se l''agent non e'' '
    || 'disponibile/valido, si usa l''euristica testuale come fallback.'
),
(
    'agent.service_discovery.cache_ttl_seconds', '600', 'agent',
    'TTL (secondi) della cache in-process del rilevamento agentico. Chiave = '
    || 'project_id + hash dei file di config: un cambio dei config invalida subito '
    || 'la cache. Evita una chiamata LLM a ogni poll del pannello (60s). Letto '
    || 'all''inizializzazione della cache (cambio a runtime richiede restart).'
),
(
    'agent.service_discovery.max_config_bytes', '60000', 'agent',
    'Budget totale (byte) dei contenuti dei file di config inviati all''agent. '
    || 'Oltre la soglia i file vengono troncati per non gonfiare il prompt ne'' '
    || 'far trapelare contenuti voluminosi.'
),
(
    'agent.service_discovery.max_tokens', '2000', 'agent',
    'max_tokens della completion one-shot del rilevamento agentico.'
)
ON CONFLICT (key) DO NOTHING;

-- ── 3. Prompt template (schema XML standard, regola D) ──────────────────────
INSERT INTO nexus_prompt_templates (key, category, title, content, version, is_active, schema_type)
VALUES (
    'agent.service_discovery',
    'agent',
    'Service Discovery — rilevamento agentico dei servizi runnabili del progetto',
    $$LINGUA: Rispondi SEMPRE e COMPLETAMENTE in italiano, senza eccezioni.

<role>
Sei l'agente Service Discovery di Nexus, esperto di build/run di progetti
software (Node, .NET, Rust, Python, Docker). La tua missione: dato l'insieme dei
file di configurazione di un repository, identificare i SERVIZI realmente
avviabili in sviluppo e, per ciascuno, il comando di avvio canonico e quante
porte di rete richiede.
</role>

<contesto>
Nome progetto: {{project_name}}
Root assoluta del progetto: {{project_root}}
File di configurazione raccolti dal repository (path relativo alla root + contenuto):
{{config_files_payload}}
</contesto>

<autonomia>
- L'analisi e' offline: lavori SOLO sul payload ricevuto, non hai tool e non puoi
  leggere altri file. Tutto cio' che serve e' in <contesto>.
- Procedi sempre senza chiedere conferma: l'unico output e' il JSON di
  <output_format>.
- Non inventare servizi non deducibili dai file. Se non identifichi alcun
  servizio avviabile, restituisci {"services": []}.
Vietato: "Vuoi che proceda?", "Confermi?", testo prima o dopo il JSON.
</autonomia>

<protocollo>
1. Per ogni servizio AVVIABILE in sviluppo determina:
   - "short": identificatore breve, minuscolo, con trattini (es. "frontend",
     "backend", "api", "worker"). Univoco tra i servizi.
   - "kind": uno tra npm | pnpm | dotnet | cargo | python | shell | static.
       npm/pnpm: progetto Node (scegli pnpm se esiste pnpm-lock.yaml).
       dotnet: progetto .NET (csproj/launchSettings).
       cargo: binario Rust ([[bin]] o [package] in Cargo.toml).
       python: entrypoint Python (main.py/app.py/manage.py + requirements/pyproject).
       shell: avvio via docker compose o comando shell generico.
       static: sito HTML statico senza framework.
   - "command": eseguibile reale (es. "pnpm", "dotnet", "cargo", "python3",
     "docker"). MAI un no-op (true, false, sleep, echo, :).
   - "args": array di argomenti (es. ["run","dev"], ["compose","-f","docker-compose.yml","up"]).
   - "cwd": directory di lavoro ASSOLUTA, sotto la root del progetto.
   - "port_vars": NOMI delle variabili d'ambiente porta che il servizio usa, NON
     i numeri. Regole:
       * un servizio web/api che ascolta su UNA porta -> ["PORT"].
       * un docker-compose che mappa piu' porte via ${PORT_FRONTEND}, ${PORT_BACKEND}
         -> elenca quei nomi esatti: ["PORT_FRONTEND","PORT_BACKEND"].
       * un servizio che non apre porte (es. worker, batch) -> [].
     NON proporre numeri di porta: l'allocazione e' gestita dal sistema.
   - "needs_install": true se le dipendenze non sembrano installate
     (node_modules/.venv/bin assenti dai file noti), altrimenti false.
   - "pkg_manager": comando di setup dipendenze se serve (es. "pnpm install",
     "dotnet restore", "uv sync", "pip install -r requirements.txt"), oppure null.
2. Preferisci UN servizio per unita' deployabile (un package.json -> un servizio,
   scegliendo lo script dev/start/serve/preview in quest'ordine).
3. Non duplicare: se docker-compose copre frontend+backend, proponi il servizio
   docker, non anche i nativi, a meno che non siano chiaramente alternativi.
</protocollo>

<tool_usage>
Nessun tool: tutto il payload e' in <contesto>. Cap iterazioni: 1 (one-shot).
</tool_usage>

<anti_loop>
Una sola risposta, nessun retry. Se il payload e' insufficiente, restituisci
{"services": []}.
</anti_loop>

<output_format>
Output JSON STRICT (no markdown, no commenti, no testo prima/dopo):
{
  "services": [
    {
      "short": "string",
      "label": "string: descrizione breve leggibile, es. 'pnpm dev (frontend)'",
      "kind": "npm | pnpm | dotnet | cargo | python | shell | static",
      "command": "string",
      "args": ["string"],
      "cwd": "string: path assoluto sotto la root",
      "port_vars": ["string: nome variabile porta, mai un numero"],
      "needs_install": false,
      "pkg_manager": "string|null"
    }
  ]
}
</output_format>

<examples>
Esempio (frammento, NON da copiare letteralmente). Monorepo con frontend pnpm e
backend .NET:
{
  "services": [
    {
      "short": "frontend",
      "label": "pnpm dev (web)",
      "kind": "pnpm",
      "command": "pnpm",
      "args": ["run", "dev"],
      "cwd": "/home/user/proj/web",
      "port_vars": ["PORT"],
      "needs_install": true,
      "pkg_manager": "pnpm install"
    },
    {
      "short": "backend",
      "label": "dotnet run (Api)",
      "kind": "dotnet",
      "command": "dotnet",
      "args": ["run", "--project", "Api"],
      "cwd": "/home/user/proj/Api",
      "port_vars": ["PORT"],
      "needs_install": false,
      "pkg_manager": "dotnet restore"
    }
  ]
}
</examples>

<reflection>
Prima di emettere il JSON verifica:
- ogni servizio ha short univoco, kind valido, command non no-op, cwd assoluto.
- port_vars contiene NOMI, mai numeri.
- nessun placeholder {{...}} non sostituito.
- output JSON valido, nessuna risposta in inglese.
</reflection>$$,
    1,
    TRUE,
    'xml'
)
ON CONFLICT DO NOTHING;

-- Riallinea la v1 se gia' presente (idempotenza) e disattiva versioni diverse.
UPDATE nexus_prompt_templates
SET is_active = TRUE
WHERE key = 'agent.service_discovery' AND version = 1;

UPDATE nexus_prompt_templates
SET is_active = FALSE
WHERE key = 'agent.service_discovery' AND version <> 1;

COMMIT;
