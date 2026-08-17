-- ─────────────────────────────────────────────────────────────────────────────
-- 0730 — anthropic: il breakpoint di prompt cache sulle DEFINIZIONI DEI TOOL
--
-- La gerarchia di cache di Anthropic e' tools -> system -> messages, e il
-- breakpoint che gia' mettiamo sul system (mig precedente, `build_system_field`)
-- copre percio' anche il prefisso dei tool, che gli sta davanti. Questo NON e'
-- quindi un guadagno generale: sono i DUE casi che quel breakpoint non copre.
--
--   1. Le richieste SENZA system. Li' oggi non esiste alcun breakpoint alto, e
--      gli schemi dei tool - la parte piu' voluminosa e piu' stabile del
--      prefisso - si riscrivono integralmente a ogni turno.
--
--   2. I DEPLOY DI PROMPT. Quando la parte stabile del system cambia, il suo
--      breakpoint si sposta e tutto cio' che lo segue si riscrive; con un
--      breakpoint proprio, il prefisso dei tool resta memorizzato attraverso il
--      cambio. Su Anthropic scrivere cache costa 1,25x l'input e rileggerla
--      0,1x, quindi la differenza si vede sul primo turno dopo ogni deploy.
--
-- STA SULL'ULTIMO TOOL, e non sul primo: `cache_control` marca «memorizza fin
-- qui», quindi sul primo memorizzerebbe un solo schema lasciando fuori tutti
-- gli altri. Tetto dei breakpoint: system 1 + history 1 + tools 1 = 3, sotto il
-- limite di 4 che l'API impone.
--
-- IL TTL E' QUELLO BREVE (ephemeral 5m) anche quando il system usa `1h`: le
-- definizioni dei tool cambiano quando cambia il set di tool del run, non
-- quando cambia il prompt, e un TTL di un'ora su un prefisso che si riscrive
-- prima paga la scrittura senza comprare riletture.
--
-- SEED 'true', a differenza della corsia differibile della 0729. La ragione e'
-- la simmetria del rischio: qui non si aggiunge un campo che il fornitore possa
-- rifiutare ne' si cambia la corsia di servizio - si marca un breakpoint in piu'
-- su un prefisso che il modello riceve comunque, e il caso peggiore e' una
-- scrittura di cache che nessuno rilegge. Il flag esiste per il ROLLBACK, che
-- e' l'evento raro, non per l'attivazione.
--
-- Spegnere la cache (`anthropic_system_cache_ttl` = 'off') spegne anche questo:
-- sono due ragioni diverse per la stessa assenza, e la prima sovrasta la
-- seconda.
--
-- VERIFICA al deploy: due chiamate identiche con i tool reali di una figura ->
-- alla seconda `cache_read_input_tokens` deve coprire almeno la dimensione
-- degli schemi. Il numero lo porta il ledger, che quei campi li registra gia'.
--
-- ROLLBACK: `providers.anthropic.cache_tools` = 'false' (TTL 60s, senza
-- riavvio).
--
-- NIENTE MIGRAZIONE per le altre due meta' del lotto: `output_config.effort` e
-- `POST /v1/count_tokens` non hanno configurazione. Il primo e' un campo del
-- CONTRATTO (`LlmRequest.effort`) con vocabolario chiuso validato nel codice -
-- un default DB lo renderebbe una politica nostra travestita da capability del
-- fornitore, e senza un produttore che lo valorizzi resterebbe comunque inerte.
-- Il secondo e' una capability del provider, dichiarata dal trait.
-- ─────────────────────────────────────────────────────────────────────────────

INSERT INTO settings (key, value, category, description) VALUES
(
    'providers.anthropic.cache_tools', 'true', 'providers',
    'Breakpoint di prompt cache sull''ULTIMO blocco tool della richiesta anthropic. La gerarchia e'' tools -> system -> messages, quindi il breakpoint sul system copre gia'' il prefisso dei tool: questo serve alle richieste SENZA system (dove nessun breakpoint alto esiste) e ai deploy di prompt (dove la parte stabile del system cambia e senza un breakpoint proprio i tool si riscrivono con lei). TTL sempre ephemeral 5m, anche col system a 1h. Spento da anthropic_system_cache_ttl=''off''. Cache 60s lato driver.'
)
ON CONFLICT (key) DO NOTHING;
