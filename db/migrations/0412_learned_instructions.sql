-- 0412_learned_instructions.sql
--
-- Livello 2 della continuita' (l'analogo dell'auto-memory di Claude Code):
-- regole DURATURE di progetto distillate dall'esperienza operativa e SEMPRE
-- iniettate nel system_text, non recuperate per similarita'. Mentre il worklog
-- di sessione (mig 0411) e' la storia operativa volatile, qui vivono le lezioni
-- stabili ("questo progetto usa pnpm, mai npm", "il servizio X si riavvia con
-- systemctl --user", "rispondi in italiano") che evitano di ripetere errori
-- attraverso sessioni e progetti diversi.
--
-- Fonti del distiller (worker Rust learned_instructions.rs):
--   - nexus_session_worklog_events kind IN ('error','failed_attempt') ricorrenti
--     cross-sessione (segnale deterministico di cosa va storto ripetutamente);
--   - wiki_docs kind IN ('chat_note','run_summary') recenti (memoria episodica).
-- Il cursore per progetto (nexus_project_distill_state) garantisce idempotenza:
-- ogni evidenza e' processata una volta, niente loop di spesa LLM.
--
-- Tutto DB-driven (regola G): purpose tier-based + prompt template a caldo +
-- settings con cache. Lifecycle delle regole con review umana (proposed ->
-- active|rejected) e protezione delle regole editate a mano (manually_edited).
-- Idempotente.

BEGIN;

-- 1. Regole durature di progetto. UNIQUE(project_id, content_hash) e' la guardia
--    anti-duplicato: la stessa regola normalizzata non viene mai inserita due
--    volte; il distiller incrementa `occurrences` e aggiorna `last_seen_at`.
CREATE TABLE IF NOT EXISTS nexus_learned_instructions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    category TEXT NOT NULL
        CHECK (category IN ('convention', 'preference', 'environment', 'tooling', 'process')),
    rule_text TEXT NOT NULL,
    rationale TEXT,
    status TEXT NOT NULL DEFAULT 'proposed'
        CHECK (status IN ('proposed', 'active', 'rejected', 'retired')),
    confidence REAL NOT NULL DEFAULT 0.5,
    occurrences INTEGER NOT NULL DEFAULT 1,
    content_hash TEXT NOT NULL,
    source_kind TEXT NOT NULL DEFAULT 'mixed'
        CHECK (source_kind IN ('worklog', 'wiki_docs', 'mixed', 'manual')),
    manually_edited BOOLEAN NOT NULL DEFAULT FALSE,
    created_by TEXT NOT NULL DEFAULT 'distiller',
    updated_by TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_learned_instructions_hash UNIQUE (project_id, content_hash)
);

CREATE INDEX IF NOT EXISTS idx_learned_instructions_project_status
    ON nexus_learned_instructions (project_id, status);

-- 2. Cursore di distillazione per progetto: idempotenza del worker. Registra
--    fin dove (created_at) gli eventi worklog e i wiki_docs sono gia' stati
--    processati, cosi' ogni giro lavora solo sul nuovo.
CREATE TABLE IF NOT EXISTS nexus_project_distill_state (
    project_id UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    last_worklog_cursor TIMESTAMPTZ,
    last_wiki_cursor TIMESTAMPTZ,
    last_run_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 3. Purpose tier-based (regola G): distillazione = lettura + ragionamento,
--    nessun tool, modello leggero. Il routing risolve provider/modello per
--    tier+capability dal PUNTO UNICO resolve_purpose_model (internal_routing.rs).
INSERT INTO nexus_purpose_model
    (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES (
    'learned_instructions_distill',
    'google', 'gemini-2.5-flash',          -- ultimo fallback se il catalog e' vuoto
    'light', 'reasoning', false,
    'learned_instructions: distilla regole durature di progetto (convenzioni, preferenze, ambiente) dall''esperienza operativa. Tier light, reasoning, no tool use.'
)
ON CONFLICT (purpose) DO UPDATE SET
    tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = NOW();

-- 4. Prompt del distiller (fuori-chat -> completo, regola D: schema XML standard
--    mig 0086). Placeholder sostituiti dal worker: {{project_name}},
--    {{current_rules_json}}, {{evidence}}, {{max_active}}.
INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES (
    'agent.learned_instructions_distill',
    'agent',
    'Learned instructions distiller (regole durature di progetto)',
    $PROMPT$<role>
Sei il curatore della memoria di lungo termine di un progetto software in Nexus. La tua missione e' distillare, dall'esperienza operativa accumulata, un insieme COMPATTO di regole durature che aiutino qualunque modello AI a lavorare meglio sul progetto: evitare errori gia' visti, rispettare le convenzioni, non ripetere tentativi falliti.
</role>

<contesto>
Progetto: {{project_name}}
Regole gia' note (JSON, da NON duplicare): {{current_rules_json}}
Numero massimo di regole attive sostenibili: {{max_active}}

Evidenza recente (eventi operativi e note episodiche del progetto):
{{evidence}}
</contesto>

<autonomia>
Operi in background, senza supervisione umana nel turno. Le regole che proponi passano una review (status 'proposed' o 'active'); sii conservativo: meglio poche regole solide che molte deboli.
</autonomia>

<protocollo>
1. Leggi l'evidenza e individua SOLO pattern DURATURI e generali del progetto: convenzioni di codice/stack, preferenze esplicite dell'utente, comandi/ambiente specifici, errori ricorrenti con la loro lezione.
2. IGNORA i fatti episodici e volatili (un singolo file toccato, lo stato di un task, un comando una-tantum): quelli vivono nel worklog di sessione, non qui.
3. Confronta con le regole gia' note: se una nuova evidenza conferma una regola esistente usa "confirm"; se la raffina usa "update"; se la contraddice (evidenza piu' recente prevale) usa "retire" sulla vecchia e "add" sulla nuova. NON duplicare regole gia' note.
4. Assegna category e confidence (0.0-1.0) onesti: alta solo se l'evidenza e' chiara e ripetuta.
</protocollo>

<anti_loop>
Massimo 8 operazioni per chiamata. Niente riformulazioni cosmetiche di regole gia' note (sarebbero scartate dal dedup). Se l'evidenza non contiene nulla di durativo, restituisci operations vuoto.
</anti_loop>

<output_format>
Rispondi ESCLUSIVAMENTE con un singolo oggetto JSON valido, senza testo prima o dopo, senza code fence:
{
  "operations": [
    {"op": "add", "category": "convention|preference|environment|tooling|process", "rule_text": "<regola imperativa concisa, max 200 char>", "rationale": "<perche, max 200 char>", "confidence": 0.0},
    {"op": "update", "id": "<uuid della regola nota>", "rule_text": "<nuovo testo>", "rationale": "<perche>", "confidence": 0.0},
    {"op": "confirm", "id": "<uuid della regola nota>"},
    {"op": "retire", "id": "<uuid della regola nota>", "reason": "<perche obsoleta, max 160 char>"}
  ]
}
</output_format>

<examples>
Evidenza: errori ripetuti "npm: command not found" e note che citano "usa pnpm". -> {"op":"add","category":"tooling","rule_text":"Usa sempre pnpm per le dipendenze di questo progetto, mai npm.","rationale":"npm non e' disponibile; errori ripetuti command not found.","confidence":0.9}
Evidenza: il servizio backend va riavviato e fallisce con kill diretto. -> {"op":"add","category":"environment","rule_text":"Riavvia i servizi con systemctl --user, non con kill diretto del processo.","rationale":"kill diretto lascia la porta occupata; il servizio e' gestito da systemd --user.","confidence":0.85}
Evidenza puramente episodica (un file modificato una volta). -> nessuna operazione (non e' una regola duratura).
</examples>

<reflection>
Prima di rispondere verifica: ogni regola e' DURATURA (non episodica)? Non duplica una regola nota? La confidence riflette davvero la forza dell'evidenza? Le operazioni sono <= 8?
</reflection>$PROMPT$,
    'migration_0412'
)
ON CONFLICT (key) DO NOTHING;

-- 5. Template del blocco iniettato nel system_text (lato brain). Placeholder
--    {{rules}} sostituito dall'elenco puntato delle regole 'active'.
INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES (
    'system.learned_instructions_block',
    'system',
    'Learned instructions block (regole durature iniettate)',
    $PROMPT$<learned_instructions>
Regole durature di questo progetto, apprese dall'esperienza (rispettale salvo indicazione contraria dell'utente nel turno corrente):
{{rules}}
</learned_instructions>$PROMPT$,
    'migration_0412'
)
ON CONFLICT (key) DO NOTHING;

-- 6. Settings DB-driven (regola G, cache lato Rust). Worker + iniezione.
INSERT INTO settings (key, value, category, description) VALUES
    ('agent.learned_instructions.distiller_enabled', 'true', 'agent',
     'Abilita il worker che distilla regole durature di progetto dagli eventi worklog e dai wiki_docs.'),
    ('agent.learned_instructions.distiller_interval_secs', '900', 'agent',
     'Intervallo (secondi) tra i giri del distiller delle learned instructions.'),
    ('agent.learned_instructions.daily_cap', '48', 'agent',
     'Numero massimo di chiamate LLM di distillazione nelle 24h (governo del costo).'),
    ('agent.learned_instructions.min_new_signals', '5', 'agent',
     'Numero minimo di nuovi segnali (eventi worklog error/failed_attempt + wiki_docs) oltre il cursore prima di distillare un progetto.'),
    ('agent.learned_instructions.evidence_max_items', '40', 'agent',
     'Numero massimo di evidenze (eventi + note) passate all''LLM per chiamata.'),
    ('agent.learned_instructions.max_active_per_project', '30', 'agent',
     'Numero massimo di regole in stato active per progetto: oltre, le nuove restano proposed.'),
    ('agent.learned_instructions.auto_activate_confidence', '0.8', 'agent',
     'Soglia di confidence sopra la quale una nuova regola entra direttamente active; sotto resta proposed (review umana).'),
    ('orchestrator.learned_instructions_enabled', 'true', 'orchestrator',
     'Abilita l''iniezione del blocco <learned_instructions> nel system_text dei run (lato brain).'),
    ('orchestrator.learned_instructions_max_chars', '1500', 'orchestrator',
     'Budget massimo di caratteri del blocco <learned_instructions> iniettato (riduzione token).')
ON CONFLICT (key) DO NOTHING;

COMMIT;
