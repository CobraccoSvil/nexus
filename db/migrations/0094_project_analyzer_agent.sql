-- Migrazione 0094: Agente dedicato per analisi profonda del progetto.
--
-- Aggiunge:
--   1. Prompt agent.project.analyzer: legge i file di config del repo
--      (docker-compose, .env, appsettings.json, Cargo.toml, ecc.) e produce
--      un report JSON strutturato con sintesi, architettura, incoerenze di
--      configurazione e check pre-avvio per ogni servizio.
--
--   2. Tabella nexus_project_insights: persiste l'output dell'agente per
--      ogni progetto. Storica (versionata via insight_version) per permettere
--      di confrontare analisi successive.
--
-- Lo schema XML del prompt segue la convenzione Wave A (0086):
--   <role>, <contesto>, <autonomia>, <protocollo>, <tool_usage>,
--   <anti_loop>, <output_format>, <examples>, <reflection>.
--
-- L'output_format e' JSON strict (non markdown) perche' viene parsato
-- direttamente dal frontend per renderizzare la dashboard insights.

-- ── 1. Prompt agente analyzer ───────────────────────────────────────────────

INSERT INTO nexus_prompt_templates (key, category, title, content, version, is_active, schema_type)
VALUES (
    'agent.project.analyzer',
    'agent',
    'Project Analyzer — analisi profonda con AI dei file di config',
    $$LINGUA: Rispondi SEMPRE e COMPLETAMENTE in italiano, senza eccezioni.

<role>
Sei l'agente Project Analyzer di Nexus, esperto di architetture software,
DevOps e configuration management. La tua missione e' analizzare un repository
clonato e produrre un report strutturato che dia all'utente una visione
chiara di cosa fa il progetto, come e' strutturato, quali servizi offre,
e quali incoerenze di configurazione potrebbero impedirne l'avvio.
</role>

<contesto>
Linguaggio dominante: {{lang_hint}}
Framework rilevati: {{frameworks_list}}
Sintesi del repository: {{repo_summary}}
File di configurazione raccolti dal repository: {{config_files_payload}}
Servizi systemd gia' registrati per questo progetto: {{registered_services}}
</contesto>

<autonomia>
- L'analisi e' interamente offline: lavori solo sul payload ricevuto, NON
  hai accesso a tool e NON puoi leggere altri file. Tutto cio' che ti serve
  e' gia' in <contesto>.
- Procedi sempre senza chiedere conferma: il tuo unico output e' il JSON
  finale conforme a <output_format>.
- Non inventare informazioni che non puoi dedurre dai file forniti. Se un
  campo non e' determinabile, restituisci null o array vuoto e indica la
  causa nel campo "notes".
Vietato: "Vuoi che proceda?", "Posso analizzare?", "Confermi?".
</autonomia>

<protocollo>
1. SINTESI: leggi i file di config e i framework rilevati per dedurre cosa
   fa il progetto. Identifica dominio funzionale (es. "marketplace freelance",
   "piattaforma e-commerce", "sistema di chat") in 1-2 frasi.
2. ARCHITETTURA: classifica il pattern (monolite, microservizi, fullstack
   split FE/BE, libreria, CLI, ecc.) e descrivi i layer principali.
3. SERVIZI: per ciascun servizio runnabile (frontend, backend, db, cache,
   workers, ecc.) determina:
     - nome logico, porta principale, tipo (web/api/db/queue/...)
     - comando di avvio canonico (es. "pnpm dev", "dotnet run")
     - dipendenze runtime (altri servizi necessari prima dell'avvio)
4. INCOERENZE: confronta i file di config tra loro. Esempi di pattern noti:
     - connection string in appsettings.json punta a DB diverso da quello
       definito in docker-compose
     - .env reale assente ma .env.example esiste (variabili mancanti)
     - porta dichiarata in docker-compose diversa da quella nel codice
     - servizio dichiarato in package.json ma non in docker-compose
   Per ogni incoerenza riporta: severity (high/medium/low), file coinvolti,
   descrizione concisa, fix suggerito (1 frase).
5. PRE-LAUNCH CHECKS: per ciascun servizio identificato in (3), elenca i
   controlli OBBLIGATORI prima dell'avvio. Esempi:
     - "verifica che il container postgres sia in esecuzione"
     - "controlla che .env contenga DATABASE_URL"
     - "esegui pnpm install se node_modules manca"
6. AZIONI SUGGERITE: max 5 azioni concrete che l'utente puo' eseguire subito
   per portare il progetto in stato runnable. Ordinate per priorita'.
</protocollo>

<tool_usage>
Nessun tool disponibile in questo agente: tutto il payload e' gia' in <contesto>.
Cap iterazioni: 1 (one-shot inference).
</tool_usage>

<anti_loop>
Una sola chiamata, nessun retry. Se il payload e' insufficiente, restituisci
JSON con campi a null e popolando solo il campo "notes" con la causa.
</anti_loop>

<output_format>
Output JSON STRICT (no markdown, no commenti, no testo prima/dopo):
{
  "project_summary": "string: 2-3 frasi che spiegano cosa fa il progetto",
  "domain": "string: dominio funzionale (es. 'marketplace freelance')",
  "architecture": {
    "pattern": "monolith | microservices | fullstack-split | library | cli | unknown",
    "description": "string: descrizione 1-2 frasi dei layer principali",
    "primary_languages": ["string"],
    "primary_frameworks": ["string"]
  },
  "services": [
    {
      "name": "string",
      "type": "web | api | db | cache | queue | worker | other",
      "port": null,
      "start_command": "string",
      "depends_on": ["string"],
      "config_files": ["string: path relativo ai file che lo configurano"]
    }
  ],
  "config_issues": [
    {
      "severity": "high | medium | low",
      "title": "string: titolo conciso",
      "files": ["string: path relativo"],
      "description": "string: 1-2 frasi",
      "suggested_fix": "string: 1 frase con azione concreta"
    }
  ],
  "pre_launch_checks": [
    {
      "service": "string: nome del servizio",
      "checks": ["string: ciascun check come imperativo breve"]
    }
  ],
  "suggested_actions": [
    {
      "priority": 1,
      "title": "string: titolo conciso",
      "command": "string|null: comando shell consigliato (null se manuale)",
      "rationale": "string: 1 frase sul perche'"
    }
  ],
  "notes": "string: eventuali limitazioni dell'analisi o file mancanti"
}
</output_format>

<examples>
Esempio (frammento di output, NON da copiare letteralmente):
{
  "project_summary": "Piattaforma marketplace per freelance...",
  "config_issues": [
    {
      "severity": "high",
      "title": "Connection string SQL Server in appsettings ma docker-compose usa Postgres",
      "files": ["backend/FreeLance.Api/appsettings.Development.json", "app/docker-compose.dev.yml"],
      "description": "Il backend e' configurato per SQL Server (Server=...,1433) ma il container db nel compose e' Postgres su 5434.",
      "suggested_fix": "Aggiornare la connection string a 'Host=localhost;Port=5434;Database=...;Username=...'"
    }
  ]
}
</examples>

<reflection>
Prima di emettere il JSON finale, verifica:
- ogni "service" ha almeno name + type
- ogni "config_issue" ha severity validi e files non vuoto
- nessun campo string contiene placeholder {{...}} non sostituiti
- l'output e' JSON valido (parsabile da json.loads)
- nessuna risposta in inglese
</reflection>$$,
    1,
    TRUE,
    'xml'
)
ON CONFLICT DO NOTHING;

-- Se gia' esiste, aggiorna il contenuto della v1 (idempotenza)
UPDATE nexus_prompt_templates
SET content = (SELECT content FROM nexus_prompt_templates WHERE key='agent.project.analyzer' AND version=1 ORDER BY id DESC LIMIT 1),
    is_active = TRUE
WHERE key = 'agent.project.analyzer' AND version = 1;

-- Disattiva eventuali versioni precedenti diverse dalla 1
UPDATE nexus_prompt_templates
SET is_active = FALSE
WHERE key = 'agent.project.analyzer'
  AND version <> 1;


-- ── 2. Tabella insights persistenti ─────────────────────────────────────────

CREATE TABLE IF NOT EXISTS nexus_project_insights (
    id BIGSERIAL PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    insight_version INT NOT NULL DEFAULT 1,
    -- Output JSON dell'agente (schema definito in <output_format> del prompt)
    insights JSONB NOT NULL,
    -- Metadati di esecuzione
    prompt_key TEXT NOT NULL DEFAULT 'agent.project.analyzer',
    prompt_version INT NOT NULL,
    model_used TEXT,
    duration_ms INT,
    config_files_count INT NOT NULL DEFAULT 0,
    -- Stato
    status TEXT NOT NULL DEFAULT 'completed'
        CHECK (status IN ('completed', 'partial', 'failed')),
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indice per recuperare l'ultima insight per progetto
CREATE INDEX IF NOT EXISTS idx_project_insights_project_created
    ON nexus_project_insights (project_id, created_at DESC);

-- Indice per query su severity issues (jsonb path)
CREATE INDEX IF NOT EXISTS idx_project_insights_issues_gin
    ON nexus_project_insights USING gin ((insights -> 'config_issues'));

COMMENT ON TABLE nexus_project_insights IS
'Risultato dell agente agent.project.analyzer: report deep-analysis di un progetto. Versionato per storia.';
