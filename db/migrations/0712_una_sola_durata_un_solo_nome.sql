-- 0712 — L'esclusione di un fornitore senza credito ha UNA durata e UN nome.
--
-- ============================================================================
-- A. LA DURATA: si rimuove `gateway.cooldown.billing_seconds`
-- ============================================================================
--
-- IL DIFETTO (misurato il 13/08/2026 sul sistema vivo). Lo stesso evento aveva
-- due durate in due processi, nello stesso istante e per lo stesso fornitore:
--
--   gateway:  cooldown provider="openai" reason="billing" duration_seconds=3600
--   mcp-core: Provider 'openai' in COOLDOWN LUNGO (21600s, 6 ore)
--   DB:       billing_cooldown_until = 2026-08-14 00:32:17+00
--
-- PERCHE' SOPRAVVIVE IL TETTO DI SEI ORE, e non l'ora del gateway. Non sono due
-- opinioni sullo stesso numero: sono un TETTO e un'ATTESA CIECA. In mcp-core il
-- numero e' un tetto, perche' il `billing_cooldown_recovery_loop` riprova con
-- una completion vera — che il credito lo esercita — e libera al primo successo:
-- sbagliare per eccesso costa al massimo un intervallo di re-probe. Nel gateway
-- non c'e' nulla che possa accorciarlo con cognizione: il suo `healthcheck()` e'
-- un `GET /models`, che risponde 200 mentre le completion sono rifiutate per
-- credito (regola O: lo strumento non raggiunge il suo oggetto). Adottare 3600
-- avrebbe preso il numero prodotto dal processo SENZA verificatore per imporlo a
-- quello che ce l'ha: dopo un'ora un fornitore senza credito sarebbe tornato
-- eleggibile e lo si sarebbe riscoperto con una chiamata a pagamento.
--
-- E i 3600 non sono mai stati la durata di niente. MISURATO su
-- `nexus_provider_health_history` fra le 03:31 e le 04:47 del 14/08/2026: il
-- cooldown billing del gateway e' stato azzerato OTTO volte in 76 minuti,
-- sempre ~600s dopo essere stato messo, dal suo stesso `GET /models` —
-- l'alternanza `healthy=f billing` / `healthy=t` per openai e anthropic ogni
-- ~10 minuti. La vera esclusione del gateway durava l'intervallo di re-probe,
-- non l'ora dichiarata. Allinearla al tetto non allunga percio' nessuna
-- esclusione osservata: toglie il secondo numero che una diagnosi poteva
-- leggere.
--
-- La chiave si RIMUOVE invece di lasciarla inerte: un setting che nessuno legge
-- e' una trappola per chi lo modifica aspettandosi un effetto (regola N punto 4).
-- Da qui in poi entrambi i processi leggono `provider.cooldown_long_s`
-- (mig 0253), nominata dal punto unico
-- `nexus_types::provider_failure::durata::CHIAVE_COOLDOWN_LUNGO`.

DELETE FROM settings WHERE key = 'gateway.cooldown.billing_seconds';

-- ============================================================================
-- B. IL NOME: `nexus_provider_health_history.error_kind` ha un vocabolario solo
-- ============================================================================
--
-- IL DIFETTO. Quella colonna ha due scrittori in due processi e nominavano lo
-- STESSO stato in due modi. MISURATO: openai senza credito produceva due righe
-- nello stesso millisecondo, `2026-08-13 18:32:09.333824 billing` (gateway) e
-- `18:32:09.335162 credit_balance_too_low` (probe di mcp-core); anthropic una
-- sola, `billing`. Sull'intero storico: 4245 righe `billing` (source=gateway)
-- contro 4893 `credit_balance_too_low` (source=probe) — 4275 alla vigilia di
-- questa migrazione, perche' il conteggio cresce finche' il vecchio scrittore e'
-- in produzione. Una query che filtra
-- `error_kind = 'billing'` conta anthropic e perde openai — cioe' la colonna su
-- cui si diagnostica non risponde alla domanda che le si pone, ed e' successo
-- davvero mentre si misurava l'efficacia di un fix.
--
-- PERCHE' VINCE `credit_balance_too_low`:
--   - la colonna vuole una CAUSA, e il suo vocabolario e' dichiarato dalla
--     mig 0097 (`quota_exceeded`, `credit_balance_too_low`, `billing_required`,
--     `rate_limit`, `timeout`, `auth_error`, `connection_error`, `unknown`):
--     `credit_balance_too_low` vi appartiene, `billing` no;
--   - `billing` non e' una causa, e' la CLASSE con cui il gateway decide
--     (`ClasseErrore::Billing`, due valori);
--   - `credit_balance_too_low` e' gia' il valore su cui DECIDONO
--     `provider_health_probe::outcome_from_error_class`,
--     `model_health_probe::is_reprobe_candidate` (che lo rilegge da
--     `ai_price_catalog.disabled_reason`, cioe' da dati persistiti) e
--     `agent_turn_setup::classify_by_error_class`. Coniare un terzo nome
--     canonico avrebbe imposto di riscrivere quei dati per un guadagno
--     estetico.
--
-- Nessun LETTORE decide su questa colonna: `fetch_provider_health_map`
-- (mcp-core/environment.rs) la ripropone tale e quale nei campi `error_kind` /
-- `last_known_error_kind` dei payload di stato provider, che sono display. Il
-- censimento dei lettori e' stato fatto prima di cambiare il valore scritto.
--
-- COSA NON SI PRETENDE DI SISTEMARE. Fra le 4245 righe `billing` ce ne sono 2 di
-- openrouter che erano un 402 di AMMISSIONE (credito residuo misurato a 10,01
-- dollari, non zero): un difetto di classificazione gia' chiuso ALLA FONTE dalla
-- mig 0709 con la causa `request_exceeds_credit`. Qui restano mal classificate
-- come lo erano gia': ri-classificare lo storico dal testo del messaggio sarebbe
-- la regola M al contrario, e non si fa.

UPDATE nexus_provider_health_history
   SET error_kind = 'credit_balance_too_low'
 WHERE error_kind = 'billing';

COMMENT ON COLUMN nexus_provider_health_history.error_kind IS
  'Causa dell''errore quando healthy=false. Vocabolario CANONICO e unico per i due scrittori (probe mcp-core + CooldownManager del gateway): il nome dello stato "fornitore senza credito" e'' credit_balance_too_low, dichiarato in nexus_types::provider_failure::stato_salute. Mai la classe di cooldown (mig 0712).';
