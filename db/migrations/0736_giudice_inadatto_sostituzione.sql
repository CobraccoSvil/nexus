-- 0736: il gate duale sostituisce un giudice INADATTO invece di restarne ostaggio
--
-- IL DIFETTO, MISURATO IL 17/08/2026 IN ESERCIZIO
-- Progetto `app-completa-17-08`, run reale dalla UI. Un sub-run `implement` e'
-- rimasto oltre 400 secondi su UN passo, e non per lentezza: due comandi di sola
-- lettura non sono mai stati eseguiti
--   node -e "require('./backend/package.json')"
--   jq '.dependencies | keys' backend/package.json
--
-- Il meta_step `step_validation` registra la catena per intero:
--   decisione: needs_human | block: retries_exhausted | level: critical
--     gatekeeper  mistral  magistral-small-latest  verdetto=approve   astensione=None
--     challenger  kimi     kimi-k2.6               verdetto=abstained astensione=schema_mismatch
--
-- Il gatekeeper aveva anche scritto la motivazione giusta («non si rilevano
-- rischi di blast radius o di distruzione irreversibile»). Il challenger non ha
-- espresso un parere: non e' riuscito a produrre la tool call del verdetto nella
-- forma STRICT che il gate pretende. Su un passo critico un parere solo non
-- basta -> rimando; l'agente riprova, la selezione ripropone LA STESSA COPPIA di
-- giudici, stesso esito, tetto dei rimandi, `retries_exhausted`. In autonomia
-- non si chiede conferma (regola D): il passo non viene MAI eseguito.
--
-- PERCHE' E' STRUTTURALE E NON UN CASO SFORTUNATO
-- `kimi-k2.6` e' a catalogo `supports_tool_use = true` e `qualified`, e la
-- dichiarazione non e' falsa in generale: quel modello le tool call le fa. Non
-- regge lo SCHEMA del verdetto del gate — un fatto su QUESTO schema, non sul
-- fornitore. Nessuno lo registrava, quindi la selezione lo riproponeva a ogni
-- tentativo. Due astensioni `schema_mismatch` misurate nel solo progetto.
--
-- IL RIMEDIO, DUE META'
--   (A) il posto di un giudice che si astiene per causa STRUTTURALE
--       (`decisions::step_gate::NaturaAstensione`) viene riassegnato UNA VOLTA a
--       un candidato non ancora usato, col veto sull'esecutore intatto. Senza
--       sostituti il gate dichiara di non aver potuto giudicare, come prima:
--       nessuna approvazione per stanchezza, nessun passo critico eseguito
--       senza quorum.
--   (B) la coppia osservata finisce in una memoria di PROCESSO
--       (`mcp-core::giudici_inadatti`, nello stile di `provider_inflight` /
--       `provider_cooldown`) che la selezione dei validatori consulta. NON e' un
--       cooldown e non entra in `esclusioni_selezione`: quel modello resta
--       perfettamente usabile per il lavoro ordinario — non sa fare IL GIUDICE
--       su QUESTO schema. Nessuna tabella nuova: se un domani servisse la serie
--       storica, la sede sarebbe `ai_model_health_history`.
--
-- COSA NON SI E' FATTO, E PERCHE'
--   - NON si allarga `orchestrator.step_reach.observation_commands` per farci
--     passare `node -e` e `jq`: il primo esegue codice arbitrario, il secondo
--     puo' scrivere. Assolverli aprirebbe un buco vero (regola H) — il criterio
--     di portata ha ragione a chiamarli `unconfined`.
--   - NON si abbassa il livello dei `run_command`: la protezione serve.
--   - NON si approva col solo gatekeeper: il quorum su un passo critico e' il
--     presidio, non un fastidio.
--
-- ROLLBACK (a caldo, senza redeploy; la cache dei settings ha TTL 60s)
--   UPDATE settings SET value = 'false'
--    WHERE key = 'orchestrator.step_gate_sostituto_enabled';   -- spegne (A)
--   UPDATE settings SET value = '0'
--    WHERE key = 'orchestrator.step_validator_inadatto_ttl_s'; -- spegne (B)
-- I due meccanismi si spengono SEPARATAMENTE perche' rispondono a due domande
-- diverse: (A) «questo giudizio si puo' ancora formare?», (B) «questa coppia va
-- riproposta al prossimo tentativo?».
--
-- Idempotente: INSERT ... ON CONFLICT DO NOTHING.

INSERT INTO settings (key, value, category, description) VALUES
(
    'orchestrator.step_validator_inadatto_ttl_s', '3600', 'orchestrator',
    'Per quanti secondi vale l''osservazione "questa coppia (fornitore, modello) non produce il verdetto del gate duale nella forma richiesta". Memoria di PROCESSO (mcp-core::giudici_inadatti), consultata dalla selezione dei validatori: non e'' un cooldown del fornitore e non toglie quel modello al lavoro ordinario. Non e'' una condanna — un modello cambia col deploy del fornitore, e scaduto il TTL la coppia torna eleggibile. 0 = registro SPENTO (nessuna annotazione, nessuna esclusione).'
),
(
    'orchestrator.step_gate_sostituto_enabled', 'true', 'orchestrator',
    'Il gate duale riassegna UNA VOLTA il posto di un validatore che si e'' astenuto per causa STRUTTURALE (schema_mismatch), convocando un candidato non ancora usato e diverso dall''esecutore. false = comportamento anteriore al 17/08/2026: l''astensione resta fra i verdetti e su un passo critico il batch viene rimandato fino al tetto dei rimandi. Le astensioni TRANSITORIE (cooldown, timeout, credito, turno vuoto) non producono mai una sostituzione: li'' il problema non e'' il giudice.'
)
ON CONFLICT (key) DO NOTHING;
