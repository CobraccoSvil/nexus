-- 0610 — La batteria misura la catena e il recupero, e il giro diventa rigiocabile
--
-- I profili `agentic_chain` (high) e `agentic_recovery` (heavy) esistono dalla 0599
-- ma il codice per eseguirli non c'era: `build_profile_request` li rifiutava con
-- "kind profilo non implementato" e ogni giro chiudeva inconclusive. Le bande alte
-- non sono mai state misurate da nessuno: i 7 `frontier` e i 24 `heavy` del catalogo
-- vengono TUTTI dall'indice esterno (`tier_source='synced'`), mai da una prova.
--
-- Il codice ora c'e' (mondo finto a handle opachi, taint tracking, loop multi-step,
-- predicati agganciati ai fatti). Questa migrazione accende il contratto lato DB.

-- ── 1. Il giro contestato dev'essere rigiocabile ────────────────────────────
--
-- Ogni token del mondo finto nasce da SHA-256 di (provider, model, profilo,
-- tentativo, SEED). Senza registrare il seme, un fallimento non e' riproducibile e
-- la diagnosi e' cieca: e' la "replayable fault injection" di ToolMisuseBench, e il
-- motivo per cui oggi il codice usa un seme fisso come limite dichiarato.
ALTER TABLE ai_model_probe_evidence
    ADD COLUMN IF NOT EXISTS seed BIGINT;

COMMENT ON COLUMN ai_model_probe_evidence.seed IS
    'Seme dei token opachi del profilo multi-step. Rende l''istanza fresca a ogni '
    'tentativo (non memorizzabile) e insieme rigiocabile bit a bit da questa riga.';

-- ── 2. I payload dei due profili multi-step ─────────────────────────────────
--
-- `max_turns`: l'attempt e' una conversazione, non un turno. Il codice lo legge dal
-- payload con clamp [2,8]: senza tetto, un modello che non si ferma mai prosciuga
-- il giro della batteria.
--
-- `write_file` esce da agentic_chain: e' un tool mutatore e non serve a seguire una
-- catena di riferimenti. `max_tokens` sale a 4096 perche' 2048 su 6 turni misura il
-- nostro budget, non il modello.
UPDATE ai_model_probe_profile
   SET payload = payload || '{"max_turns": 6, "max_tokens": 4096,
                              "tool_names": ["read_file","list_files","search_in_files","run_command"]}'::jsonb
 WHERE profile_key = 'agentic_chain';

UPDATE ai_model_probe_profile
   SET payload = payload || '{"max_turns": 6, "max_tokens": 4096}'::jsonb
 WHERE profile_key = 'agentic_recovery';

-- ── 3. Una banda alta e' una promessa di affidabilita' ──────────────────────
--
-- L'iniezione di errori degrada la COSTANZA, non il picco (Claw-Eval: "Pass@3 nearly
-- flat while Pass^3 drops sharply"): 3 pass su 4 e' precisamente la soglia che non
-- vede il fenomeno. `heavy` chiede 4 su 4. Stesse quattro run, decisione diversa.
UPDATE ai_model_probe_profile
   SET pass_predicate = pass_predicate || '{"promote_min_passes": 4, "hold_min_passes": 3}'::jsonb
 WHERE profile_key = 'agentic_recovery';

-- ── 4. Il needle non certifica il vertice ───────────────────────────────────
--
-- `agentic_longctx` ha prodotto 40 evidenze su 40 inconclusive: non ha MAI dato un
-- verdetto. E anche funzionante misurerebbe la cosa sbagliata — cinque gruppi
-- indipendenti (NoLiMa, RULER, Michelangelo, BABILong, FLenQA) dicono che il
-- needle-in-a-haystack e' una lookup su dizionario, e il nostro caso e' il peggiore:
-- la domanda ("una riga che inizia con CODICE-PRATICA:") e la riga
-- ("CODICE-PRATICA: NX7K2P9QW4") hanno sovrapposizione lessicale massima, che e' la
-- scorciatoia che NoLiMa quantifica (GPT-4o 99,3% -> 69,7% appena la togli).
--
-- Si spegne invece di lasciarlo girare: costa 4 chiamate da 100k caratteri per
-- modello e non ha mai deciso niente. `frontier` torna scoperto — e va bene: il
-- guard del tier fa si' che il silenzio non declassi nessuno, quindi i 7 frontier
-- restano dove sono finche' non arriva il profilo `latent_state` che li misura
-- davvero (stato latente + distrattori).
UPDATE ai_model_probe_profile
   SET enabled = FALSE
 WHERE profile_key = 'agentic_longctx';

-- ── 5. Il bump che fa ripartire la misura ───────────────────────────────────
--
-- `SQL_CLAIM` riclama un modello gia' qualificato solo se
-- `qualification_suite_version < max(suite_version)`. Senza questo bump i 29 modelli
-- a suite 2 non vedrebbero i profili nuovi per 30 giorni (`requalify_ttl_days`), e
-- il motore appena costruito resterebbe senza bersagli: verificato sul vivo il
-- 2026-07-17, candidati eleggibili = 0.
--
-- COSTO: 29 modelli x 4 tentativi x ~6 turni sui profili nuovi. Non e' gratis, ed e'
-- il motivo per cui questo e' l'ultimo passo e non il primo.
UPDATE ai_model_probe_profile
   SET suite_version = 3
 WHERE enabled;
