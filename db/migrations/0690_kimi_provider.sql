-- 0690_kimi_provider.sql
-- Onboarding del provider Kimi (Moonshot AI) come provider di PRIMA CLASSE: non
-- solo registrato, ma visibile a tutte le catene che usano la AI e alla gestione
-- dei tier.
--
-- Perche' una migrazione sola e non cinque come per i tre provider precedenti.
-- Groq/OpenRouter/Perplexity (mig 0566/0567/0568) sono stati registrati e poi
-- NON venivano MAI scelti dal routing: la diagnosi del 13/07/2026 ha attribuito
-- la causa dominante al seed INCOMPLETO — capability senza il tag 'reasoning',
-- che sette intent agentici su diciassette pretendono — e sono servite altre
-- quattro migrazioni a posteriori (0570 billing_url, 0575 prefissi, 0576 default
-- model, 0582 reasoning, 0583 pricing_state). Qui quei campi sono nel seed.
--
-- Cosa questa migrazione NON fa, di proposito:
--   - NON scrive `last_probe_healthy_at`: sarebbe fabbricare la prova d'inferenza
--     che il gate della mig 0629 pretende, cioe' disattivare l'unico controllo
--     reale senza lasciare traccia (regola H).
--   - NON scrive `performance_tier` ne' `tier_source`: il tier arriva dai fatti.
--     Vedi la nota "GESTIONE DEI TIER" in fondo.
--   - NON pinna alcuna cella di `nexus_routing_matrix`: ce le mette l'auto-promoter
--     quando i modelli sono qualificati. Il precedente e' misurato e va in senso
--     opposto — openrouter e' entrato in 56 celle da solo (`manual_override`=0) e
--     ha 347 chiamate reali; perplexity e' stato pinnato a mano in 4 celle, le
--     vince tutte con modelli `unqualified` e non ha MAI prodotto una chiamata.
--     Un pin non e' visibilita': e' una tabella che dice il falso.
--   - NON tocca `vision.routable_providers`: la vision di `kimi-k2.7-code` e'
--     dichiarata da due pagine ufficiali in contraddizione fra loro, e finche' non
--     la si legge da `GET /v1/models` non la si afferma.
--
-- Dati da https://platform.kimi.ai/docs (rilevati il 2026-08-09): modelli e
-- finestre da /docs/models, prezzi da /docs/pricing/chat-k3|chat-k27-code|chat-k26,
-- quirk di protocollo da /docs/api/models-overview.
--
-- ATTIVAZIONE, che e' a due tempi e va saputo prima di concludere che sia rotta:
--   1. questa migrazione inserisce i modelli con is_enabled=true; il trigger
--      `ai_price_catalog_enforce_probe_before_enable` (mig 0629) li respinge a
--      false marcandoli `auto_disabled_reason='unverified_no_probe'`;
--   2. quel marchio NON e' un vicolo cieco: e' l'unico ingresso al ciclo di
--      guarigione, perche' `is_reprobe_candidate` lo ammette esplicitamente.
--      Il worker di re-probe esegue chat-probe e tool-probe REALI e, se passano,
--      scrive insieme is_enabled=true e last_probe_healthy_at.
--   Inserirli con is_enabled=false li lascerebbe invece in uno stato TERMINALE
--   (reason NULL: il probe principale carica solo gli abilitati, il re-probe
--   pretende un reason). E' il limbo in cui stanno 47 righe, fra cui le 7 righe
--   kimi arrivate dal discovery di OpenRouter.
--   Prerequisito: `kimi_api_key` valorizzata dall'admin. Senza chiave il gateway
--   non costruisce il provider e nessun probe puo' partire.

-- 1) Settings. category='providers' e' LOAD-BEARING su due fronti: la dashboard
--    conta le chiavi con `category='providers' AND key LIKE '%_api_key'`, e il
--    gate ratchet di audit-settings (baseline 0/0/0) classificherebbe
--    `kimi_enabled` come chiave fantasma sotto qualunque altra categoria.
--    Il prefisso deve coincidere col nome del provider: `escalation_port` cerca
--    letteralmente `format!("{}_api_key", provider)`.
INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('kimi_api_key', '', 'providers',
     'API key Kimi / Moonshot AI. Vuota = provider inattivo. Le chiavi del lato internazionale (platform.kimi.ai) e di quello cinese (platform.kimi.com) NON sono interscambiabili: una chiave del lato sbagliato da'' 401.', true),
    ('kimi_enabled', 'true', 'providers',
     'Abilita il provider Kimi. Attivo solo se kimi_api_key e'' presente.', false)
ON CONFLICT (key) DO NOTHING;

-- 2) Registry provider (mig 0565). `api_format='openai_compat'` MA con adapter
--    dedicato (`crates/nexus-gateway/src/providers/kimi.rs`): in
--    `construct_provider` il ramo per NOME precede il controllo sul formato, ed
--    e' esattamente il caso di mistral. L'adapter serve per tre quirk documentati
--    che una riga di registry non puo' esprimere: la `temperature` e' un valore
--    FISSO del modello e "passing any other value returns an error" (tre call
--    site interni ne mandano una a ogni chiamata), `max_tokens` e' deprecato in
--    favore di `max_completion_tokens`, e il Preserved Thinking pretende che
--    l'assistant torni indietro col proprio `reasoning_content`.
--    `max_context_tokens` e' la finestra del modello piu' capiente (kimi-k3, 1M).
INSERT INTO nexus_provider_registry
    (name, api_format, key_setting, enabled_setting, base_url_setting, base_url_default,
     activation, tiers, max_context_tokens, supports_tools, sort_order, billing_url,
     litellm_prefixes, litellm_sync_inserts)
VALUES
    ('kimi', 'openai_compat', 'kimi_api_key', 'kimi_enabled', 'kimi_base_url',
     'https://api.moonshot.ai/v1', 'api_key', '{0,1,2}', 1048576, true, 100,
     'https://platform.kimi.ai/console/api-keys',
     -- Il prefisso LiteLLM di Moonshot non e' stato verificato sul dump: NULL
     -- dichiara "non lo so", un prefisso indovinato sincronizzerebbe i prezzi
     -- del modello sbagliato. `litellm_sync_inserts=false` come i tre precedenti.
     NULL, false)
ON CONFLICT (name) DO NOTHING;

-- 3) Policy di selezione. NON e' cosmetica: senza questa riga il JOIN di
--    `reconcile_enable_returning_to_policy` non matcha e NESSUNA riga kimi verra'
--    mai riabilitata dal ciclo. E' la differenza misurata fra openrouter (policy
--    presente, 17 modelli abilitati) e perplexity (nessuna policy, zero).
--    Il punto nei nomi (`kimi-k2.7-code`) e' escapato: in una regex POSIX
--    matcherebbe qualunque carattere.
--    I `denied_patterns` escludono cio' che non e' un modello di chat piu' la
--    serie legacy `moonshot-v1`, chiusa ai nuovi utenti e spenta il 31/08/2026.
INSERT INTO nexus_model_selection_policy (provider, allowed_patterns, denied_patterns)
VALUES
    ('kimi',
     ARRAY['^kimi-k3$', '^kimi-k2\.7-code(-highspeed)?$', '^kimi-k2\.6$'],
     ARRAY['^moonshot-v1', 'vision-preview', 'embedding', 'whisper', '-tts', ':batch$'])
ON CONFLICT (provider) DO NOTHING;

-- 4) Catalogo modelli.
--
--    PREZZI: Moonshot pubblica DUE tariffe di input, "Cache Hit" e "Cache Miss",
--    in valore assoluto. La mappatura e': input_cost = MISS (il prezzo pieno),
--    cache_read_cost = HIT. Non si deriva per proporzione come fece la mig 0403:
--    qui il numero e' di listino. `cache_creation_cost` resta NULL perche' non
--    esiste una tariffa di scrittura o storage — la cache e' automatica, senza
--    creazione ne' TTL da gestire — e comunque nessuna risposta Moonshot riporta
--    token di creazione, quindi quella colonna non verrebbe mai moltiplicata.
--
--    currency='USD' DEVE coincidere con `billing_base_currency`: resolve_active_price
--    filtra su currency e non ha ripiego (mig 0294).
--
--    capabilities INCLUDE 'reasoning' per l'intera serie kimi-k*, e non e' una
--    concessione ottimistica: su questi modelli il pensiero e' sempre acceso e
--    non disattivabile ("Always reasons" su k3, "thinking is on by default" su
--    k2.7-code). Senza quel tag il modello e' escluso dai sette intent agentici
--    che filtrano su `capabilities @> ["reasoning"]`.
--
--    capability_source='auto', NON 'manual'. Il lock manuale sembra una
--    protezione ed e' un veleno: `RECONCILE_MANUAL_LOCKED_SQL` esclude quelle
--    righe dalla reconciliation, ed e' il motivo per cui i 4 modelli Groq seedati
--    'manual' sono rimasti immobili dal 13/07 al 31/07/2026. Il seed e' gia'
--    protetto senza il lock: le capability si scrivono solo quando sono vuote.
--
--    agentic_thinking_policy='none': NON 'disable_for_tools', che prometterebbe
--    di spegnere un pensiero che su k3/k2.7-code non si spegne; NON 'exclude',
--    che escluderebbe il modello dal routing agentico.
--
--    NON seedati: `kimi-k2.5` e la serie `moonshot-v1-*` (chiusi ai nuovi utenti,
--    spegnimento della piattaforma il 31/08/2026), e le varianti `:batch` (la
--    Batch API ha un contratto di chiamata diverso, non modellato qui).
INSERT INTO ai_price_catalog
    (provider, model, display_name,
     input_cost_per_million_tokens, output_cost_per_million_tokens,
     cache_read_cost_per_million_tokens, cache_creation_cost_per_million_tokens,
     currency, is_enabled, context_window, speed_tier, capabilities,
     supports_tool_use, capability_source, agentic_thinking_policy)
VALUES
    ('kimi', 'kimi-k3', 'Kimi K3',
     3.00, 15.00, 0.30, NULL, 'USD', true, 1048576, 'medium',
     '["chat","code","reasoning","long-context"]'::jsonb, true, 'auto', 'none'),
    ('kimi', 'kimi-k2.6', 'Kimi K2.6',
     0.95, 4.00, 0.16, NULL, 'USD', true, 262144, 'medium',
     '["chat","code","reasoning"]'::jsonb, true, 'auto', 'none'),
    ('kimi', 'kimi-k2.7-code', 'Kimi K2.7 Code',
     0.95, 4.00, 0.19, NULL, 'USD', true, 262144, 'medium',
     '["chat","code","reasoning"]'::jsonb, true, 'auto', 'none'),
    -- Stesso modello del precedente a velocita' doppia e prezzo doppio: e'
    -- l'unico del parco per cui la doc dichiara un numero (~180 tok/s, fino a
    -- 260 su contesti brevi), quindi e' l'unico che nasce 'fast'.
    ('kimi', 'kimi-k2.7-code-highspeed', 'Kimi K2.7 Code HighSpeed',
     1.90, 8.00, 0.38, NULL, 'USD', true, 262144, 'fast',
     '["chat","code","reasoning"]'::jsonb, true, 'auto', 'none')
ON CONFLICT (provider, model) DO NOTHING;

-- 5) Modello di default del provider. E' il passo piu' facile da dimenticare e il
--    piu' velenoso: senza, `default_model_for_provider` ritorna la sentinella
--    `unknown-provider-kimi`, il provider risponde 404 e kimi appare DOWN pur
--    avendo una chiave valida. E' il difetto che la mig 0576 ha dovuto correggere
--    per tutti e tre i predecessori.
--    Scelto il piu' economico del parco: l'health probe gira in continuo.
INSERT INTO nexus_provider_default_model (provider, model_id, notes)
VALUES ('kimi', 'kimi-k2.6',
        'Default per health probe e routing statico: il piu'' economico dei modelli censiti.')
ON CONFLICT (provider) DO NOTHING;

-- 6) Meccaniche di chiamata per modello (`nexus_provider_capabilities`, mig 0240).
--    Il forcing dei tool NON puo' essere un default per-provider come per gli
--    altri OpenAI-compat: la doc dichiara `tool_choice: "required"` supportato
--    dal SOLO `kimi-k3` e "not supported" su k2.6 e k2.7-code. Un default
--    provider-wide 'openai_required' manderebbe `required` a tre modelli su
--    quattro che lo rifiutano; l'assenza di riga vale 'openai_auto', cioe' il
--    ripiego sicuro. Per questo il dato sta qui, per modello, e non nel match di
--    `capability.rs::default_style_for_provider` (regola G: la fonte e' il DB).
--
--    `supports_prompt_cache`/`prompt_cache_dialect` NON sono valorizzate: sono
--    colonne fossili, nessun file di codice le legge, e il CHECK ammette per il
--    dialetto solo NULL o 'anthropic_ephemeral'. La cache di Kimi e' dichiarata
--    dove il codice la legge davvero (PromptCacheKeying nell'adapter).
INSERT INTO nexus_provider_capabilities
    (provider, model, max_context_tokens, default_max_output_tokens, max_output_tokens_hard,
     tool_choice_style, request_timeout_seconds)
VALUES
    ('kimi', 'kimi-k3',                    1048576, 8192, 131072, 'openai_required', 120),
    ('kimi', 'kimi-k2.6',                   262144, 8192,  32768, 'openai_auto',     120),
    ('kimi', 'kimi-k2.7-code',              262144, 8192,  32768, 'openai_auto',     120),
    ('kimi', 'kimi-k2.7-code-highspeed',    262144, 8192,  32768, 'openai_auto',     120)
ON CONFLICT (provider, model) DO NOTHING;

-- 7) Catene di ultima spiaggia. Sono sette CSV letti da `chat_learning` quando
--    non c'e' una catena di progetto: elencano PROVIDER, non modelli, quindi
--    aggiungervi kimi non sostituisce alcun giudizio — a differenza di un pin in
--    routing_matrix. Kimi va in coda: e' il posto di chi non ha ancora storia.
UPDATE settings
   SET value = value || ',kimi', updated_at = NOW()
 WHERE key IN ('provider_hierarchy', 'routing_chat_providers', 'routing_fix_providers',
               'routing_test_providers', 'routing_docs_providers',
               'routing_refactor_providers', 'routing_architecture_providers')
   AND value NOT LIKE '%kimi%';

-- GESTIONE DEI TIER: perche' qui non c'e' nessun `performance_tier`.
--
-- Il tier non e' un'opinione da scrivere nel seed: e' una misura, e per i modelli
-- Kimi il sistema l'ha GIA' fatta. Le stesse macchine sono a catalogo da tempo
-- come rivendute da OpenRouter, e portano l'indice agentico gia' sincronizzato:
-- `moonshotai/kimi-k3` 54.3 (frontier), `moonshotai/kimi-k2.6` 31.2 (high),
-- `moonshotai/kimi-k2.7-code` 30.3 (high). `normalize_model_key` toglie il
-- prefisso del rivenditore e i separatori, quindi `kimi-k2.6` e
-- `moonshotai/kimi-k2.6` collassano sulla STESSA chiave: al primo giro di
-- `sync_agentic_index` queste righe ereditano indice e fascia con
-- `tier_source='synced'`, e piu' avanti la batteria le riscrive con `'measured'`.
--
-- Scriverlo a mano qui avrebbe l'effetto opposto a quello voluto: `apply_tier`
-- fa valere `manual` SOPRA `measured` e `synced`, quindi il valore congelerebbe
-- la fascia contro ogni misura futura. Nel catalogo vivo non esiste oggi
-- nemmeno una riga `manual`, e l'ultima volta che ne sono comparse e' servita la
-- mig 0608 per ripulirle.
