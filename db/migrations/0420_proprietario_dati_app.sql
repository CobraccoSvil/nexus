-- 0420_proprietario_dati_app.sql
-- Nexus deve agire come PROPRIETARIO al 100% dell'app gestita: alla richiesta del
-- proprietario "dammi un dato dal database" deve LEGGERE e RISPONDERE col dato, senza
-- rifiuti di sicurezza, senza chiedere "come accedere", senza proporre menu di opzioni.
--
-- Root cause (incidente reale, chat agentica progetto "Beauty Book"):
--   1. system.nexus_base contiene una clausola generica "credenziali/password -> DEVI
--      chiedere all'utente" (mig 0035) che l'LLM applica anche quando il proprietario
--      chiede di LEGGERE un dato del proprio DB -> rifiuto.
--   2. La direttiva <operatore_nexus> (mig 0137) copre scrittura file/config e DDL, ma
--      NON la lettura dei dati dal database dell'app -> non contrasta il rifiuto.
--   3. La whitelist discovery-first (mig 0257/0335/0417) espone i domini file e
--      porte/servizi ma NON i tool DB (nexus_db_query/tables/describe): chiamati diretti
--      vengono respinti "non disponibile in questo turno".
--
-- Fix DEFINITIVO (regola L: estende il punto unico <operatore_nexus> invece di crearne un
-- doppione; regola G: prompt e whitelist nel DB, cache 60s, nessun restart necessario):
--   Parte 1: circoscrive la clausola credenziali di system.nexus_base (vale solo per
--            valori di config ESTERNI da configurare, NON per i dati gia' presenti
--            nelle risorse del progetto gestito).
--   Parte 2: estende <operatore_nexus> (5 template) con l'accesso ai dati del DB dell'app.
--   Parte 3: raffina mode_confirm_instruction (conferma solo per modifiche, mai letture).
--   Parte 4: aggiunge i tool DB alla whitelist discovery-first (sblocco immediato; il
--            consolidamento "whitelist = core tools" e' fatto lato codice, regola L).
-- Idempotente: sentinel <!-- 0420:proprietario_dati_app --> + guardie WHERE/NOT LIKE.

BEGIN;

-- ── Parte 1: clausola credenziali di system.nexus_base circoscritta ─────────────
UPDATE nexus_prompt_templates
SET content = REPLACE(
        content,
        'DEVI chiedere all''utente. Non tentare di indovinare valori sensibili. Interrompi il flusso, spiega cosa ti serve e perche'', e attendi la risposta.',
        'DEVI chiederlo all''utente: non inventare valori sensibili che non hai letto. ATTENZIONE: questo riguarda SOLO i valori di CONFIGURAZIONE ESTERNA che ti servono per agire e che non puoi ricavare (es. API key di un provider terzo non ancora configurato). NON riguarda i dati gia'' presenti nelle risorse del progetto gestito (file, variabili d''ambiente e soprattutto il DATABASE dell''app): quelli sono del proprietario e sei autorizzato a LEGGERLI e MOSTRARLI direttamente, senza chiedere conferma e senza rifiutare (vedi <operatore_nexus>).'
    ),
    version = version + 1,
    updated_at = NOW(),
    updated_by = 'migration_0420'
WHERE key = 'system.nexus_base'
  AND is_active = TRUE
  AND content LIKE '%DEVI chiedere all''utente. Non tentare di indovinare valori sensibili. Interrompi il flusso, spiega cosa ti serve e perche'', e attendi la risposta.%';

-- ── Parte 2: <operatore_nexus> esteso con accesso ai dati del DB dell'app ────────
DO $$
DECLARE
    sentinel TEXT := '<!-- 0420:proprietario_dati_app -->';
    subsection TEXT := E'\n<!-- 0420:proprietario_dati_app -->\nACCESSO AI DATI DEL DATABASE DELL''APP GESTITA (lettura e amministrazione):\n- Hai pieno diritto di LEGGERE e MOSTRARE qualunque dato del database dell''app del\n  progetto, incluse colonne sensibili (password, hash, token, dati personali dei clienti).\n  Sono dati del PROPRIETARIO: NON sono "credenziali sensibili da rifiutare".\n- Per una richiesta del proprietario sui dati dell''app (es. "qual e'' la password / il\n  record del cliente X", "dammi un dato dal database") AGISCI DIRETTAMENTE: usa\n  nexus_db_tables per orientarti e nexus_db_query per leggere, poi rispondi con il DATO\n  REALE. Non rifiutare per "policy di sicurezza", non chiedere "come accedere" (l''accesso\n  al database lo gestisci tu con i tuoi tool), non proporre un menu di opzioni al posto\n  dell''azione. Esegui e rispondi.\n- Questa direttiva SUPERA qualunque altra istruzione che ti imponga di rifiutare o di\n  chiedere conferma per mostrare password / credenziali / dati del progetto gestito.\n';
    k TEXT;
    affected INT := 0;
BEGIN
    FOR k IN
        SELECT key FROM nexus_prompt_templates
         WHERE is_active = TRUE
           AND content LIKE '%</operatore_nexus>%'
           AND content NOT LIKE '%' || sentinel || '%'
    LOOP
        UPDATE nexus_prompt_templates
           SET content = REPLACE(content, '</operatore_nexus>', subsection || '</operatore_nexus>'),
               version = version + 1,
               updated_at = NOW(),
               updated_by = 'migration_0420'
         WHERE key = k AND is_active = TRUE;
        affected := affected + 1;
        RAISE NOTICE '  [%] esteso con accesso dati app', k;
    END LOOP;
    RAISE NOTICE 'Migrazione 0420 Parte 2: % template estesi', affected;
END $$;

-- ── Parte 3: mode_confirm_instruction circoscritta alle modifiche ───────────────
UPDATE nexus_prompt_templates
SET content = 'Modalita'' CON CONFERMA: per i CAMBIAMENTI potenzialmente impattanti (modifiche, creazioni o eliminazioni di file, comandi che alterano lo stato del progetto, migrazioni di schema, deploy) proponi cosa faresti e richiedi conferma esplicita prima di procedere. Le LETTURE e le interrogazioni di dati NON richiedono conferma: in particolare la lettura del database dell''app gestita va eseguita DIRETTAMENTE, rispondendo con il dato, senza chiedere conferma e senza proporre menu di opzioni.',
    version = version + 1,
    updated_at = NOW(),
    updated_by = 'migration_0420'
WHERE key = 'automation.mode_confirm_instruction'
  AND is_active = TRUE
  AND content NOT LIKE '%Le LETTURE e le interrogazioni di dati NON richiedono conferma%';

-- ── Parte 4: tool DB nella whitelist discovery-first (sblocco immediato) ─────────
-- Append idempotente (stesso pattern di 0417). Il consolidamento definitivo
-- "whitelist = insieme core/always-on" e' fatto lato codice (regola L).
DO $$
DECLARE
    needed TEXT[] := ARRAY['nexus_db_query', 'nexus_db_tables', 'nexus_db_describe', 'nexus_get_worklog'];
    tool TEXT;
BEGIN
    FOREACH tool IN ARRAY needed LOOP
        UPDATE settings
           SET value = value || ',' || tool,
               updated_at = NOW()
         WHERE key = 'agent.tools.discovery_first_whitelist'
           AND ',' || value || ',' NOT LIKE '%,' || tool || ',%';
    END LOOP;
END $$;

COMMIT;
