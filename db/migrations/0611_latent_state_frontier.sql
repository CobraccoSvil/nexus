-- 0611 — Il vertice si misura seguendo uno stato, non cercando un ago
--
-- La 0610 ha spento `agentic_longctx` e ha lasciato `frontier` scoperto, dichiarando
-- perche': un needle-in-a-haystack e' una lookup su dizionario, e il nostro era il
-- caso peggiore — la domanda ("una riga che inizia con CODICE-PRATICA:") e la riga
-- ("CODICE-PRATICA: NX7K2P9QW4") avevano sovrapposizione lessicale massima, cioe' la
-- scorciatoia che NoLiMa quantifica (GPT-4o dal 99,3% al 69,7% appena la togli). In
-- 40 evidenze su 40 non ha mai dato un verdetto.
--
-- Questa migrazione accende il sostituto: `latent_state`. Il registro racconta 13
-- aggiornamenti a uno stato (fascicoli che entrano e escono dal carico) e la domanda
-- chiede lo stato FINALE. Quella risposta — l'INSIEME dei tre superstiti — non e'
-- scritta in nessuna riga: va COSTRUITA applicando gli aggiornamenti in ordine. E' la
-- forma "Latent List"/"MRCR" di Michelangelo, e nega per costruzione la lookup.
--
-- Cinque degli otto codici sono distrattori: ERANO in carico e non lo sono piu'. Chi
-- guarda invece di seguire li trova tutti e ne riporta di stale — e QUALE stale
-- riporta dice a che punto si e' perso (`stale_closure:2/5`), che e' una diagnosi e
-- non un voto. La sovrapposizione lessicale qui e' una TRAPPOLA: la domanda dice "in
-- carico", e "in carico" compare sia in "risulta in carico" sia in "non risulta piu'
-- in carico". E' l'inverso esatto del needle.
--
-- Ground truth per costruzione (back-instruct, TaskBench): l'harness genera gli
-- aggiornamenti, quindi conosce lo stato finale. Il confronto e' un'uguaglianza di
-- insiemi su token da ~47 bit, mai il parere di un giudice — BFCL, tau-bench e
-- LiveBench escludono i giudici LLM dallo scoring.

-- ── 1. Il kind nuovo deve poter esistere ────────────────────────────────────
--
-- Il CHECK elenca i kind ammessi: senza estenderlo, l'INSERT qui sotto viene
-- rifiutato dal vincolo. E' un guard voluto (un refuso dell'admin non deve poter
-- creare un profilo che nessuno sa eseguire), quindi si allarga esplicitamente.
ALTER TABLE ai_model_probe_profile
    DROP CONSTRAINT IF EXISTS ai_model_probe_profile_kind_check;

ALTER TABLE ai_model_probe_profile
    ADD CONSTRAINT ai_model_probe_profile_kind_check
    CHECK (kind = ANY (ARRAY['chat', 'tool_minimal', 'tool_realistic', 'thinking_matrix',
                             'tool_chain', 'tool_recovery', 'long_context', 'latent_state']));

-- ── 2. Il profilo ───────────────────────────────────────────────────────────
--
-- `context_chars` 40000 (~10k token): non e' "quanto e' grande la finestra
-- dichiarata" — quello e' un numero del fornitore. E' la zona in cui FLenQA misura
-- che il ragionamento degrada gia' (anche a 3k token, coi distrattori), e sta dentro
-- una finestra da 16k: cosi' il profilo misura la capacita' e non l'overflow.
-- `agentic_longctx` chiedeva 100k e chiudeva inconclusivo — 28 evidenze su 40 sono
-- `transient`, cioe' la misura moriva prima di misurare.
--
-- `repeat` 4 con `promote_min_passes` 4: `frontier` e' il vertice e non puo' chiedere
-- meno di `heavy`, che dalla 0610 chiede 4 su 4 (Claw-Eval: l'affidabilita' si vede in
-- Pass^k, non in Pass@k). `hold_min_passes` 3 e' l'isteresi: conservare costa meno che
-- conquistare, o il routing oscillerebbe a ogni riqualifica.
--
-- `max_tokens` 4096: un modello che ragiona sui 13 aggiornamenti ha bisogno di spazio,
-- e un turno tagliato dal NOSTRO cap ora chiude inconclusivo invece di bocciare (il
-- segnale `length` -> `max_tokens` arriva davvero al turno solo da questo giro: prima
-- il produttore lo appiattiva su `end_turn` e il controllo era codice morto).
--
-- Niente `tool_names`: il compito e' leggere, non agire. Niente
-- `system_template_key`: il system prompt di produzione parla di progetti e tool e
-- non c'entra col registro; il profilo usa il suo, neutro, che non anticipa la
-- domanda.
INSERT INTO ai_model_probe_profile
    (profile_key, suite_version, ord, kind, is_blocking, applies_when, grants,
     payload, pass_predicate, enabled, certifies_tier)
VALUES (
    'agentic_latent_state',
    4,
    80,
    'latent_state',
    -- NON bloccante: un modello che non regge il vertice non e' un modello rotto.
    -- Bloccarlo lo butterebbe fuori dal routing per non essere `frontier`.
    FALSE,
    NULL,
    '[]'::jsonb,
    '{"repeat": 4, "timeout_s": 120, "max_tokens": 4096, "context_chars": 40000}'::jsonb,
    '{"requires_final_state": true, "promote_min_passes": 4, "hold_min_passes": 3,
      "max_latency_ms": 120000}'::jsonb,
    TRUE,
    'frontier'
)
ON CONFLICT (profile_key) DO UPDATE SET
    suite_version  = EXCLUDED.suite_version,
    ord            = EXCLUDED.ord,
    kind           = EXCLUDED.kind,
    payload        = EXCLUDED.payload,
    pass_predicate = EXCLUDED.pass_predicate,
    enabled        = EXCLUDED.enabled,
    certifies_tier = EXCLUDED.certifies_tier;

-- ── 3. Il bump che fa vedere il profilo nuovo ───────────────────────────────
--
-- `SQL_CLAIM` riclama un modello gia' qualificato solo se
-- `qualification_suite_version < max(suite_version)`. Il claim NON ha granularita' per
-- profilo: o il modello rifa la batteria intera, o non vede niente. Senza il bump gli
-- 11 modelli gia' portati a suite 3 dalla 0610 non incontrerebbero `latent_state` per
-- 30 giorni (`requalify_ttl_days`), e `frontier` resterebbe scoperto proprio sui
-- modelli piu' probabili.
--
-- COSTO, dichiarato con la sua premessa: al 2026-07-17, misurato sul DB vivo, le
-- righe eleggibili (`is_enabled AND supports_tool_use AND qualification_suite_version
-- < 4`) sono 100, di cui 11 rifaranno anche cio' che avevano appena fatto a suite 3.
-- Non partono insieme: il claim le prende a scaglioni di `max_models_per_round`.
-- `latent_state` da solo aggiunge 4 chiamate da ~10k token di input per modello
-- (~4M token di input sul parco intero). E' il prezzo di misurare il vertice invece
-- di ereditarlo dall'indice esterno: oggi i 7 `frontier` del catalogo vengono TUTTI da
-- `tier_source='synced'`, cioe' nessuno se l'e' guadagnato.
UPDATE ai_model_probe_profile
   SET suite_version = 4
 WHERE enabled;

-- `agentic_longctx` resta spento e a suite 2: non si riaccende. Se lo si riaccendesse
-- certificherebbe `frontier` insieme a questo profilo, e il vertice tornerebbe
-- ottenibile con una lookup.
