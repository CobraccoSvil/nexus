-- 0508: catena di verifica per-progetto INFERITA DA LLM (ADR 0036).
--
-- Problema (caso reale Beaty-Book 2026-07-02): il final_gate validava il run
-- con un generico "npm run build" — per un progetto Vite la build NON fa
-- type-check, quindi un run con import/export incoerenti e' stato chiuso
-- "Verifica superata" mentre il frontend era rotto a runtime.
--
-- Decisione (esplicita dell'utente): NESSUNA conoscenza d'ambiente fissa.
-- Ne' matrice linguaggio->comando, ne' lista di manifest riconosciuti, ne'
-- vocabolario di step: e' un LLM che osserva il progetto (sceglie LUI quali
-- file leggere dal listing) e definisce la catena di verifica con step dal
-- nome libero, marcando quali eseguire nel gate di chiusura. Se il profilo
-- non esiste e l'LLM non e' raggiungibile, il gate DICHIARA onestamente
-- "verifica tecnica non eseguita" nell'esito del run: mai comandi generici.
-- Restano fissi SOLO sicurezza e determinismo: la safety dei comandi
-- (check_command), il confinamento delle letture alla root, l'invalidazione
-- deterministica della cache (hash listing+file osservati).
--
-- Precedenza: run_configurations (override utente) > profilo LLM. Punto
-- unico di risoluzione in agent_tools::verify; il final_gate riceve gli step
-- gate=true risolti a monte (regola G).

CREATE TABLE IF NOT EXISTS project_verify_profiles (
    project_id    UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    -- [{"step":"<nome libero>","command":"...","working_dir":null,
    --   "timeout_s":null,"gate":true,"rationale":"..."}] in ordine di esecuzione.
    steps         JSONB NOT NULL DEFAULT '[]'::jsonb,
    -- {"summary":"...","observed_files":[...]}: cosa l'LLM ha osservato (audit
    -- + base dell'hash di invalidazione).
    environment   JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- Hash deterministico di listing + contenuto dei file osservati: se
    -- cambia, il profilo e' stale e viene rigenerato al prossimo uso.
    manifest_hash TEXT  NOT NULL,
    source        TEXT  NOT NULL DEFAULT 'llm' CHECK (source IN ('llm', 'user')),
    provider      TEXT,
    model         TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Purpose per l'inferenza (tier-aware: 'medium'; il tier comanda sul model
-- statico di cortesia).
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, notes)
VALUES (
    'verify_infer',
    'google',
    'gemini-2.5-flash',
    'medium',
    'Inferenza della catena di verifica per-progetto dall''ambiente reale (ADR 0036). Due pass: selezione file + catena. Output JSON strutturato.'
)
ON CONFLICT (purpose) DO NOTHING;

-- Prompt PASS 1: selezione dei file da osservare (canale fuori-chat, regola D).
INSERT INTO nexus_prompt_templates (key, category, title, content, is_active, usage_context)
VALUES (
    'system.verify_infer.select_files',
    'system',
    'Verifica ambiente - selezione file da osservare',
    '<role>Sei un ingegnere DevOps esperto di toolchain. Riceverai il listing (root + primo livello) di un progetto software di cui NON conosci lo stack.</role>
<contesto>Devi determinare la catena di comandi che verifica le modifiche al progetto. In questo primo passaggio scegli QUALI file leggere per capire l''ambiente: manifest, lockfile, configurazioni di build/test, script — qualunque cosa ti serva, qualunque sia lo stack (anche stack che non ti aspetti: guarda i nomi e decidi).</contesto>
<protocollo>Scegli al massimo 15 file, in ordine di utilita''. Solo path RELATIVI presenti nel listing. Preferisci i file che rivelano comandi e strumenti (manifest di pacchetto, config di build, CI, Makefile, script).</protocollo>
<output_format>Rispondi SOLO con JSON valido: {"files_to_read":["path/relativo", "..."]}</output_format>',
    true,
    'Pass 1 di verify_profile::ensure_profile (mcp-core). Il pass 2 e'' system.verify_infer.infer_chain.'
)
ON CONFLICT (key) DO NOTHING;

-- Prompt PASS 2: inferenza della catena (canale fuori-chat, regola D).
INSERT INTO nexus_prompt_templates (key, category, title, content, is_active, usage_context)
VALUES (
    'system.verify_infer.infer_chain',
    'system',
    'Verifica ambiente - inferenza catena di verifica',
    '<role>Sei un ingegnere DevOps esperto di toolchain. Hai chiesto e ottenuto il contenuto dei file di configurazione di un progetto: ora definisci la catena di verifica per QUESTO preciso ambiente.</role>
<contesto>I comandi verranno eseguiti automaticamente nella root del progetto per validare le modifiche di un agente AI. Un comando che "passa" senza controllare davvero produce falsi successi: preferisci sempre i comandi che FALLISCONO rumorosamente sugli errori reali dello stack. Esempio classico: in un progetto Vite+TypeScript la build NON fa type-check — serve uno step separato con `npx tsc --noEmit` PRIMA della build.</contesto>
<protocollo>
1. Identifica gli stack presenti (possono essere piu'' di uno: monorepo).
2. Definisci gli step con NOME LIBERO e descrittivo (es. "typecheck", "schema-validate", "container-build"): quello che serve a questo ambiente, nell''ordine giusto. Non inventare comandi: solo strumenti che i file osservati dimostrano presenti o canonici per lo stack.
3. Marca con "gate": true gli step che vanno eseguiti alla CHIUSURA di ogni run (rapidi e decisivi: tipicamente type-check e build); "gate": false per le verifiche profonde da eseguire su richiesta (suite E2E, test lenti).
4. Solo comandi NON interattivi, senza watch-mode, con exit code significativo, eseguibili offline dalla root (usa working_dir per le sottocartelle di un monorepo). Mai comandi distruttivi o che modificano lo stato (niente install, deploy, migrate, publish, rm).
</protocollo>
<output_format>Rispondi SOLO con JSON valido:
{"environment_summary":"<una riga sullo stack rilevato>","steps":[{"step":"<nome libero>","command":"<comando completo>","working_dir":null,"timeout_s":null,"gate":true,"rationale":"<perche'' questo comando per questo ambiente>"}]}</output_format>
<reflection>Prima di rispondere verifica: ogni comando esiste per lo stack osservato? Fallirebbe con exit code diverso da 0 sugli errori che deve intercettare? Gli step "gate" coprono gli errori che romperebbero il progetto a runtime?</reflection>',
    true,
    'Pass 2 di verify_profile::ensure_profile (mcp-core). Output validato dalla safety comandi e persistito in project_verify_profiles.'
)
ON CONFLICT (key) DO NOTHING;

-- Config del flusso (regola G).
INSERT INTO settings (key, value) VALUES
  ('agent.verify_infer.enabled', 'true'),
  ('agent.verify_infer.timeout_s', '45')
ON CONFLICT (key) DO NOTHING;

-- NESSUN fallback fisso (decisione utente): via la matrice statica
-- linguaggio->comando della 0503 e il build command generico del final_gate
-- (era esattamente il "npm run build" cieco dell'incidente). Le chiavi
-- OPERATIVE del tool (agent.verify.enabled / step_timeout_s /
-- output_max_chars) restano: sono bound di esecuzione, non conoscenza
-- d'ambiente.
DELETE FROM settings
 WHERE key LIKE 'agent.verify.typescript.%'
    OR key LIKE 'agent.verify.rust.%'
    OR key LIKE 'agent.verify.python.%'
    OR key LIKE 'agent.verify.go.%'
    OR key IN ('agent.final_gate.build_command', 'agent.final_gate.build_check_enabled');
